//! Length-prefixed JSON frame codec for the Rust ↔ Python sidecar pipe.
//!
//! WP-W2-04 §"Stdio framing":
//!
//! ```text
//! +---------+-------------------+
//! | 4 bytes | UTF-8 JSON body   |
//! | u32 BE  | (length bytes)    |
//! +---------+-------------------+
//! ```
//!
//! Symmetric with `src-tauri/sidecar/agent_runtime/agent_runtime/framing.py`.
//! Both sides cap a single frame at 16 MiB so a corrupted length
//! prefix can never trigger a multi-gigabyte allocation.
//!
//! The codec is deliberately small: two async functions
//! (`write_frame`, `read_frame`) operating on raw bytes. Higher-level
//! encode/decode is the caller's responsibility — `agent.rs` does
//! `serde_json::to_vec(...)` before `write_frame`, and
//! `serde_json::from_slice(...)` after.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Hard cap on a single frame body. 16 MiB is far more than any
/// realistic span / payload we emit — guards against a desync that
/// would otherwise allocate gigabytes from a corrupted length prefix.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Codec errors. Distinct from `AppError::Sidecar` so the supervisor
/// can decide locally whether a particular kind of frame error is
/// fatal (peer hung up on a half-frame) or recoverable (one bad JSON
/// payload, keep reading).
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame body too large: {0} > {max}", max = MAX_FRAME_BYTES)]
    TooLarge(usize),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Outcome of a single `read_frame` call. `Eof` is a clean shutdown
/// signal (peer closed between frames); `TooLarge` and `Io` keep the
/// underlying error for the supervisor's logs.
#[derive(Debug)]
pub enum Frame {
    Body(Vec<u8>),
    Eof,
}

/// Write one length-prefixed frame to `w` and flush so the peer's
/// blocking `read_exact(4)` cannot deadlock waiting for buffered bytes.
pub async fn write_frame<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    body: &[u8],
) -> Result<(), FrameError> {
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(body.len()));
    }
    // `to_be_bytes()` produces a 4-byte big-endian unsigned integer,
    // matching Python's `struct.pack("!I", len)`.
    let len = (body.len() as u32).to_be_bytes();
    w.write_all(&len).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed frame from `r`. Returns `Frame::Eof` if
/// the peer closed cleanly between frames; otherwise yields a body
/// `Vec<u8>` or surfaces an I/O / oversize error.
pub async fn read_frame<R: AsyncReadExt + Unpin>(
    r: &mut R,
) -> Result<Frame, FrameError> {
    let mut header = [0u8; 4];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(Frame::Eof);
        }
        Err(e) => return Err(FrameError::Io(e)),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut body).await?;
    }
    Ok(Frame::Body(body))
}

#[cfg(test)]
mod tests {
    //! Round-trip tests against an in-memory `tokio::io::DuplexStream`.
    //! The Python side carries an equivalent suite at
    //! `agent_runtime/tests/test_framing.py`; both must agree on the
    //! same wire shape.

    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trip_single_frame() {
        let (mut a, mut b) = duplex(1024);
        let body = b"{\"hello\":\"world\"}".to_vec();
        write_frame(&mut a, &body).await.expect("write");
        let frame = read_frame(&mut b).await.expect("read");
        match frame {
            Frame::Body(got) => assert_eq!(got, body),
            Frame::Eof => panic!("expected Body, got Eof"),
        }
    }

    #[tokio::test]
    async fn round_trip_three_frames_back_to_back() {
        let (mut a, mut b) = duplex(1024);
        let bodies: Vec<Vec<u8>> = vec![
            b"{\"a\":1}".to_vec(),
            b"{\"b\":[1,2,3]}".to_vec(),
            b"{\"c\":null}".to_vec(),
        ];
        for body in &bodies {
            write_frame(&mut a, body).await.expect("write");
        }
        // Drop the writer so the reader sees a clean Eof on the
        // post-loop fourth read.
        drop(a);
        for expected in &bodies {
            match read_frame(&mut b).await.expect("read") {
                Frame::Body(got) => assert_eq!(&got, expected),
                Frame::Eof => panic!("expected Body, got Eof"),
            }
        }
        assert!(matches!(
            read_frame(&mut b).await.expect("read"),
            Frame::Eof
        ));
    }

    #[tokio::test]
    async fn clean_eof_after_no_bytes() {
        let (a, mut b) = duplex(1024);
        drop(a);
        assert!(matches!(
            read_frame(&mut b).await.expect("read"),
            Frame::Eof
        ));
    }

    #[tokio::test]
    async fn unicode_body_preserved() {
        let (mut a, mut b) = duplex(1024);
        let payload = "{\"name\":\"süßer Hund 🐶\"}".as_bytes().to_vec();
        write_frame(&mut a, &payload).await.expect("write");
        match read_frame(&mut b).await.expect("read") {
            Frame::Body(got) => assert_eq!(got, payload),
            Frame::Eof => panic!("expected Body"),
        }
    }

    #[tokio::test]
    async fn zero_length_frame_round_trips() {
        let (mut a, mut b) = duplex(1024);
        write_frame(&mut a, b"").await.expect("write");
        match read_frame(&mut b).await.expect("read") {
            Frame::Body(got) => assert!(got.is_empty()),
            Frame::Eof => panic!("expected empty Body"),
        }
    }
}
