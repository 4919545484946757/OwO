//! 视觉模型网关（v0.4.4，设计文档 M-B 起步）。
//!
//! 职责：把屏幕/模拟窗口截图交给视觉模型做“场景描述/完成验证”，
//! 主控制仍走 OCR + 坐标的确定性链路，视觉只做理解与异步确认。
//!
//! 通道：
//! - `ollama`（默认）：本地 Ollama `/api/generate`，模型由 `OWO_VISION_MODEL` 指定
//!   （默认 qwen2.5vl:3b，可用 llava/minicpm-v 等）。
//! - `openai`：任意 OpenAI-compatible 视觉端点（BYOK），
//!   通过 `OWO_VISION_BASE_URL` / `OWO_VISION_API_KEY` 配置。

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionConfig {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub ollama_host: String,
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl VisionConfig {
    pub fn from_env() -> Self {
        let provider = std::env::var("OWO_VISION_PROVIDER")
            .unwrap_or_else(|_| "ollama".to_string())
            .to_lowercase();
        let model = std::env::var("OWO_VISION_MODEL").unwrap_or_else(|_| {
            if provider == "openai" {
                "gpt-4o-mini".to_string()
            } else {
                "qwen2.5vl:3b".to_string()
            }
        });
        Self {
            provider,
            model,
            base_url: std::env::var("OWO_VISION_BASE_URL").ok(),
            api_key: std::env::var("OWO_VISION_API_KEY").ok(),
            ollama_host: std::env::var("OLLAMA_HOST").unwrap_or_else(|_| {
                std::env::var("OWO_OLLAMA_HOST")
                    .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string())
            }),
        }
    }
}

/// 把 32bpp BMP（内存帧）转成 PNG，供视觉模型使用。
pub fn bmp_to_png(bmp: &[u8]) -> Result<Vec<u8>, String> {
    if bmp.len() < 54 || &bmp[..2] != b"BM" {
        return Err("BMP 头无效".to_string());
    }
    let width = i32::from_le_bytes([bmp[18], bmp[19], bmp[20], bmp[21]]);
    let height = i32::from_le_bytes([bmp[22], bmp[23], bmp[24], bmp[25]]).abs();
    let bit_count = u16::from_le_bytes([bmp[28], bmp[29]]);
    if bit_count != 32 || width <= 0 || height <= 0 {
        return Err(format!("不支持的 BMP：{width}x{height} {bit_count}bpp"));
    }
    let expected = (width as usize) * (height as usize) * 4;
    if bmp.len() < 54 + expected {
        return Err("BMP 像素数据不完整".to_string());
    }
    let mut rgba = Vec::with_capacity(expected + expected / 4);
    for pixel in bmp[54..54 + expected].chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("PNG 头写入失败：{e}"))?;
        writer
            .write_image_data(&rgba)
            .map_err(|e| format!("PNG 像素写入失败：{e}"))?;
    }
    Ok(out)
}

/// 把当前视觉面（模拟帧或真实屏幕）截图转成 PNG。
pub async fn capture_vision_png() -> Result<(Vec<u8>, String), String> {
    let (bmp, surface) = capture_vision_bmp().await?;
    bmp_to_png(&bmp).map(|png| (png, surface))
}

/// 获取当前视觉面的原始 BMP + 表面名（模拟帧或真实屏幕）。
pub async fn capture_vision_bmp() -> Result<(Vec<u8>, String), String> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        let base = std::env::var("OWO_SIM_QQ_URL").unwrap();
        let url = format!("{}/frame", base.trim_end_matches('/'));
        let response = reqwest::get(&url)
            .await
            .map_err(|e| format!("模拟窗口截图失败：{e}"))?;
        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("模拟窗口截图读取失败：{e}"))?
            .to_vec();
        return Ok((bytes, "sim".to_string()));
    }
    let bytes = crate::platform::capture_screen().ok_or("屏幕截图失败")?;
    Ok((bytes, "desktop".to_string()))
}

/// 获取视觉面指定区域（裁剪+放大）的 PNG，供小字/局部验证。
pub async fn capture_vision_png_region(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: u32,
) -> Result<(Vec<u8>, String), String> {
    let (bmp, surface) = capture_vision_bmp().await?;
    let cropped = crate::ocr::crop_scale_bmp(&bmp, x, y, width, height, scale)?;
    bmp_to_png(&cropped).map(|png| (png, surface))
}

/// 视觉模型场景描述/验证（主入口）。
pub async fn describe_image(png: &[u8], prompt: &str) -> Result<String, String> {
    let config = VisionConfig::from_env();
    match config.provider.as_str() {
        "openai" => describe_openai(&config, png, prompt).await,
        _ => describe_ollama(&config, png, prompt).await,
    }
}

/// 查询 Ollama 已拉取的模型列表（视觉模型可用性诊断）。
pub async fn ollama_models(config: &VisionConfig) -> Vec<String> {
    let url = format!("{}/api/tags", config.ollama_host.trim_end_matches('/'));
    let Ok(response) = reqwest::get(&url).await else {
        return Vec::new();
    };
    let Ok(value) = response.json::<Value>().await else {
        return Vec::new();
    };
    value
        .get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn describe_ollama(
    config: &VisionConfig,
    png: &[u8],
    prompt: &str,
) -> Result<String, String> {
    let url = format!("{}/api/generate", config.ollama_host.trim_end_matches('/'));
    let body = json!({
        "model": config.model,
        "prompt": prompt,
        "images": [BASE64.encode(png)],
        "stream": false,
        "options": { "num_predict": 700 },
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(240))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Ollama 请求失败（{}）：{e}", config.model))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|e| format!("Ollama 响应解析失败：{e}"))?;
    if !status.is_success() {
        let error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(format!(
            "视觉模型不可用：{error}（请先运行 ollama pull {}）",
            config.model
        ));
    }
    value
        .get("response")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "Ollama 响应缺少 response 字段".to_string())
}

async fn describe_openai(
    config: &VisionConfig,
    png: &[u8],
    prompt: &str,
) -> Result<String, String> {
    let base = config
        .base_url
        .clone()
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let data_url = format!("data:image/png;base64,{}", BASE64.encode(png));
    let body = json!({
        "model": config.model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]
        }],
        "max_tokens": 700,
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;
    let mut request = client.post(&url).json(&body);
    if let Some(key) = &config.api_key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("视觉端点请求失败：{e}"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|e| format!("视觉端点响应解析失败：{e}"))?;
    if !status.is_success() {
        return Err(format!(
            "视觉端点返回 {}：{}",
            status,
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("未知错误")
        ));
    }
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "视觉端点响应缺少 choices[0].message.content".to_string())
}

/// 解析视觉验证回答：期望以 YES/NO 开头，附 0-1 置信度。
pub fn parse_verification(text: &str) -> (String, Option<f64>) {
    let upper = text.trim().to_uppercase();
    let answer = if upper.starts_with("YES") {
        "yes"
    } else if upper.starts_with("NO") {
        "no"
    } else {
        "unknown"
    };
    let confidence = text
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .filter_map(|part| part.parse::<f64>().ok())
        .find(|value| (0.0..=1.0).contains(value))
        .or_else(|| {
            text.split('%')
                .next()
                .and_then(|head| {
                    head.rsplit(|c: char| !c.is_ascii_digit() && c != '.')
                        .next()
                })
                .and_then(|part| part.trim().parse::<f64>().ok())
                .map(|value| value / 100.0)
                .filter(|value| (0.0..=1.0).contains(value))
        });
    (answer.to_string(), confidence)
}

/// 解析视觉模型返回的边界框：`BOX x,y,w,h`、`x,y,w,h` 或 JSON `{"box":[x,y,w,h]}`。
pub fn parse_vision_box(text: &str) -> Option<(i32, i32, i32, i32)> {
    let normalized = text.replace(['(', ')', '[', ']', '“', '”', '"'], " ");
    if let Some(start) = normalized.find("BOX") {
        let rest = &normalized[start + 3..];
        let numbers: Vec<i32> = rest
            .split(|c: char| !c.is_ascii_digit() && c != '-')
            .filter_map(|part| part.parse::<i32>().ok())
            .collect();
        if numbers.len() >= 4 {
            return Some((numbers[0], numbers[1], numbers[2], numbers[3]));
        }
    }
    if let Some(start) = normalized.find("box") {
        let rest = &normalized[start + 3..];
        let numbers: Vec<i32> = rest
            .split(|c: char| !c.is_ascii_digit() && c != '-')
            .filter_map(|part| part.parse::<i32>().ok())
            .collect();
        if numbers.len() >= 4 {
            return Some((numbers[0], numbers[1], numbers[2], numbers[3]));
        }
    }
    // 裸四元组（前面没有 BOX 关键字）
    let numbers: Vec<i32> = normalized
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter_map(|part| part.parse::<i32>().ok())
        .collect();
    if numbers.len() == 4 {
        return Some((numbers[0], numbers[1], numbers[2], numbers[3]));
    }
    None
}

/// 交叉验证：OCR 行中心是否落在视觉框内（grounding 必须与 OCR 文本重合才允许点击）。
pub fn cross_validate_box(
    r#box: &(i32, i32, i32, i32),
    lines: &[crate::ocr::OcrLine],
) -> Option<serde_json::Value> {
    let (bx, by, bw, bh) = *r#box;
    for line in lines {
        let cx = line.x + line.width / 2;
        let cy = line.y + line.height / 2;
        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
            return Some(serde_json::json!({
                "text": line.text,
                "x": line.x,
                "y": line.y,
                "width": line.width,
                "height": line.height,
            }));
        }
    }
    None
}

/// 视觉 grounding（兜底定位）：视觉模型给出元素框 → 与 OCR 文本行交叉验证。
pub async fn ground_element(description: &str) -> Result<serde_json::Value, String> {
    let (bmp, surface) = capture_vision_bmp().await?;
    let png = bmp_to_png(&bmp)?;
    let prompt = format!(
        "截图中有没有满足以下描述的元素：{description}。如果有，只输出 BOX x,y,w,h \
         （四个整数：左上角 x、y，宽度 w、高度 h）；如果没有，只输出 NONE。"
    );
    let raw = describe_image(&png, &prompt).await?;
    let Some(r#box) = parse_vision_box(&raw) else {
        return Ok(serde_json::json!({
            "matched": false,
            "description": description,
            "reason": "视觉模型未给出坐标框",
            "raw": raw,
            "surface": surface,
        }));
    };
    let lines = current_ocr_lines(&bmp).await;
    if let Some(line) = cross_validate_box(&r#box, &lines) {
        Ok(serde_json::json!({
            "matched": true,
            "description": description,
            "box": r#box,
            "line": line,
            "cross_validated": true,
            "surface": surface,
        }))
    } else {
        Ok(serde_json::json!({
            "matched": false,
            "description": description,
            "box": r#box,
            "reason": "视觉框与 OCR 文本未重合，不允许点击",
            "surface": surface,
        }))
    }
}

/// 当前视觉面的 OCR 行：模拟面用真值版面（/ocr），真实桌面用 Media.Ocr。
async fn current_ocr_lines(bmp: &[u8]) -> Vec<crate::ocr::OcrLine> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        let base = std::env::var("OWO_SIM_QQ_URL").unwrap_or_default();
        let url = format!("{}/ocr", base.trim_end_matches('/'));
        if let Ok(response) = reqwest::get(&url).await {
            if let Ok(value) = response.json::<serde_json::Value>().await {
                if let Some(lines) = value.get("lines").and_then(serde_json::Value::as_array) {
                    let parsed: Vec<crate::ocr::OcrLine> = lines
                        .iter()
                        .filter_map(|line| {
                            Some(crate::ocr::OcrLine {
                                text: line.get("text")?.as_str()?.to_string(),
                                x: line.get("x")?.as_i64()? as i32,
                                y: line.get("y")?.as_i64()? as i32,
                                width: line.get("width")?.as_i64()? as i32,
                                height: line.get("height")?.as_i64()? as i32,
                            })
                        })
                        .collect();
                    if !parsed.is_empty() {
                        return parsed;
                    }
                }
            }
        }
    }
    crate::paddle_ocr::ocr_preferred(bmp)
        .await
        .map(|summary| crate::ocr::group_ocr_lines(&summary.boxes))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_to_png_produces_valid_png() {
        let mut bmp = vec![0u8; 54 + 4 * 4 * 4];
        bmp[0..2].copy_from_slice(b"BM");
        bmp[18..22].copy_from_slice(&4i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&4i32.to_le_bytes());
        bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
        let png = bmp_to_png(&bmp).expect("转换应成功");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(bmp_to_png(&[0u8; 10]).is_err());
    }

    #[test]
    fn parse_verification_extracts_yes_no_and_confidence() {
        let (answer, confidence) = parse_verification("YES (confidence 0.92)：消息已上屏");
        assert_eq!(answer, "yes");
        assert_eq!(confidence, Some(0.92));
        let (answer, confidence) = parse_verification("NO，输入框仍非空，置信度 85%");
        assert_eq!(answer, "no");
        assert_eq!(confidence, Some(0.85));
        let (answer, _) = parse_verification("不确定");
        assert_eq!(answer, "unknown");
    }

    #[test]
    fn parse_vision_box_accepts_box_keyword_and_json() {
        assert_eq!(
            parse_vision_box("BOX 815,624,170,36"),
            Some((815, 624, 170, 36))
        );
        assert_eq!(
            parse_vision_box("结果是 BOX 100 200 50 30"),
            Some((100, 200, 50, 30))
        );
        assert_eq!(
            parse_vision_box(r#"{"box":[10,20,30,40]}"#),
            Some((10, 20, 30, 40))
        );
        assert_eq!(parse_vision_box("NONE"), None);
    }

    #[test]
    fn cross_validate_box_matches_line_center_inside() {
        use crate::ocr::OcrLine;
        let lines = vec![
            OcrLine {
                text: "发送".into(),
                x: 815,
                y: 624,
                width: 170,
                height: 36,
            },
            OcrLine {
                text: "输入消息...".into(),
                x: 240,
                y: 620,
                width: 560,
                height: 44,
            },
        ];
        assert!(cross_validate_box(&(815, 624, 170, 36), &lines).is_some());
        assert!(cross_validate_box(&(0, 0, 100, 100), &lines).is_none());
    }
}
