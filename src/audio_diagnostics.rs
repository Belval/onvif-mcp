//! Opt-in, bounded retention of the exact resampled utterances given to ASR/KWS.
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct Recorder {
    pub dir: PathBuf,
    next: u64,
}
impl Recorder {
    pub fn create(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let dir = root.join(format!("hearing-{:016x}", rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { dir, next: 0 })
    }
    pub fn save(&mut self, samples: &[i16], start_ms: u64, end_ms: u64) -> Result<PathBuf> {
        let slot = self.next % 8;
        self.next += 1;
        let path = self.dir.join(format!("utterance-{slot}.pcm"));
        let temp = path.with_extension("tmp");
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&temp, data)?;
        std::fs::rename(temp, &path)?;
        let info = serde_json::json!({"sequence":self.next,"sample_rate_hz":16000,"channels":1,"encoding":"PCM16 little endian","started_at_unix_ms":start_ms,"ended_at_unix_ms":end_ms,"samples":samples.len()});
        std::fs::write(path.with_extension("json"), serde_json::to_vec(&info)?)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retention_is_bounded_and_preserves_exact_signed_samples() {
        let mut recorder = Recorder::create(&std::env::temp_dir()).unwrap();
        for i in 0..17 {
            recorder.save(&[i, -32768, 32767], 1, 2).unwrap();
        }
        assert_eq!(std::fs::read_dir(&recorder.dir).unwrap().count(), 16);
        assert_eq!(
            std::fs::read(recorder.dir.join("utterance-0.pcm")).unwrap(),
            [16, 0, 0, 128, 255, 127]
        );
        std::fs::remove_dir_all(recorder.dir).unwrap();
    }
}
