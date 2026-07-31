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
}
