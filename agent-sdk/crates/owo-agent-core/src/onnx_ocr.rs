//! 本地 ONNX OCR（M-E）：`ort` + ch_PP-OCRv4 det/rec ONNX 模型，全本地推理，无网可用。
//!
//! 模型目录：`<data>/models/ocr/`（`OWO_ONNX_OCR_MODEL_DIR` 可覆盖）：
//!
//! - `ch_PP-OCRv4_det_infer.onnx`（文本检测）
//! - `ch_PP-OCRv4_rec_infer.onnx`（文本识别）
//! - `ppocr_keys_v1.txt`（CTC 字典）
//!
//! 下载脚本：`scripts/download-onnx-ocr-models.ps1`。
//!
//! 算法参考 RapidOCR v1.1.0（ch_ppocr_v3_det/rec 模块）与 PaddleOCR DB 后处理：
//! - det 预处理：limit_side_len=736（limit_type=min）+ 32 对齐 + 1/255 + 按通道
//!   mean/std（0.485/0.456/0.406、0.229/0.224/0.225，BGR 通道序，与 Paddle/RapidOCR
//!   训练-推理管线一致）；后处理：二值化(0.3) → 2x2 膨胀 → 连通域 → 最小外接矩形
//!   → 框内概率均值(≥0.5) → unclip(1.6) → NMS(0.3)。
//! - rec 预处理：高 48、宽按长宽比（上限 320）、(x/255-0.5)/0.5；CTC 解码
//!   （重复折叠 + 空白位 0 剔除 + 字典映射）。
//!
//! 本模块是全本地确定性 OCR 通道：不产生网络请求、不受数据出境开关影响。

use crate::ocr::{OcrBox, OcrSummary};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub const DET_LIMIT_SIDE: f32 = 736.0;
pub const DET_BINARY_THRESH: f32 = 0.3;
pub const BOX_THRESH: f32 = 0.5;
pub const UNCLIP_RATIO: f32 = 1.6;
pub const NMS_THRESH: f32 = 0.3;
pub const REC_HEIGHT: usize = 48;
pub const REC_MAX_WIDTH: usize = 320;
pub const REC_TEXT_SCORE: f32 = 0.5;

const DET_MEANS: [f32; 3] = [0.485, 0.456, 0.406];
const DET_STDS: [f32; 3] = [0.229, 0.224, 0.225];

/// 32bpp BGRA 内存图像（字节序 B,G,R,A）。
pub(crate) struct BgraImage {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl BgraImage {
    fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let x = x.floor().max(0.0) as usize;
        let y = y.floor().max(0.0) as usize;
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let i = (y * self.width + x) * 4;
        [
            self.pixels[i] as f32,
            self.pixels[i + 1] as f32,
            self.pixels[i + 2] as f32,
        ]
    }
}

/// 解析 32bpp BMP → BGRA 内存图像。
pub(crate) fn parse_bmp32(bmp: &[u8]) -> Result<BgraImage, String> {
    if bmp.len() < 54 || &bmp[..2] != b"BM" {
        return Err("BMP 头无效".to_string());
    }
    let width = i32::from_le_bytes([bmp[18], bmp[19], bmp[20], bmp[21]]);
    let height_raw = i32::from_le_bytes([bmp[22], bmp[23], bmp[24], bmp[25]]);
    let height = height_raw.abs();
    let bit_count = u16::from_le_bytes([bmp[28], bmp[29]]);
    if bit_count != 32 {
        return Err(format!("不支持的 BMP 位深：{bit_count}"));
    }
    if width <= 0 || height <= 0 {
        return Err("BMP 尺寸非法".to_string());
    }
    let expected = (width as usize) * (height as usize) * 4;
    if bmp.len() < 54 + expected {
        return Err("BMP 像素数据不完整".to_string());
    }
    let pixels = bmp[54..54 + expected].to_vec();
    Ok(BgraImage {
        width: width as usize,
        height: height as usize,
        pixels,
    })
}

/// det 预处理输出：CHW 归一化张量 + 模型输入分辨率。
pub(crate) struct DetInput {
    tensor: Array4<f32>,
    map_w: usize,
    map_h: usize,
}

/// det 预处理：limit_side_len=736（min 侧限制）+ 32 对齐 + BGR 通道归一化。
pub(crate) fn det_preprocess(img: &BgraImage) -> DetInput {
    let h = img.height as f32;
    let w = img.width as f32;
    let ratio = if h.min(w) < DET_LIMIT_SIDE {
        DET_LIMIT_SIDE / h.min(w)
    } else {
        1.0
    };
    let mut map_h = ((h * ratio) / 32.0).round() as usize * 32;
    let mut map_w = ((w * ratio) / 32.0).round() as usize * 32;
    map_h = map_h.max(32);
    map_w = map_w.max(32);
    // 双线性缩放 + 通道归一化（BGR 序，mean/std 按通道索引机械对齐 RapidOCR 实现）。
    let mut tensor = vec![0f32; 3 * map_h * map_w];
    let scale_x = w / map_w as f32;
    let scale_y = h / map_h as f32;
    for y in 0..map_h {
        let src_y = (y as f32 + 0.5) * scale_y - 0.5;
        for x in 0..map_w {
            let src_x = (x as f32 + 0.5) * scale_x - 0.5;
            let [b, g, r] = bilinear(img, src_x, src_y, scale_x, scale_y);
            let c0 = (b / 255.0 - DET_MEANS[0]) / DET_STDS[0];
            let c1 = (g / 255.0 - DET_MEANS[1]) / DET_STDS[1];
            let c2 = (r / 255.0 - DET_MEANS[2]) / DET_STDS[2];
            let base = y * map_w + x;
            tensor[base] = c0;
            tensor[map_h * map_w + base] = c1;
            tensor[2 * map_h * map_w + base] = c2;
        }
    }
    let tensor =
        Array4::from_shape_vec((1, 3, map_h, map_w), tensor).expect("det 张量形状与数据长度一致");
    DetInput {
        tensor,
        map_w,
        map_h,
    }
}

/// 双线性采样（OpenCV INTER_LINEAR 语义，align_corners=false）。
/// 调用方传入的采样坐标 `(x, y)` 已按 `(dst+0.5)*scale-0.5` 计算。
fn bilinear(img: &BgraImage, x: f32, y: f32, _scale_x: f32, _scale_y: f32) -> [f32; 3] {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = (x - x0).clamp(0.0, 1.0);
    let fy = (y - y0).clamp(0.0, 1.0);
    let p00 = img.sample(x0, y0);
    let p10 = img.sample(x0 + 1.0, y0);
    let p01 = img.sample(x0, y0 + 1.0);
    let p11 = img.sample(x0 + 1.0, y0 + 1.0);
    let mut out = [0f32; 3];
    for c in 0..3 {
        let top = p00[c] * (1.0 - fx) + p10[c] * fx;
        let bottom = p01[c] * (1.0 - fx) + p11[c] * fx;
        out[c] = top * (1.0 - fy) + bottom * fy;
    }
    out
}

/// 检测框：4 点（tl, tr, br, bl，顺时针）+ 框分数。
#[derive(Debug, Clone)]
pub struct DetQuad {
    pub pts: [[f32; 2]; 4],
    pub score: f32,
}

impl DetQuad {
    /// 轴对齐包围盒 `(x, y, width, height)`。
    pub fn bbox(&self) -> (f32, f32, f32, f32) {
        let min_x = self.pts.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
        let max_x = self
            .pts
            .iter()
            .map(|p| p[0])
            .fold(f32::NEG_INFINITY, f32::max);
        let min_y = self.pts.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let max_y = self
            .pts
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        (min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

/// DB 后处理：二值化 → 2x2 膨胀 → 连通域 → 最小外接矩形 → 分数 → unclip → 坐标还原 → NMS。
/// `prob` 为模型输出的概率图（行主序，宽 `map_w` 高 `map_h`），结果还原到原图尺寸 `(orig_w, orig_h)`。
pub fn db_postprocess(
    prob: &[f32],
    map_w: usize,
    map_h: usize,
    orig_w: usize,
    orig_h: usize,
) -> Vec<DetQuad> {
    if prob.len() != map_w * map_h {
        return Vec::new();
    }
    // 1. 二值化
    let mut binary = vec![false; map_w * map_h];
    for (i, p) in prob.iter().enumerate() {
        binary[i] = *p > DET_BINARY_THRESH;
    }
    // 2. 2x2 膨胀（OpenCV dilate，anchor=(0,0)：dst(x,y)=OR(src[x..x+1, y..y+1])）
    let mut dilated = vec![false; map_w * map_h];
    for y in 0..map_h {
        for x in 0..map_w {
            let mut hit = binary[y * map_w + x];
            if x + 1 < map_w {
                hit |= binary[y * map_w + x + 1];
            }
            if y + 1 < map_h {
                hit |= binary[(y + 1) * map_w + x];
                if x + 1 < map_w {
                    hit |= binary[(y + 1) * map_w + x + 1];
                }
            }
            dilated[y * map_w + x] = hit;
        }
    }
    // 3. 连通域（并查集，4 连通）
    let mut parent: Vec<usize> = (0..map_w * map_h).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    for y in 0..map_h {
        for x in 0..map_w {
            let i = y * map_w + x;
            if !dilated[i] {
                continue;
            }
            if x > 0 && dilated[i - 1] {
                union(&mut parent, i, i - 1);
            }
            if y > 0 && dilated[i - map_w] {
                union(&mut parent, i, i - map_w);
            }
        }
    }
    let mut comps: std::collections::HashMap<usize, Vec<(usize, usize)>> =
        std::collections::HashMap::new();
    for y in 0..map_h {
        for x in 0..map_w {
            let i = y * map_w + x;
            if dilated[i] {
                let root = find(&mut parent, i);
                comps.entry(root).or_default().push((x, y));
            }
        }
    }
    // 4. 每连通域：凸包 → 最小外接矩形 → 分数 → unclip → 还原坐标
    let mut quads: Vec<DetQuad> = Vec::new();
    for pixels in comps.values() {
        if pixels.len() < 4 {
            continue;
        }
        let pts: Vec<Point2> = pixels.iter().map(|(x, y)| (*x as f32, *y as f32)).collect();
        let hull = convex_hull(&pts);
        if hull.len() < 3 {
            continue;
        }
        let Some((center, axis, half_w, half_h)) = min_area_rect(&hull) else {
            continue;
        };
        if half_w.min(half_h) * 2.0 < 3.0 {
            continue;
        }
        // 5. 框分数：矩形内概率均值（fast 模式）
        let score = quad_score(prob, map_w, map_h, center, axis, half_w, half_h);
        if score < BOX_THRESH {
            continue;
        }
        // 6. unclip：d = area * ratio / perimeter（矩形下 = w*h*r/(2*(w+h))），外扩各边 d
        let w = half_w * 2.0;
        let h = half_h * 2.0;
        let d = w * h * UNCLIP_RATIO / (2.0 * (w + h));
        let new_half_w = half_w + d;
        let new_half_h = half_h + d;
        if new_half_w.min(new_half_h) * 2.0 < 5.0 {
            continue;
        }
        let mut quad = rect_quad(center, axis, new_half_w, new_half_h);
        // 7. 还原到原图坐标（含裁剪）
        for p in quad.iter_mut() {
            p[0] = (p[0] / map_w as f32 * orig_w as f32)
                .round()
                .clamp(0.0, orig_w as f32);
            p[1] = (p[1] / map_h as f32 * orig_h as f32)
                .round()
                .clamp(0.0, orig_h as f32);
        }
        let quad = order_points_clockwise(quad);
        quads.push(DetQuad { pts: quad, score });
    }
    // 8. NMS（按分数降序贪心保留）
    nms(&mut quads, NMS_THRESH);
    quads
}

type Point2 = (f32, f32);
type RectAxes = (Point2, Point2, f32, f32);

/// Andrew 单调链凸包。
fn convex_hull(pts: &[Point2]) -> Vec<Point2> {
    let mut pts = pts.to_vec();
    pts.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap()
            .then(a.1.partial_cmp(&b.1).unwrap())
    });
    pts.dedup();
    if pts.len() <= 2 {
        return pts;
    }
    let mut lower = Vec::new();
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn cross(o: Point2, a: Point2, b: Point2) -> f32 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

/// 凸包最小外接矩形：返回 `(中心, 单位长轴, 半宽, 半高)`。
fn min_area_rect(hull: &[Point2]) -> Option<RectAxes> {
    if hull.len() < 2 {
        return None;
    }
    let mut best_area = f32::INFINITY;
    let mut best: Option<RectAxes> = None;
    let n = hull.len();
    for i in 0..n {
        let (a, b) = (hull[i], hull[(i + 1) % n]);
        let mut dx = b.0 - a.0;
        let mut dy = b.1 - a.1;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            continue;
        }
        dx /= len;
        dy /= len;
        let (nx, ny) = (-dy, dx);
        let (mut u_min, mut u_max) = (f32::INFINITY, f32::NEG_INFINITY);
        let (mut v_min, mut v_max) = (f32::INFINITY, f32::NEG_INFINITY);
        for &p in hull {
            let u = (p.0 - a.0) * dx + (p.1 - a.1) * dy;
            let v = (p.0 - a.0) * nx + (p.1 - a.1) * ny;
            u_min = u_min.min(u);
            u_max = u_max.max(u);
            v_min = v_min.min(v);
            v_max = v_max.max(v);
        }
        let area = (u_max - u_min) * (v_max - v_min);
        if area < best_area {
            best_area = area;
            let cx = a.0 + dx * (u_min + u_max) / 2.0 + nx * (v_min + v_max) / 2.0;
            let cy = a.1 + dy * (u_min + u_max) / 2.0 + ny * (v_min + v_max) / 2.0;
            best = Some((
                (cx, cy),
                (dx, dy),
                (u_max - u_min) / 2.0,
                (v_max - v_min) / 2.0,
            ));
        }
    }
    best
}

/// 由中心/长轴/半宽半高构造 4 角点。
fn rect_quad(center: Point2, axis: Point2, half_w: f32, half_h: f32) -> [[f32; 2]; 4] {
    let (nx, ny) = (-axis.1, axis.0);
    let (cx, cy) = center;
    let corner = |sx: f32, sy: f32| {
        [
            cx + axis.0 * half_w * sx + nx * half_h * sy,
            cy + axis.1 * half_w * sx + ny * half_h * sy,
        ]
    };
    [
        corner(-1.0, -1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ]
}

/// 框内概率均值（fast 模式：矩形内像素均值）。
fn quad_score(
    prob: &[f32],
    map_w: usize,
    map_h: usize,
    center: Point2,
    axis: Point2,
    half_w: f32,
    half_h: f32,
) -> f32 {
    let corners = rect_quad(center, axis, half_w, half_h);
    let x_min = corners
        .iter()
        .map(|p| p[0])
        .fold(f32::INFINITY, f32::min)
        .floor() as i64;
    let x_max = corners
        .iter()
        .map(|p| p[0])
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil() as i64;
    let y_min = corners
        .iter()
        .map(|p| p[1])
        .fold(f32::INFINITY, f32::min)
        .floor() as i64;
    let y_max = corners
        .iter()
        .map(|p| p[1])
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil() as i64;
    let x_min = x_min.max(0) as usize;
    let x_max = (x_max.min(map_w as i64 - 1).max(0)) as usize;
    let y_min = y_min.max(0) as usize;
    let y_max = (y_max.min(map_h as i64 - 1).max(0)) as usize;
    if x_max < x_min || y_max < y_min {
        return 0.0;
    }
    let (cx, cy) = center;
    let (nx, ny) = (-axis.1, axis.0);
    let mut sum = 0f32;
    let mut count = 0usize;
    for y in y_min..=y_max {
        for x in x_min..=x_max {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let u = dx * axis.0 + dy * axis.1;
            let v = dx * nx + dy * ny;
            if u.abs() <= half_w + 0.5 && v.abs() <= half_h + 0.5 {
                sum += prob[y * map_w + x];
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f32
    }
}

/// 按 x 排序 → 左列 tl/bl、右列 tr/br（与 PaddleOCR order_points_clockwise 一致）。
pub fn order_points_clockwise(mut pts: [[f32; 2]; 4]) -> [[f32; 2]; 4] {
    pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
    let (left, right) = (&pts[0..2], &pts[2..4]);
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort_by(|a, b| a[1].partial_cmp(&b[1]).unwrap());
    right.sort_by(|a, b| a[1].partial_cmp(&b[1]).unwrap());
    [left[0], right[0], right[1], left[1]]
}

/// 轴对齐 IoU。
fn quad_iou(a: &DetQuad, b: &DetQuad) -> f32 {
    let (ax, ay, aw, ah) = a.bbox();
    let (bx, by, bw, bh) = b.bbox();
    let ix = (ax + aw).min(bx + bw) - ax.max(bx);
    let iy = (ay + ah).min(by + bh) - ay.max(by);
    if ix <= 0.0 || iy <= 0.0 {
        return 0.0;
    }
    let inter = ix * iy;
    let union = aw * ah + bw * bh - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 贪心 NMS：按分数降序保留，抑制 IoU 超阈值的其余框。
pub fn nms(quads: &mut Vec<DetQuad>, thresh: f32) {
    quads.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    let mut kept: Vec<DetQuad> = Vec::new();
    for q in quads.drain(..) {
        if kept.iter().all(|k| quad_iou(k, &q) <= thresh) {
            kept.push(q);
        }
    }
    *quads = kept;
}

/// 从原图按四边形裁剪（含旋转摆正），输出 BGRA 像素（目标高度 `out_h`，宽度按比例）。
fn crop_quad(img: &BgraImage, quad: &DetQuad, out_h: usize) -> BgraImage {
    let (tl, tr, br, bl) = (quad.pts[0], quad.pts[1], quad.pts[2], quad.pts[3]);
    let top_w = dist(tl, tr);
    let bot_w = dist(bl, br);
    let left_h = dist(tl, bl);
    let right_h = dist(tr, br);
    let width = ((top_w + bot_w) / 2.0).max(2.0);
    let height = ((left_h + right_h) / 2.0).max(2.0);
    let scale = out_h as f32 / height;
    let out_w = ((width * scale).round() as usize).max(2);
    // 文本行方向向量与法向
    let ex = (tr[0] - tl[0], tr[1] - tl[1]);
    let len = (ex.0 * ex.0 + ex.1 * ex.1).sqrt().max(1e-6);
    let ex = (ex.0 / len, ex.1 / len);
    let ey = (-ex.1, ex.0);
    // 左上角为原点，按 (x, y) 采样插值
    let mut pixels = vec![0u8; out_w * out_h * 4];
    for y in 0..out_h {
        for x in 0..out_w {
            let fx = (x as f32 + 0.5) / scale;
            let fy = (y as f32 + 0.5) / scale;
            let sx = tl[0] + ex.0 * fx + ey.0 * fy;
            let sy = tl[1] + ex.1 * fx + ey.1 * fy;
            let [b, g, r] = bilinear(img, sx, sy, 1.0, 1.0);
            let i = (y * out_w + x) * 4;
            pixels[i] = b as u8;
            pixels[i + 1] = g as u8;
            pixels[i + 2] = r as u8;
            pixels[i + 3] = 255;
        }
    }
    BgraImage {
        width: out_w,
        height: out_h,
        pixels,
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// rec 预处理：高 48、宽 = min(ceil(48*ratio), 320)、归一化到 [-1,1]。
pub(crate) fn rec_preprocess(crop: &BgraImage) -> Array4<f32> {
    let ratio = crop.width as f32 / crop.height.max(1) as f32;
    let resized_w = ((REC_HEIGHT as f32 * ratio).ceil() as usize).clamp(2, REC_MAX_WIDTH);
    let mut tensor = vec![0f32; 3 * REC_HEIGHT * resized_w];
    let scale_x = crop.width as f32 / resized_w as f32;
    let scale_y = crop.height as f32 / REC_HEIGHT as f32;
    for y in 0..REC_HEIGHT {
        let src_y = (y as f32 + 0.5) * scale_y - 0.5;
        for x in 0..resized_w {
            let src_x = (x as f32 + 0.5) * scale_x - 0.5;
            let [b, g, r] = bilinear(crop, src_x, src_y, scale_x, scale_y);
            let base = y * resized_w + x;
            tensor[base] = (b / 255.0 - 0.5) / 0.5;
            tensor[REC_HEIGHT * resized_w + base] = (g / 255.0 - 0.5) / 0.5;
            tensor[2 * REC_HEIGHT * resized_w + base] = (r / 255.0 - 0.5) / 0.5;
        }
    }
    Array4::from_shape_vec((1, 3, REC_HEIGHT, resized_w), tensor)
        .expect("rec 张量形状与数据长度一致")
}

/// CTC 解码：argmax → 折叠重复 → 剔除空白（索引 0）→ 字典映射。
/// `probs` 行主序 [T, C]，`dict` 为类别 1..C 的字符表（与 CTCLabelDecode 一致）。
pub fn ctc_decode(probs: &[f32], classes: usize, dict: &[String]) -> (String, f32) {
    let t = probs.len().checked_div(classes).unwrap_or(0);
    let mut chars: Vec<&str> = Vec::new();
    let mut conf_sum = 0f32;
    let mut prev: i64 = -1;
    for i in 0..t {
        let row = &probs[i * classes..(i + 1) * classes];
        let mut best = 0usize;
        let mut best_p = f32::NEG_INFINITY;
        for (c, p) in row.iter().enumerate() {
            if *p > best_p {
                best_p = *p;
                best = c;
            }
        }
        if best == 0 {
            prev = -1;
            continue;
        }
        if best as i64 == prev {
            continue;
        }
        prev = best as i64;
        if let Some(ch) = dict.get(best - 1) {
            chars.push(ch);
            conf_sum += best_p;
        }
    }
    let conf = if chars.is_empty() {
        0.0
    } else {
        conf_sum / chars.len() as f32
    };
    (chars.concat(), conf)
}

/// ONNX OCR 引擎：det + rec 两个会话 + CTC 字典。
pub struct OnnxOcrEngine {
    det: Mutex<Session>,
    rec: Mutex<Session>,
    dict: Vec<String>,
}

impl OnnxOcrEngine {
    /// 从模型目录加载（det/rec/dict 三件套）。
    pub fn load(dir: &Path) -> Result<Self, String> {
        let det_path = dir.join("ch_PP-OCRv4_det_infer.onnx");
        let rec_path = dir.join("ch_PP-OCRv4_rec_infer.onnx");
        let dict_path = dir.join("ppocr_keys_v1.txt");
        if !det_path.exists() || !rec_path.exists() || !dict_path.exists() {
            return Err(format!(
                "ONNX OCR 模型不完整（{}）：需要 ch_PP-OCRv4_det_infer.onnx / ch_PP-OCRv4_rec_infer.onnx / ppocr_keys_v1.txt",
                dir.display()
            ));
        }
        let det = {
            let mut builder = Session::builder().map_err(|e| format!("创建 ONNX 会话失败：{e}"))?;
            builder
                .commit_from_file(&det_path)
                .map_err(|e| format!("加载 det 模型失败：{e}"))?
        };
        let rec = {
            let mut builder = Session::builder().map_err(|e| format!("创建 ONNX 会话失败：{e}"))?;
            builder
                .commit_from_file(&rec_path)
                .map_err(|e| format!("加载 rec 模型失败：{e}"))?
        };
        let dict = load_dict(&dict_path)?;
        Ok(Self {
            det: Mutex::new(det),
            rec: Mutex::new(rec),
            dict,
        })
    }

    /// 运行 det：输入 CHW 归一化张量，返回概率图（行主序 [map_h, map_w]）。
    fn run_det(&self, tensor: &Array4<f32>) -> Result<Vec<f32>, String> {
        let input =
            Tensor::from_array(tensor.clone()).map_err(|e| format!("det 输入张量失败：{e}"))?;
        let mut session = self.det.lock().map_err(|_| "det 会话锁中毒".to_string())?;
        let outputs = session
            .run(ort::inputs!["x" => input])
            .map_err(|e| format!("det 推理失败：{e}"))?;
        let value = outputs
            .values()
            .next()
            .ok_or_else(|| "det 无输出".to_string())?;
        let (shape, data) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("det 输出提取失败：{e}"))?;
        let dims = shape.to_ixdyn();
        let dims: Vec<usize> = ndarray::Dimension::slice(&dims).to_vec();
        if dims.len() != 4 || data.len() != dims[2] * dims[3] {
            return Err(format!("det 输出形状异常：{dims:?}"));
        }
        // 输出为 sigmoid 概率图 [1,1,H,W]，行主序直接返回。
        Ok(data.iter().map(|p| p.clamp(0.0, 1.0)).collect())
    }

    /// 运行 rec：输入 [1,3,48,W]，返回 `(概率行主序 [T, C], T, C)`。
    fn run_rec(&self, tensor: &Array4<f32>) -> Result<(Vec<f32>, usize, usize), String> {
        let input =
            Tensor::from_array(tensor.clone()).map_err(|e| format!("rec 输入张量失败：{e}"))?;
        let mut session = self.rec.lock().map_err(|_| "rec 会话锁中毒".to_string())?;
        let outputs = session
            .run(ort::inputs!["x" => input])
            .map_err(|e| format!("rec 推理失败：{e}"))?;
        let value = outputs
            .values()
            .next()
            .ok_or_else(|| "rec 无输出".to_string())?;
        let (shape, data) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("rec 输出提取失败：{e}"))?;
        let dims = shape.to_ixdyn();
        let dims: Vec<usize> = ndarray::Dimension::slice(&dims).to_vec();
        if dims.len() != 3 {
            return Err(format!("rec 输出形状异常：{dims:?}"));
        }
        let (t, classes) = (dims[1], dims[2]);
        if data.len() != t * classes {
            return Err(format!("rec 输出数据长度异常：{dims:?}"));
        }
        Ok((data.to_vec(), t, classes))
    }
}

/// 加载 CTC 字典（按行拆分，追加空格符，类索引 0 为 blank）。
pub fn load_dict(path: &Path) -> Result<Vec<String>, String> {
    let content = std::fs::read(path).map_err(|e| format!("读取字典失败：{e}"))?;
    let content = String::from_utf8(content).map_err(|e| format!("字典非 UTF-8：{e}"))?;
    let mut chars: Vec<String> = Vec::new();
    for line in content.lines() {
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        chars.push(line.to_string());
    }
    chars.push(' '.to_string());
    if chars.len() < 2 {
        return Err("字典为空".to_string());
    }
    Ok(chars)
}

/// 模型目录：`OWO_ONNX_OCR_MODEL_DIR` 或 `<data>/models/ocr`。
pub fn model_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OWO_ONNX_OCR_MODEL_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    let data_root = std::env::var("OWO_AGENT_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let base = std::env::var("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."));
            base.join("OwO").join("Agent")
        });
    data_root.join("models").join("ocr")
}

/// 模型三件套是否就绪（仅检查文件存在，不加载）。
pub fn models_present(dir: &Path) -> bool {
    dir.join("ch_PP-OCRv4_det_infer.onnx").exists()
        && dir.join("ch_PP-OCRv4_rec_infer.onnx").exists()
        && dir.join("ppocr_keys_v1.txt").exists()
}

static ONNX_ENGINE: OnceLock<Mutex<Option<Arc<OnnxOcrEngine>>>> = OnceLock::new();

/// 惰性加载并缓存引擎；模型缺失返回 None（供调用方降级）。
pub fn cached_engine() -> Option<Arc<OnnxOcrEngine>> {
    let cell = ONNX_ENGINE.get_or_init(|| Mutex::new(None));
    let Ok(mut guard) = cell.lock() else {
        return None;
    };
    if guard.is_none() {
        let dir = model_dir();
        if models_present(&dir) {
            match OnnxOcrEngine::load(&dir) {
                Ok(engine) => *guard = Some(Arc::new(engine)),
                Err(e) => {
                    // 引擎加载失败（onnxruntime.dll 缺失/模型损坏）→ 保持 None，调用方降级后续通道。
                    tracing::warn!(target: "owo", "ONNX OCR 引擎加载失败，降级后续 OCR 通道：{e}");
                }
            }
        }
    }
    guard.clone()
}

/// 清空引擎缓存（测试/模型更新后用）。
pub fn reset_engine_cache() {
    if let Some(cell) = ONNX_ENGINE.get() {
        if let Ok(mut guard) = cell.lock() {
            *guard = None;
        }
    }
}

/// 完整 ONNX OCR 流水线：BMP → 检测 → 识别 → OcrSummary。
pub fn ocr_bmp_onnx(bmp: &[u8], engine: &OnnxOcrEngine) -> Result<OcrSummary, String> {
    let img = parse_bmp32(bmp)?;
    let det_input = det_preprocess(&img);
    let prob = engine.run_det(&det_input.tensor)?;
    let quads = db_postprocess(
        &prob,
        det_input.map_w,
        det_input.map_h,
        img.width,
        img.height,
    );
    if quads.is_empty() {
        return Err("ONNX OCR 未检测到文本".to_string());
    }
    // 噪声框过滤：过小（<16x8）或细长条（高度小且宽高比大）
    let mut boxes_in: Vec<(f32, f32, f32, f32)> = Vec::new();
    for quad in &quads {
        let (bx, by, bw, bh) = quad.bbox();
        if bw < 16.0 || bh < 8.0 {
            continue;
        }
        if bh < 30.0 && bw / bh.max(1.0) > 8.0 {
            continue;
        }
        boxes_in.push((bx, by, bw, bh));
    }
    // 行分组：y 范围重叠 ≥50% 视为同行；同行内按 x 排序并合并相邻框（gap < 24px），
    // 避免 det 在字符间隙切出多个框导致识别顺序错乱/粘连伪字符。
    boxes_in.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap()
            .then(a.0.partial_cmp(&b.0).unwrap())
    });
    let mut rows: Vec<Vec<(f32, f32, f32, f32)>> = Vec::new();
    for bx in boxes_in {
        let mut found = None;
        for (index, row) in rows.iter().enumerate() {
            let (_, ry, _, rh) = row[0];
            let inter = (ry + rh).min(bx.1 + bx.3) - ry.max(bx.1);
            if inter > rh.min(bx.3) * 0.5 {
                found = Some(index);
                break;
            }
        }
        if let Some(index) = found {
            rows[index].push(bx);
        } else {
            rows.push(vec![bx]);
        }
    }
    for row in rows.iter_mut() {
        row.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    }
    rows.sort_by(|a, b| a[0].1.partial_cmp(&b[0].1).unwrap());
    let mut merged_rows: Vec<Vec<(f32, f32, f32, f32)>> = Vec::new();
    for row in rows {
        let mut merged: Vec<(f32, f32, f32, f32)> = Vec::new();
        for bx in row {
            if let Some(last) = merged.last_mut() {
                let gap = bx.0 - (last.0 + last.2);
                if gap < 24.0 {
                    let right = (last.0 + last.2).max(bx.0 + bx.2);
                    let bottom = (last.1 + last.3).max(bx.1 + bx.3);
                    last.0 = last.0.min(bx.0);
                    last.1 = last.1.min(bx.1);
                    last.2 = right - last.0;
                    last.3 = bottom - last.1;
                    continue;
                }
            }
            merged.push(bx);
        }
        if !merged.is_empty() {
            merged_rows.push(merged);
        }
    }
    if merged_rows.is_empty() {
        return Err("ONNX OCR 未识别出文本".to_string());
    }
    let mut text_lines = Vec::new();
    let mut boxes = Vec::new();
    for row in merged_rows {
        for (x, y, w, h) in row {
            let quad_box = DetQuad {
                pts: [[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
                score: 1.0,
            };
            let crop = crop_quad(&img, &quad_box, REC_HEIGHT);
            let rec_input = rec_preprocess(&crop);
            let (probs, _t, classes) = engine.run_rec(&rec_input)?;
            let (text, conf) = ctc_decode(&probs, classes, &engine.dict);
            if text.is_empty() || conf < REC_TEXT_SCORE {
                continue;
            }
            text_lines.push(text.clone());
            boxes.push(OcrBox {
                text: text.trim().to_string(),
                x: x as i32,
                y: y as i32,
                width: w as i32,
                height: h as i32,
            });
        }
    }
    if text_lines.is_empty() {
        return Err("ONNX OCR 未识别出文本".to_string());
    }
    let text = text_lines.join("\n");
    Ok(OcrSummary {
        chars: text.chars().count(),
        text,
        boxes,
        provider: Some("onnx-v4".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_bmp(width: usize, height: usize, fill: [u8; 3]) -> Vec<u8> {
        let mut bmp = Vec::with_capacity(54 + width * height * 4);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&((54 + width * height * 4) as u32).to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&(width as i32).to_le_bytes());
        bmp.extend_from_slice(&(height as i32).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&((width * height * 4) as u32).to_le_bytes());
        bmp.extend_from_slice(&0i32.to_le_bytes());
        bmp.extend_from_slice(&0i32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..width * height {
            bmp.extend_from_slice(&[fill[0], fill[1], fill[2], 255]);
        }
        bmp
    }

    #[test]
    fn parse_bmp32_rejects_invalid() {
        let bmp = test_bmp(8, 8, [255, 255, 255]);
        assert_eq!(bmp.len(), 54 + 8 * 8 * 4, "BMP 头应为 54 字节");
        assert!(parse_bmp32(&[0u8; 10]).is_err());
        let img = parse_bmp32(&bmp).unwrap();
        assert_eq!((img.width, img.height), (8, 8));
        assert_eq!(img.pixels.len(), 8 * 8 * 4);
        assert!(parse_bmp32(&test_bmp(4, 4, [0, 0, 0])[..54]).is_err());
    }

    #[test]
    fn det_preprocess_shapes_and_32_alignment() {
        let img = parse_bmp32(&test_bmp(100, 50, [128, 64, 32])).unwrap();
        let input = det_preprocess(&img);
        let shape = input.tensor.shape();
        assert_eq!(shape[0], 1);
        assert_eq!(shape[1], 3);
        assert_eq!(shape[2] % 32, 0);
        assert_eq!(shape[3] % 32, 0);
        // 大图：min 侧 1080 > 736 不缩放；32 对齐后高 1088
        let img = parse_bmp32(&test_bmp(1920, 1080, [0, 0, 0])).unwrap();
        let input = det_preprocess(&img);
        assert_eq!(input.map_h, 1088);
        assert_eq!(input.map_w, 1920);
    }

    #[test]
    fn det_preprocess_normalizes_bgr_channels() {
        // 纯色图：缩放不改变值，检查通道归一化
        let img = parse_bmp32(&test_bmp(64, 64, [51, 102, 153])).unwrap();
        let input = det_preprocess(&img);
        let data = input.tensor.iter().cloned().collect::<Vec<f32>>();
        let hw = input.map_h * input.map_w;
        let expect_b = (51.0 / 255.0 - 0.485) / 0.229;
        let expect_g = (102.0 / 255.0 - 0.456) / 0.224;
        let expect_r = (153.0 / 255.0 - 0.406) / 0.225;
        assert!((data[0] - expect_b).abs() < 1e-4);
        assert!((data[hw] - expect_g).abs() < 1e-4);
        assert!((data[2 * hw] - expect_r).abs() < 1e-4);
    }

    #[test]
    fn db_postprocess_detects_rect_and_scales_coords() {
        // 100x60 概率图：中央 30x20 矩形概率 0.9，其余 0.05
        let (w, h) = (100usize, 60usize);
        let mut prob = vec![0.05f32; w * h];
        for y in 20..40 {
            for x in 30..60 {
                prob[y * w + x] = 0.9;
            }
        }
        let quads = db_postprocess(&prob, w, h, 200, 120);
        assert_eq!(quads.len(), 1, "应检出 1 个框");
        let (bx, by, bw, bh) = quads[0].bbox();
        // unclip d = 30*20*1.6/(2*50) = 9.6 → 框在 map 坐标 (20.4, 10.4)-(69.6, 49.6)
        // 还原到 200x120：x≈40.8→41, y≈20.8→21, w≈98.4, h≈78.4
        assert!((bx - 41.0).abs() < 8.0, "x 应还原放大到原图坐标，got {bx}");
        assert!((by - 21.0).abs() < 8.0, "y，got {by}");
        assert!(bw > 85.0 && bw < 115.0, "unclip 后宽度≈98.4，got {bw}");
        assert!(bh > 65.0 && bh < 95.0, "unclip 后高度≈78.4，got {bh}");
    }

    #[test]
    fn db_postprocess_nms_merges_overlap() {
        let (w, h) = (100usize, 100usize);
        let mut prob = vec![0.05f32; w * h];
        for y in 30..70 {
            for x in 30..70 {
                prob[y * w + x] = 0.9;
            }
        }
        // 相邻连通域经膨胀合并 → 1 个
        let quads = db_postprocess(&prob, w, h, w, h);
        assert_eq!(quads.len(), 1);
        // 两个分离矩形 → 2 个
        let mut prob2 = vec![0.05f32; w * h];
        for (x0, y0) in [(10usize, 10usize), (60usize, 60usize)] {
            for y in y0..y0 + 25 {
                for x in x0..x0 + 25 {
                    prob2[y * w + x] = 0.9;
                }
            }
        }
        let quads2 = db_postprocess(&prob2, w, h, w, h);
        assert_eq!(quads2.len(), 2);
    }

    #[test]
    fn ctc_decode_collapses_repeats_and_blank() {
        let dict = vec![
            "你".to_string(),
            "好".to_string(),
            "世".to_string(),
            "界".to_string(),
            " ".to_string(),
        ];
        // 5 步：blank(0), 好(2), 好(2), blank(0), 你(1)
        let classes = dict.len() + 1;
        let mut probs = vec![0.0f32; 5 * classes];
        let mut set = |t: usize, c: usize, v: f32| probs[t * classes + c] = v;
        set(0, 0, 0.8);
        set(1, 2, 0.9);
        set(2, 2, 0.85);
        set(3, 0, 0.8);
        set(4, 1, 0.7);
        let (text, conf) = ctc_decode(&probs, classes, &dict);
        assert_eq!(text, "好你");
        assert!(
            (0.8 - conf).abs() < 1e-6,
            "conf 应为 (0.9+0.7)/2=0.8，got {conf}"
        );
    }

    #[test]
    fn load_dict_appends_space() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("owo_dict_test_{}.txt", std::process::id()));
        std::fs::write(&path, "a\nb\n").unwrap();
        let dict = load_dict(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            dict,
            vec!["a".to_string(), "b".to_string(), " ".to_string()]
        );
    }

    #[test]
    fn order_points_clockwise_sorts_corners() {
        let pts = [[5.0, 1.0], [1.0, 1.0], [1.0, 5.0], [5.0, 5.0]];
        let ordered = order_points_clockwise(pts);
        assert_eq!(ordered[0], [1.0, 1.0]); // tl
        assert_eq!(ordered[1], [5.0, 1.0]); // tr
        assert_eq!(ordered[2], [5.0, 5.0]); // br
        assert_eq!(ordered[3], [1.0, 5.0]); // bl
    }

    #[test]
    fn rec_preprocess_shapes() {
        // 宽 96 高 24 → 等比放大到高 48，宽 192
        let img = parse_bmp32(&test_bmp(96, 24, [200, 100, 50])).unwrap();
        let input = rec_preprocess(&img);
        let shape = input.shape().to_vec();
        assert_eq!(shape[2], REC_HEIGHT);
        assert_eq!(shape[3], 192);
        assert!(shape[3] <= REC_MAX_WIDTH);
        // 窄图：宽高比 < 1 时宽按比例（24x48 → 24x48）
        let img = parse_bmp32(&test_bmp(24, 48, [200, 100, 50])).unwrap();
        let input = rec_preprocess(&img);
        assert_eq!(input.shape()[3], 24);
    }

    /// 真实模型集成测试：模型就绪时（`scripts/download-onnx-ocr-models.ps1` 下载后）
    /// 渲染中文/英文文本并验证识别字符重合率 ≥ 阈值；模型缺失自动跳过。
    #[test]
    fn onnx_ocr_real_models_when_present() {
        let dir = model_dir();
        if !models_present(&dir) {
            eprintln!("跳过：ONNX OCR 模型未就绪（{}）", dir.display());
            return;
        }
        reset_engine_cache();
        let Some(engine) = cached_engine() else {
            eprintln!("跳过：ONNX OCR 引擎加载失败");
            return;
        };
        let cases = [
            ("发送", "发送"),
            ("输入消息", "输入消息"),
            ("hello world", "hello world"),
            ("你好 世界", "你好 世界"),
            ("输入消息\n发送", "输入消息\n发送"),
        ];
        for (rendered, expected) in cases {
            let Some(bmp) = crate::platform::render_text_bmp(rendered, 36) else {
                eprintln!("跳过：GDI 文本渲染不可用");
                return;
            };
            match ocr_bmp_onnx(&bmp, &engine) {
                Ok(summary) => {
                    let got: String = summary
                        .text
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect();
                    let want: String = expected.chars().filter(|c| !c.is_whitespace()).collect();
                    // 顺序敏感：单行渲染文本应逐字符一致（LCS 重合率）
                    let lcs = lcs_overlap(&got, &want);
                    eprintln!("[onnx] 渲染 {rendered:?} → 识别 {got:?}（LCS 重合 {lcs:.2}）");
                    assert!(lcs >= 0.8, "识别重合率过低：渲染 {rendered:?} 识别 {got:?}");
                }
                Err(e) => panic!("ONNX OCR 失败：{e}"),
            }
        }
    }

    /// 最长公共子序列重合率（顺序敏感）。
    fn lcs_overlap(got: &str, want: &str) -> f32 {
        if want.is_empty() {
            return 0.0;
        }
        let a: Vec<char> = got.chars().collect();
        let b: Vec<char> = want.chars().collect();
        let mut dp = vec![0usize; (b.len() + 1) * (a.len() + 1)];
        for i in 1..=a.len() {
            for j in 1..=b.len() {
                dp[i * (b.len() + 1) + j] = if a[i - 1] == b[j - 1] {
                    dp[(i - 1) * (b.len() + 1) + j - 1] + 1
                } else {
                    dp[(i - 1) * (b.len() + 1) + j].max(dp[i * (b.len() + 1) + j - 1])
                };
            }
        }
        dp[a.len() * (b.len() + 1) + b.len()] as f32 / b.len() as f32
    }
}
