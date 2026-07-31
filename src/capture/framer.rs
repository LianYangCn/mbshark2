//! RTU frame assembly from a byte stream.
//!
//! Modbus RTU delimits frames by a silent interval of at least 3.5 character
//! times on the bus. This struct accumulates received bytes and exposes the
//! deadline at which the current buffer should be emitted as a complete frame.
//! It is purely synchronous: the async read loop (in `capture::engine`) drives
//! it with `push` / `gap_deadline` / `flush_due`.
//!
//! CRC and structural validation happen in [`crate::protocol::frame`]; the
//! framer just yields the raw bytes of a candidate frame.

use std::time::{Duration, Instant};

/// Maximum RTU frame size (256 bytes) plus a small margin. If the buffer
/// exceeds this without a frame gap we force a flush, treating the contents as
/// garbage (a parse failure) rather than growing unboundedly.
pub const MAX_FRAME_BYTES: usize = 260;

/// Compute the RTU inter-frame gap (3.5 character times) for a baud rate.
///
/// Per the Modbus over Serial Line spec: for baud > 19200 the gap is fixed at
/// 1.75 ms; otherwise it is `3.5 × 11 bits / baud` (≈4.0 ms at 9600, 2.0 ms at
/// 19200).
pub fn frame_gap(baud: u32) -> Duration {
    if baud > 19200 {
        Duration::from_micros(1750)
    } else {
        // 3.5 char-times × 11 bits = 38.5 bits → 38_500_000 / baud µs
        Duration::from_micros(38_500_000 / baud as u64)
    }
}

#[derive(Debug)]
pub struct Framer {
    buf: Vec<u8>,
    last_byte_time: Instant,
    gap: Duration,
}

impl Framer {
    pub fn new(baud: u32) -> Self {
        Self::with_gap(frame_gap(baud))
    }

    pub fn with_gap(gap: Duration) -> Self {
        Framer {
            buf: Vec::new(),
            last_byte_time: Instant::now(),
            gap,
        }
    }

    /// Append received bytes, recording `now` as the time of the last byte.
    pub fn push(&mut self, bytes: &[u8], now: Instant) {
        if bytes.is_empty() {
            return;
        }
        self.buf.extend_from_slice(bytes);
        self.last_byte_time = now;
    }

    /// Deadline at which the current buffer becomes a complete frame, or
    /// `None` when the buffer is empty (so the read loop can use a pending
    /// future instead of a past deadline and avoid a busy loop).
    pub fn gap_deadline(&self) -> Option<Instant> {
        if self.buf.is_empty() {
            None
        } else {
            Some(self.last_byte_time + self.gap)
        }
    }

    /// Emit the accumulated bytes as a candidate frame and clear the buffer.
    pub fn flush_due(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// Whether the buffer has grown past the safety limit and should be
    /// force-flushed.
    pub fn overflowed(&self) -> bool {
        self.buf.len() > MAX_FRAME_BYTES
    }

    pub fn buffer_len(&self) -> usize {
        self.buf.len()
    }

    /// Reset internal state (used on Stop).
    pub fn reset(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_values() {
        assert_eq!(frame_gap(9600), Duration::from_micros(4010));
        assert_eq!(frame_gap(19200), Duration::from_micros(2005));
        assert_eq!(frame_gap(115200), Duration::from_micros(1750));
    }

    #[test]
    fn empty_buffer_has_no_deadline() {
        let f = Framer::new(9600);
        assert!(f.gap_deadline().is_none());
    }

    #[test]
    fn deadline_is_last_byte_plus_gap() {
        let mut f = Framer::new(9600);
        let now = Instant::now();
        f.push(&[0x01, 0x03], now);
        assert_eq!(f.gap_deadline(), Some(now + frame_gap(9600)));
    }

    #[test]
    fn flush_clears_buffer() {
        let mut f = Framer::new(9600);
        f.push(&[0x01, 0x03, 0x00], Instant::now());
        let frame = f.flush_due();
        assert_eq!(frame, vec![0x01, 0x03, 0x00]);
        assert!(f.buffer_len() == 0);
        assert!(f.gap_deadline().is_none());
    }

    #[test]
    fn overflow_detected() {
        let mut f = Framer::new(9600);
        let big = vec![0u8; MAX_FRAME_BYTES + 10];
        f.push(&big, Instant::now());
        assert!(f.overflowed());
        let flushed = f.flush_due();
        assert_eq!(flushed.len(), MAX_FRAME_BYTES + 10);
        assert!(!f.overflowed());
    }

    #[test]
    fn multiple_pushes_accumulate() {
        let mut f = Framer::new(9600);
        let t0 = Instant::now();
        f.push(&[0x01], t0);
        f.push(&[0x03, 0x00], t0);
        assert_eq!(f.buffer_len(), 3);
        assert_eq!(f.flush_due(), vec![0x01, 0x03, 0x00]);
    }
}
