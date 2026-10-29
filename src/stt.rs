//! Local Whisper model loading and explicit CPU/CUDA selection.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use whisper_rs::{WhisperContext, WhisperContextParameters, WhisperState};

fn selected(requested: &str, cuda_compiled: bool, cuda_available: bool) -> Result<bool> {
    match requested {
        "cpu" => Ok(false),
        "auto" => Ok(cuda_compiled && cuda_available),
        "cuda" => {
            ensure!(
                cuda_compiled && cuda_available,
                "CUDA requested but unavailable; build with --features cuda and check the NVIDIA driver, or use CAMERA_STT_DEVICE=auto/cpu"
            );
            Ok(true)
        }
        _ => anyhow::bail!("CAMERA_STT_DEVICE must be auto, cpu or cuda"),
    }
}
fn requested() -> String {
    std::env::var("CAMERA_STT_DEVICE").unwrap_or_else(|_| "auto".into())
}
fn cuda_available() -> bool {
    #[cfg(feature = "cuda")]
    {
        unsafe extern "C" {
            fn cudaGetDeviceCount(count: *mut i32) -> i32;
        }
        let mut count = 0;
        // SAFETY: cudart writes one integer to this live local variable.
        unsafe { cudaGetDeviceCount(&mut count) == 0 && count > 0 }
    }
    #[cfg(not(feature = "cuda"))]
    {
        false
    }
}
fn decode_beam(value: Option<&str>, gpu: bool) -> Result<i32> {
    let beam = value
        .map(str::parse::<i32>)
        .transpose()
        .context("Invalid CAMERA_STT_BEAM_SIZE")?
        .unwrap_or(if gpu { 5 } else { 1 });
    ensure!(
        (1..=8).contains(&beam),
        "CAMERA_STT_BEAM_SIZE must be between 1 and 8"
    );
    Ok(beam)
}
pub fn validate_device() -> Result<()> {
    let request = requested();
    decode_beam(std::env::var("CAMERA_STT_BEAM_SIZE").ok().as_deref(), false)?;
    selected(
        &request,
        cfg!(feature = "cuda"),
        request != "cpu" && cuda_available(),
    )
    .map(|_| ())
}
pub fn model_path(root: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("CAMERA_STT_MODEL") {
        return PathBuf::from(path);
    }
    if requested() != "cpu"
        && cuda_available()
        && let Some(path) = std::env::var_os("CAMERA_STT_GPU_MODEL")
    {
        return PathBuf::from(path);
    }
    let turbo = root.join("models/ggml-large-v3-turbo.bin");
    if requested() != "cpu" && cuda_available() && turbo.is_file() {
        turbo
    } else {
        root.join("models/ggml-base-q5_1.bin")
    }
}
fn attempt(model: &Path, use_gpu: bool) -> Result<WhisperState> {
    let mut params = WhisperContextParameters::default();
    params.use_gpu(use_gpu);
    // Pascal has no tensor cores. Keep flash attention off until measured on it.
    params.flash_attn(false);
    Ok(WhisperContext::new_with_params(model, params)?.create_state()?)
}
pub fn load(model: &Path) -> Result<(WhisperState, Value)> {
    let requested = requested();
    let mut use_gpu = selected(
        &requested,
        cfg!(feature = "cuda"),
        requested != "cpu" && cuda_available(),
    )?;
    let mut fallback = None;
    let state = match attempt(model, use_gpu) {
        Ok(state) => state,
        Err(error) if use_gpu && requested == "auto" => {
            fallback = Some(format!(
                "CUDA model initialization failed: {error}; using CPU"
            ));
            use_gpu = false;
            attempt(model, false).context("CPU fallback model initialization failed")?
        }
        Err(error) => return Err(error.context("Local Whisper model initialization failed")),
    };
    if requested == "auto" && !use_gpu && fallback.is_none() {
        fallback = Some("CUDA unavailable or not compiled; using CPU".into());
    }
    let beam = decode_beam(
        std::env::var("CAMERA_STT_BEAM_SIZE").ok().as_deref(),
        use_gpu,
    )?;
    Ok((
        state,
        json!({"engine":"whisper.cpp","model":model.file_name().and_then(|n|n.to_str()),"model_bytes":std::fs::metadata(model)?.len(),"device_requested":requested,"device_selected":if use_gpu {"cuda"} else {"cpu"},"cuda_compiled":cfg!(feature="cuda"),"beam_size":beam,"fallback_reason":fallback}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn beam_defaults_and_bounds() {
        assert_eq!(decode_beam(None, true).unwrap(), 5);
        assert_eq!(decode_beam(None, false).unwrap(), 1);
        assert_eq!(decode_beam(Some("3"), true).unwrap(), 3);
        assert!(decode_beam(Some("0"), true).is_err());
        assert!(decode_beam(Some("9"), true).is_err());
        assert!(decode_beam(Some("NaN"), true).is_err());
    }
    #[test]
    fn explicit_cuda_fails_closed_while_auto_can_use_cpu() {
        assert!(!selected("cpu", true, true).unwrap());
        assert!(!selected("auto", false, false).unwrap());
        assert!(!selected("auto", true, false).unwrap());
        assert!(selected("auto", true, true).unwrap());
        assert!(selected("cuda", true, true).unwrap());
        assert!(selected("cuda", true, false).is_err());
        assert!(selected("cuda", false, true).is_err());
        assert!(selected("gpu", true, true).is_err());
    }
}
