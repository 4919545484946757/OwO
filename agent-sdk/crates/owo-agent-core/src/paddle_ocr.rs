//! PP-OCRv6（PaddleOCR 云 API）接入：优先通道，失败自动回退 Media.Ocr。
//!
//! 设计文档 M-A 决策：主力路径 PaddleOCR（本地 ONNX 后续可替换为 RapidOCR）。
//! 凭据只经环境变量（PADDLE_OCR_TOKEN），云 OCR 受数据出境开关（OWO_CLOUD_ENABLED）约束。
//!
//! API 流程（PaddleOCR AI Studio）：
//!   POST /api/v2/ocr/jobs（multipart: model + optionalPayload + file）
//!   → 轮询 GET /jobs/{jobId} 至 done → 下载 resultUrl.jsonUrl（JSONL）→ 解析文本+坐标。

use crate::ocr::{OcrBox, OcrSummary};
use serde_json::Value;
use std::time::Duration;

const DEFAULT_API_URL: &str = "https://paddleocr.aistudio-app.com/api/v2/ocr/jobs";
const DEFAULT_MODEL: &str = "PP-OCRv6";

/// Paddle 云 OCR 是否启用：配置了 token 且数据出境开关未关闭。
pub fn paddle_enabled() -> bool {
    let token = std::env::var("PADDLE_OCR_TOKEN")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !token {
        return false;
    }
    !std::env::var("OWO_CLOUD_ENABLED")
        .map(|value| value.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

fn api_url() -> String {
    std::env::var("PADDLE_OCR_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

fn model_name() -> String {
    std::env::var("PADDLE_OCR_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

/// OCR 首选入口：Paddle 启用则先试 Paddle，失败回退 Media.Ocr。
pub async fn ocr_preferred(bmp: &[u8]) -> Result<OcrSummary, String> {
    if paddle_enabled() {
        match ocr_paddle(bmp).await {
            Ok(mut summary) => {
                summary.provider = Some("paddle-v6".to_string());
                return Ok(summary);
            }
            Err(paddle_error) => {
                if std::env::var("OWO_OCR_STRICT")
                    .map(|value| value.eq_ignore_ascii_case("paddle"))
                    .unwrap_or(false)
                {
                    return Err(format!("PaddleOCR 严格模式：{paddle_error}"));
                }
                let mut summary = crate::ocr::ocr_bmp_detailed(bmp).map_err(|media_error| {
                    format!("PaddleOCR({paddle_error}) 与 Media.Ocr({media_error}) 均失败")
                })?;
                summary.provider = Some("media".to_string());
                return Ok(summary);
            }
        }
    }
    let mut summary = crate::ocr::ocr_bmp_detailed(bmp)?;
    summary.provider = Some("media".to_string());
    Ok(summary)
}

/// 调用 PP-OCRv6 云 API 识别 BMP（内部转 PNG 上传，降低体积）。
pub async fn ocr_paddle(bmp: &[u8]) -> Result<OcrSummary, String> {
    let png = crate::vision::bmp_to_png(bmp)?;
    let token =
        std::env::var("PADDLE_OCR_TOKEN").map_err(|_| "未配置 PADDLE_OCR_TOKEN".to_string())?;
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(240));
    if let Ok(proxy) = std::env::var("PADDLE_OCR_PROXY") {
        if !proxy.trim().is_empty() {
            if let Ok(proxy) = reqwest::Proxy::all(&proxy) {
                builder = builder.proxy(proxy);
            }
        }
    }
    let client = builder
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;

    let job_url = api_url();
    let optional_payload = serde_json::json!({
        "useDocOrientationClassify": false,
        "useDocUnwarping": false,
        "useTextlineOrientation": false,
    });
    let form = reqwest::multipart::Form::new()
        .text("model", model_name())
        .text("optionalPayload", optional_payload.to_string())
        .part(
            "file",
            reqwest::multipart::Part::bytes(png)
                .file_name("frame.png")
                .mime_str("image/png")
                .map_err(|e| format!("构造 multipart 失败：{e}"))?,
        );
    let response = client
        .post(&job_url)
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("提交 OCR 任务失败：{e}"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|e| format!("OCR 任务响应解析失败：{e}"))?;
    if !status.is_success() {
        return Err(format!(
            "OCR 任务提交失败（HTTP {status}）：{}",
            value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("未知错误")
        ));
    }
    let job_id = value
        .get("data")
        .and_then(|data| data.get("jobId"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("OCR 任务响应缺少 jobId：{value}"))?
        .to_string();

    // 轮询任务状态
    let poll_url = format!("{job_url}/{job_id}");
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let jsonl_url: Option<String> = loop {
        if std::time::Instant::now() > deadline {
            return Err("OCR 任务轮询超时（180s）".to_string());
        }
        let state_value: Value = client
            .get(&poll_url)
            .bearer_auth(
                std::env::var("PADDLE_OCR_TOKEN")
                    .map_err(|_| "未配置 PADDLE_OCR_TOKEN".to_string())?,
            )
            .send()
            .await
            .map_err(|e| format!("查询 OCR 任务失败：{e}"))?
            .json()
            .await
            .map_err(|e| format!("OCR 任务状态解析失败：{e}"))?;
        let state = state_value
            .get("data")
            .and_then(|data| data.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        match state {
            "done" => {
                break Some(
                    state_value
                        .get("data")
                        .and_then(|data| data.get("resultUrl"))
                        .and_then(|result| result.get("jsonUrl"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| "OCR 任务完成但缺少 resultUrl.jsonUrl".to_string())?
                        .to_string(),
                );
            }
            "failed" => {
                let error = state_value
                    .get("data")
                    .and_then(|data| data.get("errorMsg"))
                    .and_then(Value::as_str)
                    .unwrap_or("未知错误");
                return Err(format!("OCR 任务失败：{error}"));
            }
            _ => {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    };

    // 下载 JSONL 结果
    let jsonl_url = jsonl_url.ok_or_else(|| "OCR 任务未完成".to_string())?;
    let jsonl = client
        .get(&jsonl_url)
        .send()
        .await
        .map_err(|e| format!("下载 OCR 结果失败：{e}"))?
        .text()
        .await
        .map_err(|e| format!("读取 OCR 结果失败：{e}"))?;
    parse_paddle_jsonl(&jsonl).map_err(|error| {
        if std::env::var("OWO_OCR_STRICT")
            .map(|value| value.eq_ignore_ascii_case("paddle"))
            .unwrap_or(false)
        {
            let preview: String = jsonl.chars().take(2000).collect();
            format!("{error}；原始 JSONL 前 2000 字符：{preview}")
        } else {
            error
        }
    })
}

/// 解析 PaddleOCR-X JSONL：每行 `{"result":{"ocrResults":[{texts, scores, polys, ocrImage}]}}`。
pub fn parse_paddle_jsonl(jsonl: &str) -> Result<OcrSummary, String> {
    let mut text_lines: Vec<String> = Vec::new();
    let mut boxes: Vec<OcrBox> = Vec::new();
    for (index, line) in jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|e| format!("JSONL 第 {} 行解析失败：{e}", index + 1))?;
        let pages = value
            .get("result")
            .and_then(|result| result.get("ocrResults"))
            .and_then(Value::as_array);
        let Some(pages) = pages else {
            continue;
        };
        for page in pages {
            let texts = find_texts(page);
            let polys = find_polys(page);
            for (text_index, text) in texts.iter().enumerate() {
                if text.trim().is_empty() {
                    continue;
                }
                text_lines.push(text.trim().to_string());
                let rect = polys
                    .get(text_index)
                    .and_then(poly_to_rect)
                    .unwrap_or((0, 0, 0, 0));
                boxes.push(OcrBox {
                    text: text.trim().to_string(),
                    x: rect.0,
                    y: rect.1,
                    width: rect.2,
                    height: rect.3,
                });
            }
        }
    }
    if text_lines.is_empty() {
        return Err("PaddleOCR 结果为空".to_string());
    }
    let text = text_lines.join("\n");
    Ok(OcrSummary {
        chars: text.chars().count(),
        text,
        boxes,
        provider: Some("paddle-v6".to_string()),
    })
}

fn find_texts(page: &Value) -> Vec<String> {
    let mut candidates: Vec<&Value> = Vec::new();
    if let Some(pruned) = page.get("prunedResult") {
        candidates.push(pruned);
    }
    candidates.push(page);
    for candidate in candidates {
        for key in ["rec_texts", "texts", "text", "words"] {
            if let Some(list) = candidate.get(key).and_then(Value::as_array) {
                let values: Vec<String> = list
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect();
                if !values.is_empty() {
                    return values;
                }
            }
        }
    }
    Vec::new()
}

fn find_polys(page: &Value) -> Vec<Value> {
    let mut candidates: Vec<&Value> = Vec::new();
    if let Some(pruned) = page.get("prunedResult") {
        candidates.push(pruned);
    }
    candidates.push(page);
    for candidate in candidates {
        for key in ["rec_polys", "polys", "dt_polys", "boxes", "polygons", "box"] {
            if let Some(list) = candidate.get(key).and_then(Value::as_array) {
                return list.clone();
            }
        }
    }
    Vec::new()
}

/// 多边形/矩形 → (x, y, w, h)。
fn poly_to_rect(poly: &Value) -> Option<(i32, i32, i32, i32)> {
    let points = poly.as_array()?;
    if points.is_empty() {
        return None;
    }
    // 支持 [[x,y],[x,y],...] 与 [x,y,w,h]
    if points.len() == 4 && points.iter().all(|p| p.is_number()) {
        let x = points[0].as_i64()? as i32;
        let y = points[1].as_i64()? as i32;
        let w = points[2].as_i64()? as i32;
        let h = points[3].as_i64()? as i32;
        return Some((x, y, w, h));
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for point in points {
        let pair = point.as_array()?;
        if pair.len() < 2 {
            return None;
        }
        let x = pair[0].as_i64()? as i32;
        let y = pair[1].as_i64()? as i32;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    Some((min_x, min_y, max_x - min_x, max_y - min_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_paddle_jsonl_extracts_texts_and_boxes() {
        let jsonl = r#"{"result":{"ocrResults":[{"texts":["发送","输入消息..."],"scores":[0.98,0.95],"polys":[[[815,624],[985,624],[985,660],[815,660]],[[240,620],[800,620],[800,664],[240,664]]]}]}}
{"result":{"ocrResults":[{"texts":["在吗？"],"boxes":[[10,20,100,30]]}]}}"#;
        let summary = parse_paddle_jsonl(jsonl).expect("解析成功");
        assert_eq!(summary.text, "发送\n输入消息...\n在吗？");
        assert_eq!(summary.chars, summary.text.chars().count());
        assert_eq!(summary.boxes[0].x, 815);
        assert_eq!(summary.boxes[0].width, 170);
        assert_eq!(summary.boxes[2].x, 10);
        assert_eq!(summary.boxes[2].height, 30);
        assert_eq!(summary.provider.as_deref(), Some("paddle-v6"));
    }

    #[test]
    fn paddle_enabled_requires_token_and_cloud_switch() {
        std::env::remove_var("PADDLE_OCR_TOKEN");
        std::env::remove_var("OWO_CLOUD_ENABLED");
        assert!(!paddle_enabled());
        std::env::set_var("PADDLE_OCR_TOKEN", "test");
        std::env::set_var("OWO_CLOUD_ENABLED", "false");
        assert!(!paddle_enabled());
        std::env::set_var("OWO_CLOUD_ENABLED", "true");
        assert!(paddle_enabled());
        std::env::remove_var("PADDLE_OCR_TOKEN");
        std::env::remove_var("OWO_CLOUD_ENABLED");
    }

    #[test]
    fn parse_paddle_jsonl_handles_pruned_result_schema() {
        let jsonl = r#"{"logId":"x","result":{"ocrResults":[{"prunedResult":{"rec_texts":["输入消息...","发送"],"rec_polys":[[[240,620],[800,620],[800,664],[240,664]],[[815,624],[985,624],[985,660],[815,660]]]}}]}}"#;
        let summary = parse_paddle_jsonl(jsonl).expect("解析成功");
        assert_eq!(summary.text, "输入消息...\n发送");
        assert_eq!(summary.boxes[1].x, 815);
        assert_eq!(summary.boxes[1].width, 170);
    }
}
