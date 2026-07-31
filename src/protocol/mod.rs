//! Modbus RTU protocol layer: CRC-16, frame validation, and PDU decoding.

pub mod crc;
pub mod frame;
pub mod pdu;

pub use frame::{ModbusFrame, ParseError};
pub use pdu::{parse_pdu, ParsedPdu, PduDetails, Role};
