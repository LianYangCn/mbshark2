//! RTU frame assembly from a byte stream.
//!
//! Frame delimiting uses two complementary mechanisms:
//!
//! - **Length-prediction + CRC split (primary)** — [`Framer::drain_complete`]
//!   peels complete frames off the front of the buffer by predicting each
//!   frame's length from its function-code structure and confirming the
//!   boundary with a CRC check. This is what makes coalesced frames (multiple
//!   RTU frames delivered in one `read()`, common on real hardware) split
//!   correctly, since the OS stamps the whole read with a single time and the
//!   inter-frame gap is invisible to timing logic.
//!
//! - **3.5-character inter-frame gap (fallback)** — [`Framer::gap_deadline`]
//!   exposes the deadline at which whatever remains in the buffer (an
//!   indeterminate-function-code frame, a partial frame, or a corrupt frame
//!   that failed CRC) is emitted as a single candidate frame, which becomes a
//!   `ParseFailure` entry if invalid. Nothing is ever discarded.
//!
//! The struct is purely synchronous: the async read loop (in `capture::engine`)
//! drives it with `push` / `drain_complete` / `gap_deadline` / `flush_due`.

use std::time::{Duration, Instant};

use crate::protocol::crc::crc16;
use crate::protocol::frame::{frame_length_hint, LenHint};

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

    /// Peel all complete, CRC-valid frames off the front of the buffer.
    ///
    /// This is the primary frame-delimiting mechanism. Real serial stacks
    /// routinely deliver bytes from multiple RTU frames in a single `read()`
    /// (all stamped with one time), so the inter-frame time gap is invisible to
    /// the gap-timer logic. Instead we predict each frame's length from its
    /// function-code structure (`frame_length_hint`) and confirm the boundary
    /// with a CRC check, greedily consuming one frame at a time.
    ///
    /// Any indeterminate / partial / CRC-failing tail is left in the buffer for
    /// the gap timer to flush as a single parse-failure entry (so nothing is
    /// ever discarded). The returned vectors are already CRC-validated; callers
    /// re-validate via `ModbusFrame::from_bytes` harmlessly.
    pub fn drain_complete(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 4 {
                break;
            }
            let cands = match frame_length_hint(&self.buf) {
                LenHint::Indeterminate | LenHint::NeedMore(_) => break,
                LenHint::Candidates(c) => c,
            };
            let mut peeled: Option<usize> = None;
            for &n in &cands {
                if !(4..=MAX_FRAME_BYTES).contains(&n) || self.buf.len() < n {
                    continue;
                }
                let payload = &self.buf[..n - 2];
                let stored = u16::from_le_bytes([self.buf[n - 2], self.buf[n - 1]]);
                if crc16(payload) == stored {
                    peeled = Some(n);
                    break; // candidates are ascending: first valid wins
                }
            }
            match peeled {
                Some(n) => out.push(self.buf.drain(..n).collect::<Vec<u8>>()),
                None => break, // need more bytes, or a corrupt frame → wait for gap
            }
        }
        out
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

    // --- drain_complete tests ----------------------------------------------

    /// Build a valid RTU frame (slave + fc + data + CRC) from a payload.
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut v = payload.to_vec();
        crate::protocol::crc::append_crc(&mut v);
        v
    }

    /// A framer with a huge gap so the gap timer never interferes with tests.
    fn frozen_framer() -> Framer {
        Framer::with_gap(Duration::from_secs(60))
    }

    #[test]
    fn drain_splits_two_coalesced_requests() {
        let mut f = frozen_framer();
        let req = frame(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]);
        let mut both = req.clone();
        both.extend_from_slice(&req);
        f.push(&both, Instant::now());

        let out = f.drain_complete();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], req);
        assert_eq!(out[1], req);
        assert_eq!(f.buffer_len(), 0);
    }

    #[test]
    fn drain_splits_request_then_response() {
        let mut f = frozen_framer();
        let req = frame(&[0x02, 0x10, 0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]);
        let resp = frame(&[0x02, 0x10, 0x00, 0x00, 0x00, 0x02]);
        let mut both = req.clone();
        both.extend_from_slice(&resp);
        f.push(&both, Instant::now());

        let out = f.drain_complete();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], req);
        assert_eq!(out[1], resp);
        assert_eq!(f.buffer_len(), 0);
    }

    #[test]
    fn drain_leaves_partial_tail() {
        let mut f = frozen_framer();
        let req = frame(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]);
        let mut both = req.clone();
        both.extend_from_slice(&[0x01, 0x03, 0x00]); // 3 bytes of a future frame
        f.push(&both, Instant::now());

        let out = f.drain_complete();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], req);
        assert_eq!(f.buffer_len(), 3);
        assert_eq!(f.flush_due(), vec![0x01, 0x03, 0x00]);
    }

    #[test]
    fn drain_partial_frame_returns_empty() {
        let mut f = frozen_framer();
        // First 6 bytes of an 0x03 request (no CRC) — no complete frame yet.
        f.push(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A], Instant::now());
        let out = f.drain_complete();
        assert!(out.is_empty());
        assert_eq!(f.buffer_len(), 6);
    }

    #[test]
    fn drain_indeterminate_function_code_breaks() {
        let mut f = frozen_framer();
        // A valid 0x08 (Diagnostics) frame is Indeterminate, so drain_complete
        // must NOT peel it — even when a valid 0x03 request follows. The whole
        // buffer is left for the gap timer to flush as one parse-failure entry.
        let diag = frame(&[0x01, 0x08, 0x00, 0x00]);
        let req = frame(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]);
        let mut both = diag.clone();
        both.extend_from_slice(&req);
        f.push(&both, Instant::now());

        let out = f.drain_complete();
        assert!(out.is_empty(), "Indeterminate FC must not be peeled");
        assert_eq!(f.buffer_len(), both.len());
    }

    #[test]
    fn drain_overflow_candidate_is_skipped() {
        let mut f = frozen_framer();
        // 0x18 with a corrupt u16 count of 0xFFFF → response candidate
        // 6 + 65535 = 65541 (> MAX_FRAME_BYTES). The 6-byte request candidate
        // isn't satisfiable with only 4 bytes, so nothing is peeled and no
        // panic occurs on the oversized candidate.
        f.push(&[0x01, 0x18, 0xFF, 0xFF], Instant::now());
        let out = f.drain_complete();
        assert!(out.is_empty());
        assert_eq!(f.buffer_len(), 4);
    }

    #[test]
    fn drain_after_reset_returns_empty() {
        let mut f = frozen_framer();
        f.push(&frame(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]), Instant::now());
        f.reset();
        assert!(f.drain_complete().is_empty());
        assert_eq!(f.buffer_len(), 0);
    }
}
