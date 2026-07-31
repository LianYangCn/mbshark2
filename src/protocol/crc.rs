//! CRC-16/Modbus (poly 0xA001, init 0xFFFF, reflected).
//!
//! Used to validate RTU frames. The two trailing CRC bytes in an RTU frame
//! are stored little-endian (low byte first).

use std::sync::LazyLock;

/// Precomputed table for the reflected 0xA001 polynomial.
static TABLE: LazyLock<[u16; 256]> = LazyLock::new(|| {
    let mut t = [0u16; 256];
    for (i, slot) in t.iter_mut().enumerate() {
        let mut crc = i as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
        *slot = crc;
    }
    t
});

/// Compute the CRC-16/Modbus of `buf`.
pub fn crc16(buf: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in buf {
        crc = (crc >> 8) ^ TABLE[((crc ^ b as u16) & 0xFF) as usize];
    }
    crc
}

/// Append the CRC (little-endian) to `buf`.
pub fn append_crc(buf: &mut Vec<u8>) {
    let c = crc16(buf);
    buf.push((c & 0xFF) as u8);
    buf.push((c >> 8) as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard check value: CRC-16/Modbus of "123456789" == 0x4B37.
    #[test]
    fn check_value() {
        assert_eq!(crc16(b"123456789"), 0x4B37);
    }

    #[test]
    fn round_trip() {
        let mut frame = vec![0x01, 0x03, 0x00, 0x00, 0x00, 0x0A];
        append_crc(&mut frame);
        // The CRC of everything except the last two bytes must match the
        // little-endian value in the last two bytes.
        let payload = &frame[..frame.len() - 2];
        let computed = crc16(payload);
        let stored = u16::from_le_bytes([frame[frame.len() - 2], frame[frame.len() - 1]]);
        assert_eq!(computed, stored);
    }

    /// Known frame: slave 1, FC 03, read holding registers from 0 for 10.
    /// Wire bytes `01 03 00 00 00 0A C5 CD` — the trailing CRC is stored
    /// little-endian (low byte 0xC5, high byte 0xCD), so the numeric CRC
    /// value is 0xCDC5.
    #[test]
    fn known_frame_crc() {
        let payload = [0x01, 0x03, 0x00, 0x00, 0x00, 0x0A];
        assert_eq!(crc16(&payload), 0xCDC5);
        // And the wire bytes round-trip through append_crc:
        let mut frame = payload.to_vec();
        append_crc(&mut frame);
        assert_eq!(&frame[6..], &[0xC5, 0xCD]);
    }
}
