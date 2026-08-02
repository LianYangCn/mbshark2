//! Modbus RTU frame validation.
//!
//! An RTU frame is `[slave(1)][function(1)][data...][crc_lo(1)][crc_hi(1)]`,
//! where the CRC covers everything except itself and is stored
//! little-endian. The minimum frame length is 4 bytes (slave + function +
//! 2 CRC bytes).

use chrono::{DateTime, Local};

use crate::protocol::crc::crc16;

/// A raw byte-level error encountered while splitting an RTU frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Fewer than 4 bytes — cannot contain slave + function + CRC.
    TooShort { len: usize },
    /// CRC mismatch. Carries the raw bytes so the UI can still show them.
    BadCrc {
        raw: Vec<u8>,
        expected: u16,
        computed: u16,
    },
}

impl ParseError {
    /// Human-readable reason line (used verbatim in the `  Error: <reason>`
    /// display line).
    pub fn reason(&self) -> String {
        match self {
            ParseError::TooShort { len } => {
                format!("Truncated frame ({} bytes, need ≥4)", len)
            }
            ParseError::BadCrc {
                expected, computed, ..
            } => {
                format!(
                    "Bad CRC (expected 0x{:04X}, computed 0x{:04X})",
                    expected, computed
                )
            }
        }
    }

    /// The raw bytes associated with the failure (for display).
    pub fn raw(&self) -> &[u8] {
        match self {
            ParseError::TooShort { .. } => &[],
            ParseError::BadCrc { raw, .. } => raw,
        }
    }
}

/// A validated Modbus RTU frame.
#[derive(Debug, Clone)]
pub struct ModbusFrame {
    /// The complete raw bytes including slave, function, data and CRC.
    pub raw: Vec<u8>,
    pub slave: u8,
    pub function: u8,
    /// PDU data bytes (everything between function and CRC).
    pub data: Vec<u8>,
    /// When the frame was received (wall-clock, for display + timeout math).
    pub timestamp: DateTime<Local>,
}

impl ModbusFrame {
    /// Validate `raw` as an RTU frame.
    ///
    /// Returns `Err(ParseError::TooShort)` if shorter than 4 bytes,
    /// `Err(ParseError::BadCrc)` if the CRC check fails (the raw bytes are
    /// carried along so the caller can still display them), or `Ok` with the
    /// parsed frame.
    pub fn from_bytes(raw: Vec<u8>, timestamp: DateTime<Local>) -> Result<Self, ParseError> {
        if raw.len() < 4 {
            return Err(ParseError::TooShort { len: raw.len() });
        }
        let split = raw.len() - 2;
        let payload = &raw[..split];
        let computed = crc16(payload);
        let expected = u16::from_le_bytes([raw[split], raw[split + 1]]);
        if computed != expected {
            return Err(ParseError::BadCrc {
                raw,
                expected,
                computed,
            });
        }
        let slave = raw[0];
        let function = raw[1];
        let data = raw[2..split].to_vec();
        Ok(ModbusFrame {
            raw,
            slave,
            function,
            data,
            timestamp,
        })
    }
}

/// Outcome of predicting a frame's total length from a buffer prefix.
///
/// Used by the framer to split coalesced RTU frames proactively: when bytes
/// from multiple frames arrive in a single `read()` (common on real hardware),
/// the inter-frame time gap is undetectable, so we fall back to structural
/// length prediction plus a CRC check to find each frame boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LenHint {
    /// Need at least this many bytes before a length can be predicted.
    NeedMore(usize),
    /// One or more candidate total frame lengths (including the 2 CRC bytes),
    /// ascending and de-duplicated. The caller validates each via CRC and
    /// consumes the first that matches.
    Candidates(Vec<usize>),
    /// The function code admits no self-describing length (e.g. 0x08
    /// Diagnostics, 0x2B Encapsulated Interface, unknown FCs). Fall back to
    /// gap-timeout framing.
    Indeterminate,
}

/// Predict candidate total frame lengths (incl. CRC) from a buffer prefix.
///
/// The frame layout is `[slave(1)][function(1)][data(L)][crc_lo][crc_hi]`,
/// so `total = L + 4` and `data = buf[2 .. len-2]`, i.e. `data[k] == buf[k+2]`.
///
/// Lengths are derived from the per-function-code structure as accepted by
/// [`crate::protocol::pdu::parse_pdu`]. For function codes where the request
/// and response have different layouts, all shape-compatible candidates are
/// returned; the framer disambiguates by CRC. For function codes whose length
/// cannot be determined from a prefix (variable-length with no byte-count
/// field), [`LenHint::Indeterminate`] is returned.
pub fn frame_length_hint(buf: &[u8]) -> LenHint {
    if buf.len() < 2 {
        return LenHint::NeedMore(2); // need slave + function
    }
    if buf.len() < 4 {
        return LenHint::NeedMore(4); // minimum frame (slave + fc + crc)
    }
    let fc = buf[1];

    // Exception response: slave + function + exception(1) + crc(2) = 5.
    if fc & 0x80 != 0 {
        return LenHint::Candidates(vec![5]);
    }

    let mut cands: Vec<usize> = Vec::new();
    // `data[k] == buf[k+2]`; byte-count fields living at data[k] are at buf[k+2].
    let bc_at = |k: usize| -> usize { 5 + buf[k] as usize }; // 4 + (1 + byte_count)
    match fc {
        0x01..=0x04 => {
            // Request: data=4 → total 8. Response: total = 5 + buf[2].
            cands.push(8);
            cands.push(bc_at(2));
        }
        0x05 | 0x06 => {
            // Request and response both: data=4 → total 8.
            cands.push(8);
        }
        0x07 => {
            // Request: data=0 → total 4. Response: data=1 → total 5.
            cands.push(4);
            cands.push(5);
        }
        0x08 => {
            // Diagnostics: sub(2) + variable data; no self-describing length.
            return LenHint::Indeterminate;
        }
        0x0B => {
            // Request: data=0 → total 4. Response: data=4 → total 8.
            cands.push(4);
            cands.push(8);
        }
        0x0C | 0x11 => {
            // Request: data=0 → total 4. Response: total = 5 + buf[2].
            cands.push(4);
            cands.push(bc_at(2));
        }
        0x0F | 0x10 => {
            // Response: data=4 → total 8.
            // Request: data = start(2)+count(2)+bytecount(1)+data(bytecount);
            //          bytecount at data[4] = buf[6] → total = 9 + buf[6].
            cands.push(8);
            if buf.len() >= 7 {
                cands.push(9 + buf[6] as usize);
            }
        }
        0x14 | 0x15 => {
            // data = byte_count(1) + payload(byte_count); byte_count at data[0]
            // = buf[2] → total = 5 + buf[2].
            cands.push(bc_at(2));
        }
        0x16 => {
            // Request and response both: data=6 → total 10.
            cands.push(10);
        }
        0x17 => {
            // Response: total = 5 + buf[2].
            // Request: data = read_start(2)+read_count(2)+write_start(2)+
            //          write_count(2)+bytecount(1)+data(bytecount);
            //          bytecount at data[8] = buf[10] → total = 13 + buf[10].
            cands.push(bc_at(2));
            if buf.len() >= 11 {
                cands.push(13 + buf[10] as usize);
            }
        }
        0x18 => {
            // Request: data=2 → total 6.
            // Response: data = count(2 u16) + payload(count); count at
            //           data[0..2] = buf[2..4] → total = 6 + u16be(buf,2).
            cands.push(6);
            let count = u16::from_be_bytes([buf[2], buf[3]]) as usize;
            cands.push(6 + count);
        }
        0x2B => {
            // Encapsulated Interface: mei_type(1) + variable; no self-describing length.
            return LenHint::Indeterminate;
        }
        _ => {
            return LenHint::Indeterminate;
        }
    }
    cands.sort_unstable();
    cands.dedup();
    LenHint::Candidates(cands)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_bytes(payload: &[u8]) -> Vec<u8> {
        let mut v = payload.to_vec();
        let c = crc16(payload);
        v.push((c & 0xFF) as u8);
        v.push((c >> 8) as u8);
        v
    }

    #[test]
    fn parses_valid_frame() {
        let raw = frame_bytes(&[0x02, 0x10, 0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]);
        let f = ModbusFrame::from_bytes(raw.clone(), Local::now()).unwrap();
        assert_eq!(f.slave, 0x02);
        assert_eq!(f.function, 0x10);
        assert_eq!(f.data, &[0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(f.raw, raw);
    }

    #[test]
    fn rejects_too_short() {
        let err = ModbusFrame::from_bytes(vec![0x01, 0x03], Local::now()).unwrap_err();
        assert_eq!(err, ParseError::TooShort { len: 2 });
        assert!(err.reason().contains("Truncated"));
    }

    #[test]
    fn rejects_bad_crc_and_carries_raw() {
        let mut raw = frame_bytes(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]);
        // Corrupt the last CRC byte.
        let last = raw.len() - 1;
        raw[last] ^= 0xFF;
        let err = ModbusFrame::from_bytes(raw.clone(), Local::now()).unwrap_err();
        assert!(err.reason().contains("Bad CRC"));
        assert_eq!(err.raw(), raw.as_slice());
        match err {
            ParseError::BadCrc { expected, computed, .. } => assert_ne!(expected, computed),
            _ => panic!("expected BadCrc"),
        }
    }

    // --- frame_length_hint tests -------------------------------------------

    /// Candidate list of a full frame built from `payload` (slave + fc + data).
    fn cands_of(buf: &[u8]) -> Vec<usize> {
        match frame_length_hint(buf) {
            LenHint::Candidates(c) => c,
            other => panic!("expected Candidates, got {other:?}"),
        }
    }

    #[test]
    fn hint_short_buffers_need_more() {
        assert_eq!(frame_length_hint(&[]), LenHint::NeedMore(2));
        assert_eq!(frame_length_hint(&[0x01]), LenHint::NeedMore(2));
        assert_eq!(frame_length_hint(&[0x01, 0x03]), LenHint::NeedMore(4));
        assert_eq!(frame_length_hint(&[0x01, 0x03, 0x00]), LenHint::NeedMore(4));
    }

    #[test]
    fn hint_exception_response() {
        // slave + 0x83 + exception(1) + crc = 5
        let buf = frame_bytes(&[0x02, 0x83, 0x02]);
        assert_eq!(cands_of(&buf), vec![5]);
    }

    #[test]
    fn hint_read_holding_regs_request() {
        // 0x03 request: data=4 → total 8; buf[2]=start_hi=0x00 → 5+0=5
        let buf = frame_bytes(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]);
        assert_eq!(cands_of(&buf), vec![5, 8]);
    }

    #[test]
    fn hint_read_holding_regs_response() {
        // 0x03 response: bc=4 at buf[2] → 5+4=9; plus req candidate 8
        let buf = frame_bytes(&[0x01, 0x03, 0x04, 0x00, 0x0A, 0x00, 0x14, 0x00, 0x1E]);
        assert_eq!(cands_of(&buf), vec![8, 9]);
    }

    #[test]
    fn hint_write_single_coil_and_reg() {
        // 0x05 / 0x06: data=4 → total 8 (both req and resp)
        assert_eq!(cands_of(&frame_bytes(&[0x01, 0x05, 0x00, 0xAC, 0xFF, 0x00])), vec![8]);
        assert_eq!(cands_of(&frame_bytes(&[0x01, 0x06, 0x00, 0x01, 0x00, 0x03])), vec![8]);
    }

    #[test]
    fn hint_read_exception_status() {
        // 0x07: req data=0 → 4; resp data=1 → 5. Hint is role-agnostic.
        assert_eq!(cands_of(&frame_bytes(&[0x01, 0x07])), vec![4, 5]);
    }

    #[test]
    fn hint_diagnostic_is_indeterminate() {
        assert_eq!(frame_length_hint(&frame_bytes(&[0x01, 0x08, 0x00, 0x00])), LenHint::Indeterminate);
    }

    #[test]
    fn hint_comm_event_counter() {
        // 0x0B: req data=0 → 4; resp data=4 → 8
        assert_eq!(cands_of(&frame_bytes(&[0x01, 0x0B])), vec![4, 8]);
    }

    #[test]
    fn hint_comm_event_log_response() {
        // 0x0C resp: bc=6 at buf[2] → 5+6=11; plus req candidate 4
        let buf = frame_bytes(&[0x01, 0x0C, 0x06, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(cands_of(&buf), vec![4, 11]);
    }

    #[test]
    fn hint_write_multiple_regs_request_full() {
        // 0x10 req: bytecount at buf[6]=0x04 → 9+4=13; resp candidate 8
        let buf = frame_bytes(&[0x02, 0x10, 0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(cands_of(&buf), vec![8, 13]);
    }

    #[test]
    fn hint_write_multiple_regs_request_partial() {
        // With fewer than 7 bytes, the req byte-count byte (buf[6]) isn't
        // available, so only the resp candidate {8} is offered.
        let full = frame_bytes(&[0x02, 0x10, 0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(cands_of(&full[..4]), vec![8]);
        assert_eq!(cands_of(&full[..6]), vec![8]);
        // Once 7 bytes are present the req candidate appears.
        assert_eq!(cands_of(&full[..7]), vec![8, 13]);
    }

    #[test]
    fn hint_write_multiple_coils_request() {
        // 0x0F req: bytecount at buf[6]=0x01 → 9+1=10; resp candidate 8
        let buf = frame_bytes(&[0x02, 0x0F, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01]);
        assert_eq!(cands_of(&buf), vec![8, 10]);
    }

    #[test]
    fn hint_report_slave_id_response() {
        // 0x11 resp: bc=3 at buf[2] → 5+3=8; req candidate 4
        let buf = frame_bytes(&[0x01, 0x11, 0x03, 0x21, 0x42, 0x00]);
        assert_eq!(cands_of(&buf), vec![4, 8]);
    }

    #[test]
    fn hint_read_file_records() {
        // 0x14: data = 1 + byte_count(at buf[2]); bc=7 → 5+7=12
        let buf = frame_bytes(&[0x01, 0x14, 0x07, 0x06, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]);
        assert_eq!(cands_of(&buf), vec![12]);
    }

    #[test]
    fn hint_mask_write_register() {
        // 0x16: data=6 → total 10
        assert_eq!(cands_of(&frame_bytes(&[0x01, 0x16, 0x00, 0x04, 0xFF, 0xF2, 0x00, 0x00])), vec![10]);
    }

    #[test]
    fn hint_read_write_multiple_regs_request_full() {
        // 0x17 req: write bytecount at buf[10]=0x04 → 13+4=17;
        // resp candidate 5+buf[2]=5+0=5
        let buf = frame_bytes(&[
            0x01, 0x17, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x0A, 0x00, 0x0B,
        ]);
        assert_eq!(cands_of(&buf), vec![5, 17]);
    }

    #[test]
    fn hint_read_write_multiple_regs_request_partial() {
        // With fewer than 11 bytes the req byte-count (buf[10]) isn't present,
        // so only the resp candidate {5+buf[2]} is offered.
        let full = frame_bytes(&[
            0x01, 0x17, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x0A, 0x00, 0x0B,
        ]);
        assert_eq!(cands_of(&full[..4]), vec![5]);
        assert_eq!(cands_of(&full[..10]), vec![5]);
        assert_eq!(cands_of(&full[..11]), vec![5, 17]);
    }

    #[test]
    fn hint_read_fifo_queue_request() {
        // 0x18 req: data=2 (addr=0x0001) → total 6; resp candidate 6+1=7
        let buf = frame_bytes(&[0x01, 0x18, 0x00, 0x01]);
        assert_eq!(cands_of(&buf), vec![6, 7]);
    }

    #[test]
    fn hint_read_fifo_queue_response() {
        // 0x18 resp: count(bc)=6 at buf[2..4] → 6+6=12; req candidate 6
        let buf = frame_bytes(&[0x01, 0x18, 0x00, 0x06, 0x00, 0x02, 0x00, 0x0A, 0x00, 0x0B]);
        assert_eq!(cands_of(&buf), vec![6, 12]);
    }

    #[test]
    fn hint_read_fifo_queue_huge_count_overflows_max_frame() {
        // A corrupt u16 count (0xFFFF) yields a candidate (6+65535) far beyond
        // MAX_FRAME_BYTES; the framer will reject it and fall back to gap.
        let buf = [0x01, 0x18, 0xFF, 0xFF];
        assert_eq!(cands_of(&buf), vec![6, 65541]);
    }

    #[test]
    fn hint_encapsulated_interface_is_indeterminate() {
        assert_eq!(frame_length_hint(&frame_bytes(&[0x01, 0x2B, 0x0E, 0x01, 0x00])), LenHint::Indeterminate);
    }

    #[test]
    fn hint_unknown_fc_is_indeterminate() {
        assert_eq!(frame_length_hint(&frame_bytes(&[0x01, 0x55, 0x00])), LenHint::Indeterminate);
    }
}
