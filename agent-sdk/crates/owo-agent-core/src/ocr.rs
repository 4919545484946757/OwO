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

/// 对内存 BMP 做本地 OCR，返回文字摘要（无文字时返回 None）。
#[cfg(target_os = "windows")]
pub fn ocr_bmp(bmp: &[u8]) -> Option<OcrSummary> {
    use futures::executor::block_on;
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

    block_on(async move {
        let stream = InMemoryRandomAccessStream::new().ok()?;
        let writer = DataWriter::CreateDataWriter(&stream).ok()?;
        writer.WriteBytes(bmp).ok()?;
        writer.FlushAsync().ok()?.await.ok()?;
        stream.Seek(0).ok()?;

        let decoder = BitmapDecoder::CreateAsync(&stream).ok()?.await.ok()?;
        let software = decoder.GetSoftwareBitmapAsync().ok()?.await.ok()?;
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().ok()?;
        if OcrEngine::MaxImageDimension()
            .map(|dim| dim == 0)
            .unwrap_or(true)
        {
            return None; // 系统未安装 OCR 语言包
        }
        let result = engine.RecognizeAsync(&software).ok()?.await.ok()?;
        let text = result.Text().ok()?.to_string();
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
            None
        } else {
            Some(OcrSummary {
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
