//! Modbus PDU (Protocol Data Unit) decoding for all standard function codes.
//!
//! The PDU is the function-code byte plus its data bytes (i.e. the RTU frame
//! without the slave address and without the CRC). Modbus is big-endian on
//! the wire.

/// Whether a frame is a request, a response, or (before classification)
/// unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Request,
    Response,
    Unknown,
}

/// A sub-request within Read/Write File Records (FC 0x14/0x15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSubRequest {
    pub reference_type: u8,
    pub file_number: u16,
    pub record_number: u16,
    pub record_length: u16,
}

/// Parsed contents of a PDU. Variants are FC-specific; `Raw` covers any
/// structural mismatch (bad length, unknown FC, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PduDetails {
    // 0x01 / 0x02
    ReadCoilsReq { start: u16, count: u16 },
    ReadCoilsResp { bits: Vec<bool> },
    // 0x03 / 0x04
    ReadRegsReq { start: u16, count: u16 },
    ReadRegsResp { values: Vec<u16> },
    // 0x05
    WriteSingleCoilReq { addr: u16, on: bool },
    WriteSingleCoilResp { addr: u16, on: bool },
    // 0x06
    WriteSingleRegReq { addr: u16, value: u16 },
    WriteSingleRegResp { addr: u16, value: u16 },
    // 0x07
    ReadExceptionStatusResp { status: u8 },
    // 0x08
    Diagnostic { sub: u16, data: Vec<u8> },
    // 0x0B
    CommEventCounterResp { status: u16, event_count: u16 },
    // 0x0C
    CommEventLogResp {
        status: u16,
        event_count: u16,
        message_count: u16,
        events: Vec<u8>,
    },
    // 0x0F
    WriteMultipleCoilsReq { start: u16, count: u16, bits: Vec<bool> },
    WriteMultipleCoilsResp { start: u16, count: u16 },
    // 0x10
    WriteMultipleRegsReq { start: u16, count: u16, values: Vec<u16> },
    WriteMultipleRegsResp { start: u16, count: u16 },
    // 0x11
    ReportSlaveIdResp { slave_id: Vec<u8>, run_indicator: u8 },
    // 0x14 / 0x15 (parsed best-effort; raw sub-data retained)
    ReadFileRecordsReq { sub_requests: Vec<FileSubRequest> },
    ReadFileRecordsResp { byte_count: u8, data: Vec<u8> },
    WriteFileRecords { byte_count: u8, data: Vec<u8> },
    // 0x16
    MaskWriteRegReq { addr: u16, and_mask: u16, or_mask: u16 },
    MaskWriteRegResp { addr: u16, and_mask: u16, or_mask: u16 },
    // 0x17
    ReadWriteMultipleRegsReq {
        read_start: u16,
        read_count: u16,
        write_start: u16,
        write_count: u16,
        write_values: Vec<u16>,
    },
    ReadWriteMultipleRegsResp { read_values: Vec<u16> },
    // 0x18
    ReadFifoQueueResp { fifo_count: u16, values: Vec<u16> },
    // 0x2B
    EncapsulatedInterface { mei_type: u8, data: Vec<u8> },
    // Exception response (function | 0x80)
    Exception { code: u8 },
    /// Structural parse failure. `reason` is shown in the `  Error:` line.
    Raw { reason: String },
}

/// A parsed PDU together with the (possibly inferred) role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPdu {
    pub function: u8,
    pub role: Role,
    pub details: PduDetails,
}

/// Human-readable name for a function code (used by the format layer).
pub fn function_name(fc: u8) -> &'static str {
    match fc {
        0x01 => "Read Coils",
        0x02 => "Read Discrete Inputs",
        0x03 => "Read Holding Registers",
        0x04 => "Read Input Registers",
        0x05 => "Write Single Coil",
        0x06 => "Write Single Register",
        0x07 => "Read Exception Status",
        0x08 => "Diagnostics",
        0x0B => "Get Comm Event Counter",
        0x0C => "Get Comm Event Log",
        0x0F => "Write Multiple Coils",
        0x10 => "Write Holding Registers",
        0x11 => "Report Slave ID",
        0x14 => "Read File Records",
        0x15 => "Write File Records",
        0x16 => "Mask Write Register",
        0x17 => "Read/Write Multiple Registers",
        0x18 => "Read FIFO Queue",
        0x2B => "Encapsulated Interface Transport",
        _ => "Unknown Function",
    }
}

/// Translate a Modbus exception code to text.
pub fn exception_text(code: u8) -> &'static str {
    match code {
        1 => "Illegal Function",
        2 => "Illegal Data Address",
        3 => "Illegal Data Value",
        4 => "Slave Device Failure",
        5 => "Acknowledge",
        6 => "Slave Device Busy",
        8 => "Memory Parity Error",
        10 => "Gateway Path Unavailable",
        11 => "Gateway Target No Response",
        _ => "Unknown Exception",
    }
}

// --- helpers -----------------------------------------------------------------

fn u16be(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(i)?, *d.get(i + 1)?]))
}

fn read_regs(d: &[u8], from: usize, n: usize) -> Option<Vec<u16>> {
    if d.len() < from + 2 * n {
        return None;
    }
    (0..n).map(|k| u16be(d, from + 2 * k)).collect()
}

fn bytes_to_bits(data: &[u8], count: usize) -> Vec<bool> {
    (0..count).map(|i| data[i / 8] & (1 << (i % 8)) != 0).collect()
}

fn raw(reason: impl Into<String>) -> PduDetails {
    PduDetails::Raw { reason: reason.into() }
}

/// Infer the role of a frame from its PDU layout when the caller has no
/// context. Returns `Unknown` if the layout is ambiguous or malformed.
pub fn infer_role(fc: u8, data: &[u8]) -> Role {
    if fc & 0x80 != 0 {
        return Role::Response;
    }
    match fc {
        0x01..=0x04 => {
            if data.len() == 4 {
                Role::Request
            } else if !data.is_empty() && data.len() == 1 + data[0] as usize {
                Role::Response
            } else {
                Role::Unknown
            }
        }
        0x05 | 0x06 | 0x08 | 0x16 | 0x2B => Role::Unknown,
        0x07 | 0x0B | 0x0C | 0x11 => {
            if data.is_empty() {
                Role::Request
            } else {
                Role::Response
            }
        }
        0x0F | 0x10 => {
            if data.len() == 4 {
                Role::Response
            } else if data.len() >= 5 {
                Role::Request
            } else {
                Role::Unknown
            }
        }
        0x14 | 0x15 => Role::Unknown,
        0x17 => {
            if data.len() >= 9 {
                Role::Request
            } else {
                Role::Response
            }
        }
        0x18 => {
            if data.len() == 2 {
                Role::Request
            } else {
                Role::Response
            }
        }
        _ => Role::Unknown,
    }
}

/// Parse a PDU given a role hint. If the hint is `Unknown`, the role is
/// inferred from the layout and stored on the result.
pub fn parse_pdu(role_hint: Role, function: u8, data: &[u8]) -> ParsedPdu {
    // Exception response: function | 0x80.
    if function & 0x80 != 0 {
        let orig = function & 0x7F;
        let details = if data.is_empty() {
            raw("Exception response missing exception code")
        } else if data.len() == 1 {
            PduDetails::Exception { code: data[0] }
        } else {
            raw(format!(
                "Exception response for {} has extra bytes (expected 1, got {})",
                function_name(orig),
                data.len()
            ))
        };
        return ParsedPdu {
            function,
            role: Role::Response,
            details,
        };
    }

    let role = match role_hint {
        Role::Unknown => infer_role(function, data),
        other => other,
    };

    let details = match function {
        0x01 | 0x02 => parse_read_bits(function, role, data),
        0x03 | 0x04 => parse_read_regs(function, role, data),
        0x05 => parse_write_single_coil(role, data),
        0x06 => parse_write_single_reg(role, data),
        0x07 => parse_read_exception_status(role, data),
        0x08 => parse_diagnostic(data),
        0x0B => parse_comm_event_counter(role, data),
        0x0C => parse_comm_event_log(role, data),
        0x0F => parse_write_multiple_coils(role, data),
        0x10 => parse_write_multiple_regs(role, data),
        0x11 => parse_report_slave_id(role, data),
        0x14 | 0x15 => parse_file_records(function, role, data),
        0x16 => parse_mask_write_reg(data),
        0x17 => parse_read_write_multiple_regs(role, data),
        0x18 => parse_read_fifo_queue(role, data),
        0x2B => parse_encapsulated_interface(data),
        _ => raw(format!("Unknown Function 0x{:02X}", function)),
    };

    ParsedPdu {
        function,
        role,
        details,
    }
}

fn parse_read_bits(fc: u8, role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Request => {
            if d.len() != 4 {
                return raw(format!("{} request: expected 4 bytes, got {}", function_name(fc), d.len()));
            }
            PduDetails::ReadCoilsReq {
                start: u16be(d, 0).unwrap(),
                count: u16be(d, 2).unwrap(),
            }
        }
        _ => {
            if d.is_empty() {
                return raw(format!("{} response: empty", function_name(fc)));
            }
            let bc = d[0] as usize;
            if d.len() != 1 + bc {
                return raw(format!(
                    "{} response: byte count {} but {} data bytes",
                    function_name(fc),
                    bc,
                    d.len() - 1
                ));
            }
            PduDetails::ReadCoilsResp {
                bits: bytes_to_bits(&d[1..], bc * 8),
            }
        }
    }
}

fn parse_read_regs(fc: u8, role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Request => {
            if d.len() != 4 {
                return raw(format!("{} request: expected 4 bytes, got {}", function_name(fc), d.len()));
            }
            PduDetails::ReadRegsReq {
                start: u16be(d, 0).unwrap(),
                count: u16be(d, 2).unwrap(),
            }
        }
        _ => {
            if d.is_empty() {
                return raw(format!("{} response: empty", function_name(fc)));
            }
            let bc = d[0] as usize;
            if d.len() != 1 + bc || !bc.is_multiple_of(2) {
                return raw(format!(
                    "{} response: bad byte count {} for {} data bytes",
                    function_name(fc),
                    bc,
                    d.len() - 1
                ));
            }
            PduDetails::ReadRegsResp {
                values: read_regs(d, 1, bc / 2).unwrap_or_default(),
            }
        }
    }
}

fn parse_write_single_coil(role: Role, d: &[u8]) -> PduDetails {
    if d.len() != 4 {
        return raw(format!("Write Single Coil: expected 4 bytes, got {}", d.len()));
    }
    let addr = u16be(d, 0).unwrap();
    let value = u16be(d, 2).unwrap();
    let on = match value {
        0xFF00 => true,
        0x0000 => false,
        _ => return raw(format!("Write Single Coil: invalid value 0x{:04X}", value)),
    };
    match role {
        Role::Request => PduDetails::WriteSingleCoilReq { addr, on },
        _ => PduDetails::WriteSingleCoilResp { addr, on },
    }
}

fn parse_write_single_reg(role: Role, d: &[u8]) -> PduDetails {
    if d.len() != 4 {
        return raw(format!("Write Single Register: expected 4 bytes, got {}", d.len()));
    }
    let addr = u16be(d, 0).unwrap();
    let value = u16be(d, 2).unwrap();
    match role {
        Role::Request => PduDetails::WriteSingleRegReq { addr, value },
        _ => PduDetails::WriteSingleRegResp { addr, value },
    }
}

fn parse_read_exception_status(role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Request => {
            if !d.is_empty() {
                return raw(format!("Read Exception Status request: expected 0 bytes, got {}", d.len()));
            }
            raw("Read Exception Status (request, no data)")
        }
        _ => {
            if d.len() != 1 {
                return raw(format!("Read Exception Status response: expected 1 byte, got {}", d.len()));
            }
            PduDetails::ReadExceptionStatusResp { status: d[0] }
        }
    }
}

fn parse_diagnostic(d: &[u8]) -> PduDetails {
    if d.len() < 2 {
        return raw(format!("Diagnostics: expected ≥2 bytes, got {}", d.len()));
    }
    PduDetails::Diagnostic {
        sub: u16be(d, 0).unwrap(),
        data: d[2..].to_vec(),
    }
}

fn parse_comm_event_counter(role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Request => {
            if !d.is_empty() {
                return raw(format!("Get Comm Event Counter request: expected 0 bytes, got {}", d.len()));
            }
            raw("Get Comm Event Counter (request, no data)")
        }
        _ => {
            if d.len() != 4 {
                return raw(format!("Get Comm Event Counter response: expected 4 bytes, got {}", d.len()));
            }
            PduDetails::CommEventCounterResp {
                status: u16be(d, 0).unwrap(),
                event_count: u16be(d, 2).unwrap(),
            }
        }
    }
}

fn parse_comm_event_log(role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Request => {
            if !d.is_empty() {
                return raw(format!("Get Comm Event Log request: expected 0 bytes, got {}", d.len()));
            }
            raw("Get Comm Event Log (request, no data)")
        }
        _ => {
            if d.len() < 7 {
                return raw(format!("Get Comm Event Log response: expected ≥7 bytes, got {}", d.len()));
            }
            let bc = d[0] as usize;
            if d.len() != 1 + bc {
                return raw(format!("Get Comm Event Log: byte count {} but {} data bytes", bc, d.len() - 1));
            }
            PduDetails::CommEventLogResp {
                status: u16be(d, 1).unwrap(),
                event_count: u16be(d, 3).unwrap(),
                message_count: u16be(d, 5).unwrap(),
                events: d[7..].to_vec(),
            }
        }
    }
}

fn parse_write_multiple_coils(role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Response => {
            if d.len() != 4 {
                return raw(format!("Write Multiple Coils response: expected 4 bytes, got {}", d.len()));
            }
            PduDetails::WriteMultipleCoilsResp {
                start: u16be(d, 0).unwrap(),
                count: u16be(d, 2).unwrap(),
            }
        }
        _ => {
            if d.len() < 5 {
                return raw(format!("Write Multiple Coils request: expected ≥5 bytes, got {}", d.len()));
            }
            let start = u16be(d, 0).unwrap();
            let count = u16be(d, 2).unwrap();
            let bc = d[4] as usize;
            if d.len() != 5 + bc {
                return raw(format!("Write Multiple Coils: byte count {} but {} bytes", bc, d.len() - 5));
            }
            PduDetails::WriteMultipleCoilsReq {
                start,
                count,
                bits: bytes_to_bits(&d[5..], count as usize),
            }
        }
    }
}

fn parse_write_multiple_regs(role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Response => {
            if d.len() != 4 {
                return raw(format!("Write Holding Registers response: expected 4 bytes, got {}", d.len()));
            }
            PduDetails::WriteMultipleRegsResp {
                start: u16be(d, 0).unwrap(),
                count: u16be(d, 2).unwrap(),
            }
        }
        _ => {
            if d.len() < 5 {
                return raw(format!("Write Holding Registers request: expected ≥5 bytes, got {}", d.len()));
            }
            let start = u16be(d, 0).unwrap();
            let count = u16be(d, 2).unwrap();
            let bc = d[4] as usize;
            if d.len() != 5 + bc || !bc.is_multiple_of(2) || bc / 2 != count as usize {
                return raw(format!(
                    "Write Holding Registers: inconsistent count {} / byte count {} / {} bytes",
                    count,
                    bc,
                    d.len() - 5
                ));
            }
            PduDetails::WriteMultipleRegsReq {
                start,
                count,
                values: read_regs(d, 5, count as usize).unwrap_or_default(),
            }
        }
    }
}

fn parse_report_slave_id(role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Request => {
            if !d.is_empty() {
                return raw(format!("Report Slave ID request: expected 0 bytes, got {}", d.len()));
            }
            raw("Report Slave ID (request, no data)")
        }
        _ => {
            if d.is_empty() {
                return raw("Report Slave ID response: empty");
            }
            let bc = d[0] as usize;
            if d.len() != 1 + bc || bc == 0 {
                return raw(format!("Report Slave ID: byte count {} but {} data bytes", bc, d.len() - 1));
            }
            let run_indicator = d[d.len() - 1];
            PduDetails::ReportSlaveIdResp {
                slave_id: d[1..d.len() - 1].to_vec(),
                run_indicator,
            }
        }
    }
}

fn parse_file_records(fc: u8, role: Role, d: &[u8]) -> PduDetails {
    if d.is_empty() {
        return raw(format!("{}: empty", function_name(fc)));
    }
    let bc = d[0] as usize;
    if d.len() != 1 + bc {
        return raw(format!("{}: byte count {} but {} data bytes", function_name(fc), bc, d.len() - 1));
    }
    if fc == 0x14 && role == Role::Request {
        let mut subs = Vec::new();
        let mut i = 1;
        while i + 7 <= d.len() {
            subs.push(FileSubRequest {
                reference_type: d[i],
                file_number: u16be(d, i + 1).unwrap(),
                record_number: u16be(d, i + 3).unwrap(),
                record_length: u16be(d, i + 5).unwrap(),
            });
            i += 7;
        }
        PduDetails::ReadFileRecordsReq { sub_requests: subs }
    } else if fc == 0x14 {
        PduDetails::ReadFileRecordsResp {
            byte_count: d[0],
            data: d[1..].to_vec(),
        }
    } else {
        PduDetails::WriteFileRecords {
            byte_count: d[0],
            data: d[1..].to_vec(),
        }
    }
}

fn parse_mask_write_reg(d: &[u8]) -> PduDetails {
    if d.len() != 6 {
        return raw(format!("Mask Write Register: expected 6 bytes, got {}", d.len()));
    }
    // Request and response share the same layout; role does not change it.
    PduDetails::MaskWriteRegReq {
        addr: u16be(d, 0).unwrap(),
        and_mask: u16be(d, 2).unwrap(),
        or_mask: u16be(d, 4).unwrap(),
    }
}

fn parse_read_write_multiple_regs(role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Response => {
            if d.is_empty() {
                return raw("Read/Write Multiple Registers response: empty");
            }
            let bc = d[0] as usize;
            if d.len() != 1 + bc || !bc.is_multiple_of(2) {
                return raw(format!("Read/Write Multiple Registers response: bad byte count {}", bc));
            }
            PduDetails::ReadWriteMultipleRegsResp {
                read_values: read_regs(d, 1, bc / 2).unwrap_or_default(),
            }
        }
        _ => {
            if d.len() < 9 {
                return raw(format!("Read/Write Multiple Registers request: expected ≥9 bytes, got {}", d.len()));
            }
            let read_start = u16be(d, 0).unwrap();
            let read_count = u16be(d, 2).unwrap();
            let write_start = u16be(d, 4).unwrap();
            let write_count = u16be(d, 6).unwrap();
            let bc = d[8] as usize;
            if d.len() != 9 + bc || !bc.is_multiple_of(2) || bc / 2 != write_count as usize {
                return raw("Read/Write Multiple Registers request: inconsistent write count");
            }
            PduDetails::ReadWriteMultipleRegsReq {
                read_start,
                read_count,
                write_start,
                write_count,
                write_values: read_regs(d, 9, write_count as usize).unwrap_or_default(),
            }
        }
    }
}

fn parse_read_fifo_queue(role: Role, d: &[u8]) -> PduDetails {
    match role {
        Role::Request => {
            if d.len() != 2 {
                return raw(format!("Read FIFO Queue request: expected 2 bytes, got {}", d.len()));
            }
            raw(format!("Read FIFO Queue (request, addr 0x{:04X})", u16be(d, 0).unwrap()))
        }
        _ => {
            if d.len() < 4 {
                return raw(format!("Read FIFO Queue response: expected ≥4 bytes, got {}", d.len()));
            }
            let bc = u16be(d, 0).unwrap() as usize;
            if d.len() != 2 + bc {
                return raw(format!("Read FIFO Queue response: byte count {} but {} bytes", bc, d.len() - 2));
            }
            let fifo_count = u16be(d, 2).unwrap();
            PduDetails::ReadFifoQueueResp {
                fifo_count,
                values: read_regs(d, 4, fifo_count as usize).unwrap_or_default(),
            }
        }
    }
}

fn parse_encapsulated_interface(d: &[u8]) -> PduDetails {
    if d.is_empty() {
        return raw("Encapsulated Interface Transport: empty");
    }
    PduDetails::EncapsulatedInterface {
        mei_type: d[0],
        data: d[1..].to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be(v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    #[test]
    fn read_holding_registers_request() {
        let pdu = parse_pdu(Role::Unknown, 0x03, &be(&[0x0000, 0x000A]));
        assert_eq!(pdu.role, Role::Request);
        match pdu.details {
            PduDetails::ReadRegsReq { start, count } => {
                assert_eq!(start, 0);
                assert_eq!(count, 10);
            }
            d => panic!("wrong variant {:?}", d),
        }
    }

    #[test]
    fn write_multiple_registers_request_matches_readme() {
        // FC 0x10: start 0x0000, count 2, byte_count 4, values 0x0000 0x0001
        let mut data = be(&[0x0000, 0x0002]);
        data.push(0x04);
        data.extend(be(&[0x0000, 0x0001]));
        let pdu = parse_pdu(Role::Unknown, 0x10, &data);
        assert_eq!(pdu.role, Role::Request);
        match pdu.details {
            PduDetails::WriteMultipleRegsReq { start, count, values } => {
                assert_eq!(start, 0);
                assert_eq!(count, 2);
                assert_eq!(values, vec![0x0000, 0x0001]);
            }
            d => panic!("wrong variant {:?}", d),
        }
    }

    #[test]
    fn write_multiple_registers_response() {
        let pdu = parse_pdu(Role::Unknown, 0x10, &be(&[0x0000, 0x0002]));
        assert_eq!(pdu.role, Role::Response);
        assert!(matches!(
            pdu.details,
            PduDetails::WriteMultipleRegsResp { start: 0, count: 2 }
        ));
    }

    #[test]
    fn exception_response() {
        let pdu = parse_pdu(Role::Unknown, 0x83, &[0x02]);
        assert_eq!(pdu.role, Role::Response);
        assert!(matches!(pdu.details, PduDetails::Exception { code: 2 }));
        assert_eq!(exception_text(2), "Illegal Data Address");
    }

    #[test]
    fn unknown_function_is_raw() {
        // 0x55 has no 0x80 bit (so not an exception) and isn't a known FC.
        let pdu = parse_pdu(Role::Unknown, 0x55, &[0x00]);
        assert!(matches!(pdu.details, PduDetails::Raw { .. }));
    }

    #[test]
    fn bad_length_is_raw() {
        let mut data = be(&[0x0000, 0x0002]);
        data.push(0x06);
        data.extend(be(&[0x0000, 0x0001]));
        let pdu = parse_pdu(Role::Request, 0x10, &data);
        assert!(matches!(pdu.details, PduDetails::Raw { .. }));
    }

    #[test]
    fn write_single_coil_value() {
        let pdu = parse_pdu(Role::Request, 0x05, &be(&[0x00AC, 0xFF00]));
        assert!(matches!(
            pdu.details,
            PduDetails::WriteSingleCoilReq { addr: 0xAC, on: true }
        ));
    }

    #[test]
    fn function_names() {
        assert_eq!(function_name(0x10), "Write Holding Registers");
        assert_eq!(function_name(0x03), "Read Holding Registers");
        assert_eq!(function_name(0x01), "Read Coils");
    }
}
