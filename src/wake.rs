//! Acoustic keyword verification, independent of the generative transcriber.
use anyhow::{Context, Result, ensure};
use sentencepiece_rs::SentencePieceProcessor;
use sherpa_onnx::{KeywordSpotter, KeywordSpotterConfig};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const STEM: &str = "epoch-12-avg-2-chunk-16-left-64";
pub fn model_dir(root: &Path) -> PathBuf {
    std::env::var_os("CAMERA_WAKE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01"))
}

pub struct Settings {
    pub phrase: Option<String>,
    pub aliases: Vec<String>,
    pub model_dir: PathBuf,
}

pub struct Detector {
    kws: KeywordSpotter,
    pub threshold: f32,
}
impl Detector {
    pub fn load(dir: &Path, phrases: &[String]) -> Result<Self> {
        let file = |name: &str| -> Result<String> {
            let path = dir.join(name);
            ensure!(
                path.is_file(),
                "Wake model missing {}; install with scripts/install-wake-model.sh or set CAMERA_WAKE_MODEL_DIR",
                name
            );
            let path = path.to_str().context("Wake model path must be UTF-8")?;
            ensure!(!path.contains('\0'), "Invalid wake model path");
            Ok(path.to_owned())
        };
        let tokenizer = SentencePieceProcessor::open(file("bpe.model")?)
            .context("Cannot load wake tokenizer")?;
        let tokens_path = file("tokens.txt")?;
        let tokens: HashSet<String> = std::fs::read_to_string(&tokens_path)?
            .lines()
            .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
            .collect();
        ensure!(!phrases.is_empty(), "At least one wake phrase is required");
        let mut keywords = String::new();
        for phrase in phrases {
            let normalized = phrase
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
                .to_uppercase();
            let pieces = tokenizer
                .encode(&normalized)
                .context("Cannot tokenize wake phrase")?;
            ensure!(
                !pieces.is_empty() && pieces.iter().all(|p| p != "<unk>" && tokens.contains(p)),
                "Wake phrase cannot be represented by the installed English keyword model"
            );
            keywords.push_str(&format!("{} @wake\n", pieces.join(" ")));
        }
        let mut config = KeywordSpotterConfig::default();
        config.model_config.transducer.encoder = Some(file(&format!("encoder-{STEM}.int8.onnx"))?);
        config.model_config.transducer.decoder = Some(file(&format!("decoder-{STEM}.onnx"))?);
        config.model_config.transducer.joiner = Some(file(&format!("joiner-{STEM}.int8.onnx"))?);
        config.model_config.tokens = Some(tokens_path);
        config.model_config.num_threads = 1;
        config.model_config.provider = Some("cpu".into());
        // Keep the upstream keyword score. Require stronger acoustic evidence than its
        // 0.25 default; transcript matching is a separate, additional requirement.
        config.keywords_score = 1.0;
        config.keywords_threshold = std::env::var("CAMERA_WAKE_THRESHOLD")
            .ok()
            .map(|v| v.parse::<f32>())
            .transpose()
            .context("Invalid CAMERA_WAKE_THRESHOLD")?
            .unwrap_or(0.35);
        ensure!(
            config.keywords_threshold.is_finite()
                && (0.1..=1.).contains(&config.keywords_threshold),
            "CAMERA_WAKE_THRESHOLD must be between 0.1 and 1"
        );
        config.num_trailing_blanks = 2;
        config.keywords_buf = Some(keywords);
        let kws =
            KeywordSpotter::create(&config).context("Cannot initialize acoustic wake detector")?;
        Ok(Self {
            kws,
            threshold: config.keywords_threshold,
        })
    }
    pub fn detects(&self, samples: &[i16], cancelled: &AtomicBool) -> bool {
        // A fresh stream prevents acoustic evidence from carrying into later
        // utterances or across lost audio. Tail silence flushes the final chunk.
        let stream = self.kws.create_stream();
        for chunk in samples.chunks(1600) {
            if cancelled.load(Ordering::SeqCst) {
                return false;
            }
            let pcm: Vec<f32> = chunk.iter().map(|s| *s as f32 / 32768.).collect();
            stream.accept_waveform(16000, &pcm);
            while self.kws.is_ready(&stream) {
                self.kws.decode(&stream);
                if self
                    .kws
                    .get_result(&stream)
                    .is_some_and(|r| r.keyword == "wake")
                {
                    return true;
                }
            }
        }
        stream.accept_waveform(16000, &[0.; 8000]);
        stream.input_finished();
        while self.kws.is_ready(&stream) {
            if cancelled.load(Ordering::SeqCst) {
                return false;
            }
            self.kws.decode(&stream);
            if self
                .kws
                .get_result(&stream)
                .is_some_and(|r| r.keyword == "wake")
            {
                return true;
            }
        }
        false
    }
}

pub fn verified(transcript_match: bool, acoustic_match: bool) -> bool {
    transcript_match && acoustic_match
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hallucinated_transcript_cannot_activate_a_request() {
        assert!(!verified(true, false));
        assert!(!verified(false, true));
        assert!(!verified(false, false));
        assert!(verified(true, true));
    }
    #[test]
    #[ignore = "requires installed acoustic model and CAMERA_WAKE_TEST_PCM"]
    fn local_acoustic_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let phrase = std::env::var("CAMERA_WAKE_TEST_PHRASE").unwrap_or_else(|_| "Hey Remi".into());
        let detector = Detector::load(&model_dir(root), &[phrase]).unwrap();
        let data = std::fs::read(std::env::var("CAMERA_WAKE_TEST_PCM").unwrap()).unwrap();
        let samples: Vec<i16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        let start = std::time::Instant::now();
        let actual = detector.detects(&samples, &AtomicBool::new(false));
        eprintln!(
            "Acoustic detection={actual}, elapsed_ms={}",
            start.elapsed().as_millis()
        );
        assert_eq!(
            verified(true, actual),
            std::env::var("CAMERA_WAKE_TEST_EXPECT").as_deref() == Ok("true")
        );
    }
}
