//! Tail-only stderr ring buffer + stderr-tail formatting.
//!
//! Split out of the former single-file `transport.rs` (DEEP refactor).
//! Shared with `persistent_session`, which dimensions its own stderr
//! drain to the same [`STDERR_RING_CAPACITY`] budget and reuses the
//! [`RingBuffer`] shape.

/// 64 KiB upper bound on the stderr ring buffer. Generous enough to
/// hold a full `claude` traceback; small enough that the bound is
/// hit only on adversarial output.
///
/// Pub-within-crate so `persistent_session.rs` can dimension its own
/// stderr drain to the same budget without re-litigating the size.
pub(crate) const STDERR_RING_CAPACITY: usize = 64 * 1024;

/// Tail-only ring buffer. `append` truncates oldest bytes when full.
///
/// Pub-within-crate so `persistent_session.rs` reuses the same shape
/// for its own stderr drain.
pub(crate) struct RingBuffer {
    buf: Vec<u8>,
    capacity: usize,
}

impl RingBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity.min(8 * 1024)),
            capacity,
        }
    }

    pub(crate) fn append(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.capacity {
            // New burst alone exceeds capacity — keep only its tail.
            let start = bytes.len() - self.capacity;
            self.buf.clear();
            self.buf.extend_from_slice(&bytes[start..]);
            return;
        }
        let combined = self.buf.len() + bytes.len();
        if combined > self.capacity {
            let drop = combined - self.capacity;
            self.buf.drain(..drop);
        }
        self.buf.extend_from_slice(bytes);
    }

    pub(crate) fn tail_string(&self, max_bytes: usize) -> String {
        let start = self.buf.len().saturating_sub(max_bytes);
        String::from_utf8_lossy(&self.buf[start..]).into_owned()
    }
}

pub(crate) fn fmt_stderr_tail(tail: &str) -> String {
    let trimmed = tail.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(" — stderr tail: {trimmed}")
    }
}
