//! Byte-preserving stdio timing. Only a small response prefix is inspected;
//! image/base64 payloads are never parsed or retained by this adapter.
use crate::metrics;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
const INPUT_LIMIT: usize = 64 * 1024;
const PREFIX_LIMIT: usize = 1024;
pub struct Read<R> {
    inner: R,
    line: Vec<u8>,
    started: Option<Instant>,
    oversize: bool,
}
pub struct Write<W> {
    inner: W,
    prefix: Vec<u8>,
    started: Option<Instant>,
    key: Option<String>,
    bytes: u64,
}
pub fn instrument<R, W>((read, write): (R, W)) -> (Read<R>, Write<W>) {
    (
        Read {
            inner: read,
            line: vec![],
            started: None,
            oversize: false,
        },
        Write {
            inner: write,
            prefix: vec![],
            started: None,
            key: None,
            bytes: 0,
        },
    )
}
impl<R: AsyncRead + Unpin> AsyncRead for Read<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !metrics::enabled() {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }
        let before = buf.filled().len();
        let began = Instant::now();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        metrics::observe("transport.read_poll", began.elapsed());
        if let Poll::Ready(Ok(())) = &result {
            let data = &buf.filled()[before..];
            for chunk in data.split_inclusive(|b| *b == b'\n') {
                let started = *this.started.get_or_insert_with(Instant::now);
                if !this.oversize && this.line.len() + chunk.len() <= INPUT_LIMIT {
                    this.line.extend_from_slice(chunk);
                } else {
                    this.oversize = true;
                }
                if chunk.last() == Some(&b'\n') {
                    if !this.oversize {
                        metrics::inbound(&this.line, started, Instant::now());
                    } else {
                        metrics::counter("transport.oversize_untraced_input", 1);
                    }
                    this.line.clear();
                    this.started = None;
                    this.oversize = false;
                }
            }
        }
        result
    }
}
fn prefix_id(prefix: &[u8]) -> Option<String> {
    // SDK serializes the top-level JSON-RPC id before result/error. Do not search the body.
    let marker = b"\"id\"";
    let position = prefix.windows(marker.len()).position(|w| w == marker)?;
    let after = &prefix[position + marker.len()..];
    let colon = after.iter().position(|b| *b == b':')?;
    let mut stream =
        serde_json::Deserializer::from_slice(&after[colon + 1..]).into_iter::<serde_json::Value>();
    let id = stream.next()?.ok()?;
    // A numeric token can parse successfully before the next write completes it.
    let delimiter = after[colon + 1 + stream.byte_offset()..]
        .iter()
        .find(|b| !b.is_ascii_whitespace());
    if !matches!(delimiter, Some(b',' | b'}')) {
        return None;
    }
    if id.is_string() || id.is_number() || id.is_null() {
        let key = id.to_string();
        (key.len() <= 256).then_some(key)
    } else {
        None
    }
}
impl<W: AsyncWrite + Unpin> AsyncWrite for Write<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if !metrics::enabled() {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        }
        let began = *this.started.get_or_insert_with(Instant::now);
        let polled = Instant::now();
        let result = Pin::new(&mut this.inner).poll_write(cx, buf);
        metrics::observe("transport.write_poll", polled.elapsed());
        if let Poll::Ready(Ok(n)) = &result {
            for chunk in buf[..*n].split_inclusive(|b| *b == b'\n') {
                let started = *this.started.get_or_insert(began);
                this.bytes += chunk.len() as u64;
                let remaining = PREFIX_LIMIT.saturating_sub(this.prefix.len());
                this.prefix
                    .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                if this.key.is_none() {
                    this.key = prefix_id(&this.prefix);
                }
                if chunk.last() == Some(&b'\n') {
                    metrics::counter("transport.response_bytes", this.bytes);
                    if let Some(key) = &this.key {
                        metrics::outbound_finished(key, started, Instant::now(), this.bytes);
                    } else {
                        metrics::counter("transport.unmatched_output", 1);
                    }
                    this.prefix.clear();
                    this.started = None;
                    this.key = None;
                    this.bytes = 0;
                }
            }
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[test]
    fn response_id_is_found_without_decoding_payload() {
        assert_eq!(
            prefix_id(br#"{"jsonrpc":"2.0","id":42,"result":{"content":[{"data":""#),
            Some("42".into())
        );
        assert_eq!(prefix_id(br#"{"id":"split"#), None);
        assert_eq!(prefix_id(br#"{"id":83"#), None);
        assert_eq!(prefix_id(br#"{"id":8341,"#), Some("8341".into()));
    }
    #[tokio::test]
    async fn adapters_preserve_fragmented_bytes_and_finalize_trace() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let (r, w) = tokio::io::split(a);
        let (mut r, mut w) = instrument((r, w));
        let (mut br, mut bw) = tokio::io::split(b);
        let request =
            b"{\"id\":8341,\"method\":\"tools/call\",\"params\":{\"name\":\"camera_snapshot\"}}\n";
        bw.write_all(&request[..17]).await.unwrap();
        let mut first = vec![0; 17];
        r.read_exact(&mut first).await.unwrap();
        bw.write_all(&request[17..]).await.unwrap();
        let mut rest = vec![0; request.len() - 17];
        r.read_exact(&mut rest).await.unwrap();
        first.extend(rest);
        assert_eq!(first, request);
        let trace = metrics::trace_for_response("8341").unwrap();
        let response = b"{\"id\":8341,\"result\":{\"content\":[]}}\n";
        w.write_all(&response[..9]).await.unwrap();
        w.write_all(&response[9..]).await.unwrap();
        let mut observed = vec![0; response.len()];
        br.read_exact(&mut observed).await.unwrap();
        assert_eq!(observed, response);
        let value = metrics::snapshot(Some(&trace.id), true, 1);
        assert_eq!(value["response_complete"], true);
        assert_eq!(
            value["response_bytes"].as_u64(),
            Some(response.len() as u64)
        );
    }
}
