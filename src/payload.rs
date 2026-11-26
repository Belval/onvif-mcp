//! Standard padded base64 for MCP images and audio, with SIMD dispatch.
pub fn encode(bytes: &[u8]) -> String {
    base64_simd::STANDARD.encode_to_string(bytes)
}

/// Camera-free scalar/SIMD control, including allocation of the returned String.
pub fn benchmark() -> anyhow::Result<()> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::{hint::black_box, time::Instant};
    let mut blocks = Vec::new();
    for size in [32 * 1024, 128 * 1024, 512 * 1024] {
        let bytes: Vec<u8> = (0..size).map(|i| (i * 71 + i / 17) as u8).collect();
        let expected = STANDARD.encode(&bytes);
        for (block, simd) in [false, true, true, false, false, true]
            .into_iter()
            .enumerate()
        {
            let mut samples = Vec::new();
            for iteration in 0..103 {
                let began = Instant::now();
                let output = if simd {
                    encode(black_box(&bytes))
                } else {
                    STANDARD.encode(black_box(&bytes))
                };
                let elapsed = began.elapsed().as_secs_f64() * 1000.;
                anyhow::ensure!(
                    output == expected,
                    "Base64 output differs from the standard encoder"
                );
                if iteration >= 3 {
                    samples.push(elapsed);
                }
            }
            let mut sorted = samples.clone();
            sorted.sort_by(f64::total_cmp);
            blocks.push(serde_json::json!({"size_bytes":size,"block":block+1,"variant":if simd{"simd"}else{"scalar"},"n":samples.len(),"median_ms":(sorted[49]+sorted[50])/2.,"samples_ms":samples}));
        }
    }
    println!(
        "{}",
        serde_json::json!({"protocol":"Same binary, scalar/SIMD/SIMD/scalar/scalar/SIMD per size; 100 encodes/block after three warmups; allocation timed; byte equality checked after timer", "results":blocks})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    #[test]
    fn standard_padding_and_bytes_match_for_tails_and_large_payloads() {
        for size in (0..1024).chain([32 * 1024, 128 * 1024 + 1, 512 * 1024 + 2]) {
            let input: Vec<u8> = (0..size).map(|i| (i * 71 + i / 17) as u8).collect();
            let encoded = encode(&input);
            assert_eq!(encoded, STANDARD.encode(&input), "length {size}");
            assert_eq!(STANDARD.decode(encoded).unwrap(), input);
        }
    }
}
