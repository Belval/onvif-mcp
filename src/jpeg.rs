//! Tapo C210 native JPEG/RTP assembly without decoding or recompression.
//!
//! This firmware advertises RFC 2435 Q=90/type=0, but sends an in-band JPEG
//! header tail: 63 luma quantizers followed by DQT/SOF/DHT/SOS and entropy data.
//! Restore the missing SOI/DQT prefix and DC quantizer, inferring the latter
//! only when ALL 63 available coefficients match a standard JPEG quality table.
//! Generic RFC 2435 header synthesis otherwise creates duplicate/conflicting headers.
use anyhow::{Result, ensure};

#[rustfmt::skip]
const LUMA:[u32;64]=[
16,11,10,16,24,40,51,61,12,12,14,19,26,58,60,55,
14,13,16,24,40,57,69,56,14,17,22,29,51,87,80,62,
18,22,37,56,68,109,103,77,24,35,55,64,81,104,113,92,
49,64,78,87,103,121,120,101,72,92,95,98,112,100,103,99];
#[rustfmt::skip]
const ZIGZAG:[usize;64]=[
0,1,8,16,9,2,3,10,17,24,32,25,18,11,4,5,
12,19,26,33,40,48,41,34,27,20,13,6,7,14,21,28,
35,42,49,56,57,50,43,36,29,22,15,23,30,37,44,51,
58,59,52,45,38,31,39,46,53,60,61,54,47,55,62,63];
fn prefix(data: &[u8]) -> Result<Vec<u8>> {
    crate::metrics::sync_result("jpeg.header_repair", || {
        if data.starts_with(&[0xff, 0xd8]) {
            return Ok(vec![]);
        }
        ensure!(
            data.len() >= 68 && data[63..68] == [0xff, 0xdb, 0, 0x43, 1],
            "Unsupported camera JPEG header layout"
        );
        for quality in 1..=100 {
            let scale = if quality < 50 {
                5000 / quality
            } else {
                200 - 2 * quality
            };
            let quant =
                |index: usize| ((LUMA[ZIGZAG[index]] * scale + 50) / 100).clamp(1, 255) as u8;
            if (1..64).all(|i| data[i - 1] == quant(i)) {
                return Ok(vec![0xff, 0xd8, 0xff, 0xdb, 0, 0x43, 0, quant(0)]);
            }
        }
        anyhow::bail!("Unknown camera JPEG quantization table; refusing to guess image data")
    })
}
#[derive(Default)]
pub struct NativeJpeg {
    timestamp: Option<i64>,
    next_offset: usize,
    next_sequence: u16,
    data: Vec<u8>,
}
impl NativeJpeg {
    pub fn push(
        &mut self,
        timestamp: i64,
        sequence: u16,
        marker: bool,
        payload: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        crate::metrics::sync_result("jpeg.packet_assembly", || {
            ensure!(payload.len() >= 8, "Short camera RTP/JPEG packet");
            let offset =
                ((payload[1] as usize) << 16) | ((payload[2] as usize) << 8) | payload[3] as usize;
            let body = &payload[8..];
            if offset == 0 {
                crate::metrics::counter("jpeg.frames_started", 1);
                if self.timestamp.is_some() {
                    crate::metrics::counter("jpeg.incomplete_frames_replaced", 1);
                }
                self.timestamp = Some(timestamp);
                self.next_offset = 0;
                self.next_sequence = sequence;
                self.data = Vec::with_capacity(64 * 1024);
                self.data.extend(prefix(body)?);
            }
            if self.timestamp != Some(timestamp)
                || self.next_offset != offset
                || self.next_sequence != sequence
            {
                self.timestamp = None;
                self.data.clear();
                crate::metrics::counter("jpeg.fragments_discarded", 1);
                return Ok(None);
            }
            ensure!(
                self.data.len() + body.len() <= 4 * 1024 * 1024,
                "Camera JPEG exceeds size limit"
            );
            self.data.extend_from_slice(body);
            self.next_offset += body.len();
            self.next_sequence = sequence.wrapping_add(1);
            if marker {
                crate::metrics::counter("jpeg.frames_completed", 1);
                ensure!(
                    self.data.ends_with(&[0xff, 0xd9]),
                    "Incomplete camera JPEG frame"
                );
                self.timestamp = None;
                return Ok(Some(std::mem::take(&mut self.data)));
            }
            Ok(None)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, codecs::jpeg::JpegEncoder};
    fn fixture() -> Vec<u8> {
        let image = RgbImage::from_fn(640, 360, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 100])
        });
        let mut jpeg = vec![];
        JpegEncoder::new_with_quality(&mut jpeg, 75)
            .encode_image(&image)
            .unwrap();
        // Reorder the encoder's valid header segments into this camera's DQT-first order.
        let mut quantizers = vec![];
        let mut headers = vec![];
        let mut offset = 2;
        while jpeg[offset + 1] != 0xda {
            let marker = jpeg[offset + 1];
            let length = u16::from_be_bytes([jpeg[offset + 2], jpeg[offset + 3]]) as usize;
            let segment = &jpeg[offset..offset + 2 + length];
            if marker == 0xdb {
                quantizers.extend_from_slice(segment);
            } else if !(0xe0..=0xef).contains(&marker) {
                headers.extend_from_slice(segment);
            }
            offset += 2 + length;
        }
        let mut reordered = vec![0xff, 0xd8];
        reordered.extend(quantizers);
        reordered.extend(headers);
        reordered.extend_from_slice(&jpeg[offset..]);
        reordered[8..].to_vec() // Camera omits SOI/DQT plus the first quantizer.
    }
    fn packet(body: &[u8], offset: usize) -> Vec<u8> {
        let mut p = vec![
            0,
            ((offset >> 16) & 255) as u8,
            ((offset >> 8) & 255) as u8,
            (offset & 255) as u8,
            0,
            90,
            80,
            45,
        ];
        p.extend_from_slice(body);
        p
    }
    #[test]
    fn reconstructs_native_jpeg_losslessly() {
        let body = fixture();
        let mut assembler = NativeJpeg::default();
        let mut offset = 0;
        let mut sequence = 65530u16;
        let mut decoded = None;
        for chunk in body.chunks(200) {
            let last = offset + chunk.len() == body.len();
            decoded = assembler
                .push(123, sequence, last, &packet(chunk, offset))
                .unwrap();
            offset += chunk.len();
            sequence = sequence.wrapping_add(1);
        }
        let jpeg = decoded.unwrap();
        let image = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((image.width(), image.height()), (640, 360));
        assert_eq!(&jpeg[8..], body); // Compressed pixels and original remaining headers are untouched.
    }
    #[test]
    fn missing_fragment_does_not_publish_corrupt_frame() {
        let body = fixture();
        let mut assembler = NativeJpeg::default();
        assert!(
            assembler
                .push(123, 1, false, &packet(&body[..200], 0))
                .unwrap()
                .is_none()
        );
        assert!(
            assembler
                .push(123, 3, true, &packet(&body[400..], 400))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn refuses_unknown_quantization_instead_of_guessing() {
        let mut body = fixture();
        body[0] = 255;
        assert!(prefix(&body).is_err());
    }
}
