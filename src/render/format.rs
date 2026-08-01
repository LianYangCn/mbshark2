//! Shared text formatting for display entries. Produces structured
//! [`Line`]s (a list of [`Span`]s) so the UI can colorize by [`SpanRole`]
//! while the exporter writes the same text without color.
//!
//! This is the single source of truth for the on-screen / on-disk layout,
//! matching the three scenarios in the README (normal pair, timeout, orphan).

use chrono::{DateTime, Local};

use crate::protocol::pdu::{exception_text, function_name, ParsedPdu, PduDetails};
use crate::session::model::{Entry, EntryBody, Tag};

/// The role a span plays in rendering — maps to a color in the UI, and is
/// ignored by the plain-text exporter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanRole {
    Tag,
    Timestamp,
    Hex,
    Label,
    Address,
    Value,
    Error,
    Plain,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub role: SpanRole,
}

#[derive(Debug, Clone)]
pub struct Line(pub Vec<Span>);

impl Line {
    fn plain() -> Self {
        Line(Vec::new())
    }
    fn push(mut self, text: impl Into<String>, role: SpanRole) -> Self {
        self.0.push(Span {
            text: text.into(),
            role,
        });
        self
    }
}

/// Render an entry to its display lines.
pub fn format_entry(entry: &Entry) -> Vec<Line> {
    let mut lines = Vec::new();
    lines.push(header_line(entry.tag, entry.ts, entry.counter, &entry.raw));

    match &entry.body {
        EntryBody::Frame { slave, parsed } => match entry.tag {
            Tag::Request => {
                lines.push(slave_line(*slave));
                lines.extend(request_detail_lines(parsed));
            }
            Tag::Response => {
                lines.push(slave_line(*slave));
                lines.extend(response_detail_lines(parsed));
            }
            _ => {}
        },
        EntryBody::Timeout => {
            lines.push(error_line("Timeout"));
        }
        EntryBody::Orphan { slave, orig, .. } => {
            lines.push(slave_line(*slave));
            lines.push(error_line("Response Timeout"));
            lines.extend(request_detail_lines(orig));
        }
        EntryBody::Unsolicited { slave, parsed } => {
            lines.push(slave_line(*slave));
            lines.push(error_line("No matching request"));
            lines.extend(response_detail_lines(parsed));
        }
        EntryBody::ParseFailure { reason } => {
            lines.push(error_line(reason));
        }
    }

    lines
}

/// Flatten lines to plain text (no color), separated by newlines.
pub fn lines_to_plain(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| l.0.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Convenience: format an entry straight to plain text.
pub fn entry_to_plain(entry: &Entry) -> String {
    lines_to_plain(&format_entry(entry))
}

/// Decide whether a visual separator should be inserted before an entry.
///
/// Separator rules:
/// - Different transaction (counter changed): always separate
/// - Orphan / Parse entry: cannot belong to a normal session, so separate
///   it even if the counter happens to match
pub fn should_separate(prev_counter: Option<u64>, current_counter: u64, tag: Tag) -> bool {
    let counter_changed = prev_counter.is_some_and(|pc| pc != current_counter);
    let is_standalone = matches!(tag, Tag::Orphan | Tag::Parse);
    counter_changed || is_standalone
}

// --- builders ----------------------------------------------------------------

fn header_line(tag: Tag, ts: DateTime<Local>, counter: u64, raw: &[u8]) -> Line {
    let mut line = Line::plain()
        .push(format!("[{}]", tag.label()), SpanRole::Tag)
        .push(
            format!("[{}({:>7})]", ts.format("%H:%M:%S%.3f"), counter),
            SpanRole::Timestamp,
        );
    if !raw.is_empty() {
        line = line.push(format!(" {}", hex_str(raw)), SpanRole::Hex);
    }
    line
}

fn slave_line(slave: u8) -> Line {
    Line::plain()
        .push("  Slave:   ", SpanRole::Label)
        .push(format!("{}", slave), SpanRole::Value)
        .push(format!("(0x{:02X})", slave), SpanRole::Address)
}

fn error_line(msg: &str) -> Line {
    Line::plain()
        .push("  Error: ", SpanRole::Label)
        .push(msg.to_string(), SpanRole::Error)
}

fn hex_str(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

// --- per-PDU detail lines ----------------------------------------------------

/// Lines describing a request PDU (e.g. `  Write Holding Registers: from
/// 0x0000, count 2` plus per-register value lines).
fn request_detail_lines(parsed: &ParsedPdu) -> Vec<Line> {
    let mut out = Vec::new();
    match &parsed.details {
        PduDetails::ReadCoilsReq { start, count } => {
            out.push(detail_line(format!(
                "{}: from 0x{:04X}, count {}",
                function_name(parsed.function),
                start,
                count
            )));
        }
        PduDetails::ReadRegsReq { start, count } => {
            out.push(detail_line(format!(
                "{}: from 0x{:04X}, count {}",
                function_name(parsed.function),
                start,
                count
            )));
        }
        PduDetails::WriteSingleCoilReq { addr, on } => {
            out.push(detail_line(format!(
                "{}: addr 0x{:04X}, {}",
                function_name(parsed.function),
                addr,
                if *on { "ON" } else { "OFF" }
            )));
        }
        PduDetails::WriteSingleRegReq { addr, value } => {
            out.push(detail_line(format!(
                "{}: addr 0x{:04X}, value 0x{:04X}({})",
                function_name(parsed.function),
                addr,
                value,
                value
            )));
        }
        PduDetails::WriteMultipleCoilsReq { start, count, bits } => {
            out.push(detail_line(format!(
                "{}: from 0x{:04X}, count {}",
                function_name(parsed.function),
                start,
                count
            )));
            for (i, b) in bits.iter().enumerate() {
                out.push(reg_line(start + i as u16, if *b { 1u16 } else { 0u16 }));
            }
        }
        PduDetails::WriteMultipleRegsReq { start, count, values } => {
            out.push(detail_line(format!(
                "{}: from 0x{:04X}, count {}",
                function_name(parsed.function),
                start,
                count
            )));
            for (i, v) in values.iter().enumerate() {
                out.push(reg_line(start + i as u16, *v));
            }
        }
        PduDetails::MaskWriteRegReq {
            addr,
            and_mask,
            or_mask,
        } => {
            out.push(detail_line(format!(
                "{}: addr 0x{:04X}, and 0x{:04X}, or 0x{:04X}",
                function_name(parsed.function),
                addr,
                and_mask,
                or_mask
            )));
        }
        PduDetails::ReadWriteMultipleRegsReq {
            read_start,
            read_count,
            write_start,
            write_count,
            write_values,
        } => {
            out.push(detail_line(format!(
                "{}: read 0x{:04X}/{}, write 0x{:04X}/{}",
                function_name(parsed.function),
                read_start,
                read_count,
                write_start,
                write_count
            )));
            for (i, v) in write_values.iter().enumerate() {
                out.push(reg_line(write_start + i as u16, *v));
            }
        }
        PduDetails::Diagnostic { sub, data } => {
            out.push(detail_line(format!(
                "{}: sub 0x{:04X}, {} data bytes",
                function_name(parsed.function),
                sub,
                data.len()
            )));
        }
        PduDetails::EncapsulatedInterface { mei_type, data } => {
            out.push(detail_line(format!(
                "{}: MEI 0x{:02X}, {} data bytes",
                function_name(parsed.function),
                mei_type,
                data.len()
            )));
        }
        PduDetails::Raw { reason } => {
            out.push(error_line(reason));
        }
        // Responses / uncommon variants: show a generic one-liner.
        _ => {
            out.push(detail_line(format!(
                "{} (request view unavailable)",
                function_name(parsed.function)
            )));
        }
    }
    out
}

/// Lines describing a response PDU. Uses the same function-name-first style
/// as request lines (no redundant `Function: Response` prefix). Modbus
/// exception responses get a dedicated two-line layout with the exception
/// code and text highlighted as `Error`.
fn response_detail_lines(parsed: &ParsedPdu) -> Vec<Line> {
    let mut out = Vec::new();
    match &parsed.details {
        PduDetails::Exception { code } => {
            out.push(detail_line(format!(
                "{}: Exception",
                function_name(parsed.function & 0x7F)
            )));
            out.push(error_line(&format!(
                "{} (code {})",
                exception_text(*code),
                code
            )));
        }
        PduDetails::ReadCoilsResp { bits } => {
            out.push(detail_line(format!(
                "{}: {} bits",
                function_name(parsed.function),
                bits.len()
            )));
        }
        PduDetails::ReadRegsResp { values } => {
            out.push(detail_line(format!(
                "{}: {} registers",
                function_name(parsed.function),
                values.len()
            )));
            // Also show the returned values.
            for (i, v) in values.iter().enumerate() {
                out.push(reg_line(i as u16, *v));
            }
        }
        PduDetails::WriteSingleCoilResp { addr, on } => {
            out.push(detail_line(format!(
                "{}: addr 0x{:04X}, {}",
                function_name(parsed.function),
                addr,
                if *on { "ON" } else { "OFF" }
            )));
        }
        PduDetails::WriteSingleRegResp { addr, value } => {
            out.push(detail_line(format!(
                "{}: addr 0x{:04X}, value 0x{:04X}({})",
                function_name(parsed.function),
                addr,
                value,
                value
            )));
        }
        PduDetails::WriteMultipleCoilsResp { start, count } => {
            out.push(detail_line(format!(
                "{}: from 0x{:04X}, count {}",
                function_name(parsed.function),
                start,
                count
            )));
        }
        PduDetails::WriteMultipleRegsResp { start, count } => {
            out.push(detail_line(format!(
                "{}: from 0x{:04X}, count {}",
                function_name(parsed.function),
                start,
                count
            )));
        }
        PduDetails::MaskWriteRegResp {
            addr,
            and_mask,
            or_mask,
        } => {
            out.push(detail_line(format!(
                "{}: addr 0x{:04X}, and 0x{:04X}, or 0x{:04X}",
                function_name(parsed.function),
                addr,
                and_mask,
                or_mask
            )));
        }
        PduDetails::ReadWriteMultipleRegsResp { read_values } => {
            out.push(detail_line(format!(
                "{}: {} registers",
                function_name(parsed.function),
                read_values.len()
            )));
            for (i, v) in read_values.iter().enumerate() {
                out.push(reg_line(i as u16, *v));
            }
        }
        PduDetails::ReadExceptionStatusResp { status } => {
            out.push(detail_line(format!(
                "{}: status 0x{:02X}",
                function_name(parsed.function),
                status
            )));
        }
        PduDetails::CommEventCounterResp {
            status,
            event_count,
        } => {
            out.push(detail_line(format!(
                "{}: status {}, events {}",
                function_name(parsed.function),
                status,
                event_count
            )));
        }
        PduDetails::Raw { reason } => {
            out.push(error_line(reason));
        }
        _ => {
            out.push(detail_line(function_name(parsed.function & 0x7F).to_string()));
        }
    }
    out
}

fn detail_line(text: impl Into<String>) -> Line {
    Line::plain().push(format!("  {}", text.into()), SpanRole::Label)
}

fn reg_line(addr: u16, value: u16) -> Line {
    Line::plain()
        .push("    0x", SpanRole::Label)
        .push(format!("{:04X}", addr), SpanRole::Address)
        .push(": 0x", SpanRole::Label)
        .push(format!("{:04X}", value), SpanRole::Value)
        .push(format!("({})", value), SpanRole::Plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::pdu::{parse_pdu, Role};
    use crate::session::model::Entry;
    use chrono::{TimeZone, Timelike};

    fn ts() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 7, 31, 22, 3, 40)
            .unwrap()
            .with_nanosecond(775_000_000)
            .unwrap()
    }

    fn hex_line_of(entry: &Entry) -> String {
        // The first line is the header; extract its plain text.
        lines_to_plain(&format_entry(entry)).lines().next().unwrap().to_string()
    }

    #[test]
    fn request_block_matches_readme_layout() {
        // FC 0x10 request: slave 2, start 0, count 2, values 0x0000 0x0001
        let mut data = vec![0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01];
        let parsed = parse_pdu(Role::Unknown, 0x10, &data);
        let raw = {
            let mut r = vec![0x02, 0x10];
            r.append(&mut data);
            let c = crate::protocol::crc::crc16(&r);
            r.push((c & 0xFF) as u8);
            r.push((c >> 8) as u8);
            r
        };
        let entry = Entry::request(ts(), 1, raw.clone(), 2, parsed);
        let text = entry_to_plain(&entry);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            format!("[REQUEST ][22:03:40.775(      1)] {}", hex_str(&raw))
        );
        assert_eq!(lines[1], "  Slave:   2(0x02)");
        assert_eq!(lines[2], "  Write Holding Registers: from 0x0000, count 2");
        assert_eq!(lines[3], "    0x0000: 0x0000(0)");
        assert_eq!(lines[4], "    0x0001: 0x0001(1)");
    }

    #[test]
    fn response_block_matches_readme_layout() {
        // FC 0x10 response: slave 2, start 0, count 2
        let data = vec![0x00, 0x00, 0x00, 0x02];
        let parsed = parse_pdu(Role::Unknown, 0x10, &data);
        let raw = {
            let mut r = vec![0x02, 0x10];
            r.extend_from_slice(&data);
            let c = crate::protocol::crc::crc16(&r);
            r.push((c & 0xFF) as u8);
            r.push((c >> 8) as u8);
            r
        };
        let entry = Entry::response(ts(), 1, raw.clone(), 2, parsed);
        let text = entry_to_plain(&entry);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            format!("[RESPONSE][22:03:40.775(      1)] {}", hex_str(&raw))
        );
        assert_eq!(lines[1], "  Slave:   2(0x02)");
        assert_eq!(
            lines[2],
            "  Write Holding Registers: from 0x0000, count 2"
        );
    }

    #[test]
    fn exception_response_block() {
        // FC 0x10 exception response: slave 2, exception code 2 (Illegal Data Address)
        let data = vec![0x02];
        let parsed = parse_pdu(Role::Unknown, 0x90, &data);
        let raw = {
            let mut r = vec![0x02, 0x90];
            r.extend_from_slice(&data);
            let c = crate::protocol::crc::crc16(&r);
            r.push((c & 0xFF) as u8);
            r.push((c >> 8) as u8);
            r
        };
        let entry = Entry::response(ts(), 1, raw.clone(), 2, parsed);
        let text = entry_to_plain(&entry);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            format!("[RESPONSE][22:03:40.775(      1)] {}", hex_str(&raw))
        );
        assert_eq!(lines[1], "  Slave:   2(0x02)");
        assert_eq!(lines[2], "  Write Holding Registers: Exception");
        assert_eq!(lines[3], "  Error: Illegal Data Address (code 2)");
    }

    #[test]
    fn timeout_block_has_no_hex() {
        let entry = Entry::timeout(ts(), 1);
        let text = entry_to_plain(&entry);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "[RESPONSE][22:03:40.775(      1)]");
        assert_eq!(lines[1], "  Error: Timeout");
    }

    #[test]
    fn orphan_block_matches_readme_layout() {
        // Original request: FC 0x10, slave 2, start 0, count 2, values 0/1
        let req_data = vec![0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01];
        let orig = parse_pdu(Role::Unknown, 0x10, &req_data);
        // Late response: FC 0x10, slave 2, start 0, count 2
        let resp_data = vec![0x00, 0x00, 0x00, 0x02];
        let late = parse_pdu(Role::Unknown, 0x10, &resp_data);
        let late_raw = {
            let mut r = vec![0x02, 0x10];
            r.extend_from_slice(&resp_data);
            let c = crate::protocol::crc::crc16(&r);
            r.push((c & 0xFF) as u8);
            r.push((c >> 8) as u8);
            r
        };
        let entry = Entry::orphan(ts(), 1, late_raw.clone(), 2, late, orig);
        let text = entry_to_plain(&entry);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            format!("[ORPHAN  ][22:03:40.775(      1)] {}", hex_str(&late_raw))
        );
        assert_eq!(lines[1], "  Slave:   2(0x02)");
        assert_eq!(lines[2], "  Error: Response Timeout");
        assert_eq!(lines[3], "  Write Holding Registers: from 0x0000, count 2");
        assert_eq!(lines[4], "    0x0000: 0x0000(0)");
        assert_eq!(lines[5], "    0x0001: 0x0001(1)");
    }

    #[test]
    fn parse_failure_block() {
        let entry = Entry::parse_failure(ts(), 1, vec![0x01, 0x02], "Bad CRC (expected 0x1234, computed 0x5678)".into());
        let text = entry_to_plain(&entry);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "[PARSE   ][22:03:40.775(      1)] 01 02");
        assert_eq!(lines[1], "  Error: Bad CRC (expected 0x1234, computed 0x5678)");
        // smoke-check the helper used by the test itself
        let _ = hex_line_of(&Entry::timeout(ts(), 1));
    }
}
