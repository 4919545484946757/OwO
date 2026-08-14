//! L2 本地摘要（v0.4 P2）：Windows 自带 OCR（Media.Ocr，离线）。
//! 输入内存 BMP，输出文字摘要；全程不落盘，隐私边界与截图环形缓冲一致。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrBox {
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrSummary {
    pub text: String,
    pub chars: usize,
    /// 逐词识别框（屏幕坐标，供 OCR+坐标点击使用）。
    #[serde(default)]
    pub boxes: Vec<OcrBox>,
    /// 识别引擎（media / paddle-v6），便于诊断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrLine {
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrEngineStatus {
    pub engine_created: bool,
    pub max_image_dimension: u32,
    /// 本地 ONNX OCR（M-E）模型是否就绪。
    #[serde(default)]
    pub onnx_models_present: bool,
}

/// OCR 引擎诊断：语言包是否存在、最大图像尺寸、可用识别语言。
#[cfg(target_os = "windows")]
pub fn ocr_engine_status() -> OcrEngineStatus {
    use windows::Media::Ocr::OcrEngine;
    let engine_created = OcrEngine::TryCreateFromUserProfileLanguages().is_ok();
    let max_image_dimension = OcrEngine::MaxImageDimension().unwrap_or(0);
    OcrEngineStatus {
        engine_created,
        max_image_dimension,
        onnx_models_present: crate::onnx_ocr::models_present(&crate::onnx_ocr::model_dir()),
    }
}

#[cfg(not(target_os = "windows"))]
pub fn ocr_engine_status() -> OcrEngineStatus {
    OcrEngineStatus {
        engine_created: false,
        max_image_dimension: 0,
        onnx_models_present: false,
    }
}

/// 对内存 BMP 做本地 OCR，返回文字摘要（无文字时返回 None）。
#[cfg(target_os = "windows")]
pub fn ocr_bmp(bmp: &[u8]) -> Option<OcrSummary> {
    ocr_bmp_detailed(bmp).ok()
}

/// 带错误原因的 OCR（诊断用）：返回失败原因而不是静默 None。
#[cfg(target_os = "windows")]
pub fn ocr_bmp_detailed(bmp: &[u8]) -> Result<OcrSummary, String> {
    use futures::executor::block_on;
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;

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
    let pixels = &bmp[54..54 + expected];

    block_on(async move {
        let writer = DataWriter::new().map_err(|error| format!("创建 DataWriter 失败：{error}"))?;
        writer
            .WriteBytes(pixels)
            .map_err(|error| format!("写入像素失败：{error}"))?;
        let buffer = writer
            .DetachBuffer()
            .map_err(|error| format!("取出像素缓冲失败：{error}"))?;
        let software =
            SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, width, height)
                .map_err(|error| format!("构造 SoftwareBitmap 失败：{error}"))?;
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()
            .map_err(|error| format!("OCR 引擎创建失败：{error}"))?;
        if OcrEngine::MaxImageDimension()
            .map(|dim| dim == 0)
            .unwrap_or(true)
        {
            return Err("系统未安装 OCR 语言包".to_string());
        }
        let result = engine
            .RecognizeAsync(&software)
            .map_err(|error| format!("发起识别失败：{error}"))?
            .await
            .map_err(|error| format!("识别等待失败：{error}"))?;
        let text = result
            .Text()
            .map_err(|error| format!("读取识别文本失败：{error}"))?
            .to_string();
        let mut boxes = Vec::new();
        if let Ok(lines) = result.Lines() {
            if let Ok(line_count) = lines.Size() {
                for line_index in 0..line_count {
                    if let Ok(line) = lines.GetAt(line_index) {
                        if let Ok(words) = line.Words() {
                            if let Ok(word_count) = words.Size() {
                                for word_index in 0..word_count {
                                    if let Ok(word) = words.GetAt(word_index) {
                                        let word_text = word
                                            .Text()
                                            .map(|value| value.to_string())
                                            .unwrap_or_default();
                                        if let Ok(rect) = word.BoundingRect() {
                                            boxes.push(OcrBox {
                                                text: word_text,
                                                x: rect.X as i32,
                                                y: rect.Y as i32,
                                                width: rect.Width as i32,
                                                height: rect.Height as i32,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if text.trim().is_empty() {
            Err("识别结果为空".to_string())
        } else {
            Ok(OcrSummary {
                chars: text.trim().chars().count(),
                text,
                boxes,
                provider: Some("media".to_string()),
            })
        }
    })
}

/// 裁剪 + 放大的 BMP（最近邻），供小字区域 OCR（验证窗口/红包面板）。
pub fn crop_scale_bmp(
    bmp: &[u8],
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: u32,
) -> Result<Vec<u8>, String> {
    if bmp.len() < 54 || &bmp[..2] != b"BM" {
        return Err("BMP 头无效".to_string());
    }
    let src_width = i32::from_le_bytes([bmp[18], bmp[19], bmp[20], bmp[21]]);
    let src_height = i32::from_le_bytes([bmp[22], bmp[23], bmp[24], bmp[25]]).abs();
    let bit_count = u16::from_le_bytes([bmp[28], bmp[29]]);
    if bit_count != 32 || src_width <= 0 || src_height <= 0 {
        return Err("仅支持 32bpp BMP".to_string());
    }
    let x = x.max(0).min(src_width - 1);
    let y = y.max(0).min(src_height - 1);
    let width = width.min(src_width - x).max(1);
    let height = height.min(src_height - y).max(1);
    let scale = scale.max(1) as usize;
    let src = &bmp[54..];
    let out_width = width as usize * scale;
    let out_height = height as usize * scale;
    let mut out = Vec::with_capacity(out_width * out_height * 4);
    for out_y in 0..out_height {
        let src_y = y + (out_y / scale) as i32;
        for out_x in 0..out_width {
            let src_x = x + (out_x / scale) as i32;
            let src_index = ((src_y * src_width + src_x) as usize) * 4;
            out.extend_from_slice(&src[src_index..src_index + 4]);
        }
    }
    Ok(encode_bmp32(out_width as i32, out_height as i32, &out))
}

/// 区域 OCR：全屏 BMP → 裁剪放大 → 识别（小字验证窗口/自绘面板用）。
pub fn ocr_bmp_region(
    bmp: &[u8],
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: u32,
) -> Result<OcrSummary, String> {
    let cropped = crop_scale_bmp(bmp, x, y, width, height, scale)?;
    ocr_bmp_detailed(&cropped)
}

/// 把逐词框按“同行 + 邻近”合并为整行文本与包围盒（OcrLayout 的轻量版）。
/// Media.Ocr 对中文通常每字一个词框，合并后才能稳定匹配“发送”“输入消息”等按钮/控件。
pub fn group_ocr_lines(boxes: &[OcrBox]) -> Vec<OcrLine> {
    if boxes.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&OcrBox> = boxes.iter().collect();
    sorted.sort_by_key(|b| (b.y, b.x));
    let mut lines: Vec<OcrLine> = Vec::new();
    for b in sorted {
        let y_center = b.y + b.height / 2;
        let merged = lines.iter_mut().find(|line| {
            let line_center = line.y + line.height / 2;
            (line_center - y_center).abs() <= (line.height.max(b.height) / 2 + 6)
                && b.x >= line.x
                && b.x - (line.x + line.width) <= 36
        });
        match merged {
            Some(line) => {
                line.text.push_str(&b.text);
                line.width = (b.x + b.width) - line.x;
                line.height = line.height.max(b.height);
            }
            None => {
                lines.push(OcrLine {
                    text: b.text.clone(),
                    x: b.x,
                    y: b.y,
                    width: b.width,
                    height: b.height,
                });
            }
        }
    }
    // 合并后的行按 y 排序，去掉空文本。
    lines.retain(|line| !line.text.trim().is_empty());
    lines.sort_by_key(|line| (line.y, line.x));
    lines
}

fn encode_bmp32(width: i32, height: i32, bgra: &[u8]) -> Vec<u8> {
    let file_size = 54 + bgra.len();
    let mut out = Vec::with_capacity(file_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(bgra.len() as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(bgra);
    out
}

#[cfg(not(target_os = "windows"))]
pub fn ocr_bmp(_bmp: &[u8]) -> Option<OcrSummary> {
    None
}

#[cfg(not(target_os = "windows"))]
pub fn ocr_bmp_detailed(_bmp: &[u8]) -> Result<OcrSummary, String> {
    Err("本地 OCR 暂仅支持 Windows".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_is_callable_and_never_panics() {
        // 4x4 空白 BMP：无文字时应返回 None；有 OCR 语言包也不会崩溃。
        if let Some(bmp) = crate::platform::capture_screen_region(4, 4) {
            let _ = ocr_bmp(&bmp);
        }
    }

    #[test]
    fn crop_scale_bmp_produces_scaled_32bpp_bmp() {
        let bmp = encode_bmp32(4, 4, &[0u8; 64]);
        let cropped = crop_scale_bmp(&bmp, 1, 1, 2, 2, 3).unwrap();
        assert_eq!(&cropped[..2], b"BM");
        let width = i32::from_le_bytes([cropped[18], cropped[19], cropped[20], cropped[21]]);
        let height = i32::from_le_bytes([cropped[22], cropped[23], cropped[24], cropped[25]]);
        assert_eq!((width, height.abs()), (6, 6));
        assert!(crop_scale_bmp(&bmp, 0, 0, 10, 10, 1).is_ok());
        assert!(crop_scale_bmp(&[0u8; 10], 0, 0, 1, 1, 1).is_err());
    }

    #[test]
    fn group_ocr_lines_merges_adjacent_character_boxes() {
        let boxes = vec![
            OcrBox {
                text: "发".into(),
                x: 100,
                y: 50,
                width: 18,
                height: 18,
            },
            OcrBox {
                text: "送".into(),
                x: 118,
                y: 50,
                width: 18,
                height: 18,
            },
            OcrBox {
                text: "在".into(),
                x: 20,
                y: 80,
                width: 18,
                height: 18,
            },
            OcrBox {
                text: "吗".into(),
                x: 38,
                y: 80,
                width: 18,
                height: 18,
            },
            OcrBox {
                text: "？".into(),
                x: 56,
                y: 80,
                width: 16,
                height: 18,
            },
            OcrBox {
                text: "发送".into(),
                x: 200,
                y: 120,
                width: 36,
                height: 18,
            },
        ];
        let lines = group_ocr_lines(&boxes);
        assert!(lines.iter().any(|line| line.text == "发送"));
        let question = lines
            .iter()
            .find(|line| line.text == "在吗？")
            .expect("应合并出整行");
        assert_eq!(question.x, 20);
        assert_eq!(lines[0].x, 100); // 按 y 排序：y=50 的“发送”在前
    }
}
