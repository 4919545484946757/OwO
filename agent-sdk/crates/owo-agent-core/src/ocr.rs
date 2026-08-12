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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrEngineStatus {
    pub engine_created: bool,
    pub max_image_dimension: u32,
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
    }
}

#[cfg(not(target_os = "windows"))]
pub fn ocr_engine_status() -> OcrEngineStatus {
    OcrEngineStatus {
        engine_created: false,
        max_image_dimension: 0,
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
            })
        }
    })
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
}
