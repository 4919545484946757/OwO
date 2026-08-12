//! 本地语音转写（v0.4 D20）：默认 SenseVoice-Small（sherpa-onnx，离线优先）。
//!
//! 模型目录：`<data>/models/stt/<settings.stt.model>/`（model.int8.onnx + tokens.txt），
//! 由 `scripts/download-stt-model.ps1` 下载；模型未就绪时返回明确错误，不静默降级云端。

use crate::settings::SttSettings;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SttOutcome {
    pub text: String,
    pub elapsed_ms: u64,
}

pub struct LocalStt {
    model_dir: PathBuf,
    engine: String,
}

impl LocalStt {
    pub fn new(settings: &SttSettings, data_root: &Path) -> Self {
        Self {
            model_dir: data_root.join("models").join("stt").join(&settings.model),
            engine: settings.model.clone(),
        }
    }

    /// 默认数据目录：`OWO_AGENT_DATA` 或 `%LOCALAPPDATA%\OwO\Agent`。
    pub fn default_local() -> Self {
        let data_root = std::env::var("OWO_AGENT_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let base = std::env::var("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."));
                base.join("OwO").join("Agent")
            });
        Self::new(&SttSettings::default(), &data_root)
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    pub fn engine(&self) -> &str {
        &self.engine
    }

    pub fn is_ready(&self) -> bool {
        self.model_dir.join("model.int8.onnx").exists()
            && self.model_dir.join("tokens.txt").exists()
    }

    /// 离线转写 WAV（16k PCM 单声道；SenseVoice-Small via sherpa-onnx）。
    pub fn transcribe_wav(&self, wav_path: &Path) -> Result<SttOutcome, String> {
        #[cfg(target_os = "windows")]
        {
            if !self.is_ready() {
                return Err(format!(
                    "本地 STT 模型未就绪：{}（运行 scripts/download-stt-model.ps1）",
                    self.model_dir.display()
                ));
            }
            let started = std::time::Instant::now();
            let path = wav_path
                .to_str()
                .ok_or_else(|| format!("WAV 路径非法：{}", wav_path.display()))?;
            let wave = sherpa_onnx::Wave::read(path).ok_or("读取 WAV 失败")?;
            let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
            config.model_config.sense_voice = sherpa_onnx::OfflineSenseVoiceModelConfig {
                model: Some(
                    self.model_dir
                        .join("model.int8.onnx")
                        .to_string_lossy()
                        .into_owned(),
                ),
                language: Some("auto".to_string()),
                use_itn: true,
            };
            config.model_config.tokens = Some(
                self.model_dir
                    .join("tokens.txt")
                    .to_string_lossy()
                    .into_owned(),
            );
            let recognizer =
                sherpa_onnx::OfflineRecognizer::create(&config).ok_or("创建识别器失败")?;
            let stream = recognizer.create_stream();
            stream.accept_waveform(wave.sample_rate(), wave.samples());
            recognizer.decode(&stream);
            let text = stream
                .get_result()
                .map(|result| result.text)
                .unwrap_or_default();
            if text.trim().is_empty() {
                return Err("未识别到语音".to_string());
            }
            Ok(SttOutcome {
                text,
                elapsed_ms: started.elapsed().as_millis() as u64,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (wav_path, self);
            Err("本地 STT 暂仅支持 Windows".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_absent_returns_clear_error() {
        let stt = LocalStt::new(
            &SttSettings::default(),
            Path::new("C:\\owo-nonexistent-data-root"),
        );
        assert!(!stt.is_ready());
        assert_eq!(stt.engine(), "SenseVoice-Small");
        let error = stt.transcribe_wav(Path::new("missing.wav")).unwrap_err();
        assert!(error.contains("模型未就绪"));
    }
}
