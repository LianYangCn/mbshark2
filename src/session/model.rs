//! The display model: one [`Entry`] per captured event.

use chrono::{DateTime, Local};

use crate::protocol::pdu::ParsedPdu;

/// Block tag — the bracketed label at the start of a line (`[REQUEST ]` etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Request,
    Response,
    Orphan,
    Parse,
}

impl Tag {
    /// The fixed-width (8 char) label shown inside the brackets.
    pub fn label(self) -> &'static str {
        match self {
            Tag::Request => "REQUEST ",
            Tag::Response => "RESPONSE",
            Tag::Orphan => "ORPHAN  ",
            Tag::Parse => "PARSE   ",
        }
    }
}

/// What kind of content an entry carries.
#[derive(Debug, Clone)]
pub enum EntryBody {
    /// A normally-parsed frame (request or response).
    Frame { slave: u8, parsed: ParsedPdu },
    /// Synthetic timeout response: no raw bytes, just `  Error: Timeout`.
    /// Emitted by the sweeper when `response_timeout` has elapsed.
    Timeout,
    /// No response: a new request arrived (RTU is half-duplex, so the master
    /// has moved on to the next transaction) before this request was answered.
    /// Like [`EntryBody::Timeout`] but indicates the wait was cut short by bus
    /// turn-around rather than the configured timeout elapsing. No raw bytes.
    NoResponse,
    /// A response that arrived after its request already timed out.
    /// `late` is the late response (its raw bytes are the entry's `raw`);
    /// `orig` is the original request, echoed for context.
    Orphan { slave: u8, late: ParsedPdu, orig: ParsedPdu },
    /// A response with no matching request on record.
    Unsolicited { slave: u8, parsed: ParsedPdu },
    /// A frame that failed to parse (bad CRC, truncated, …).
    ParseFailure { reason: String },
}

/// One display block. The UI renders a list of these; export writes them
/// sequentially as plain text.
#[derive(Debug, Clone)]
pub struct Entry {
    pub tag: Tag,
    pub ts: DateTime<Local>,
    pub counter: u64,
    /// Raw bytes shown on the hex line (empty for [`EntryBody::Timeout`]).
    pub raw: Vec<u8>,
    pub body: EntryBody,
}

impl Entry {
    pub fn request(ts: DateTime<Local>, counter: u64, raw: Vec<u8>, slave: u8, parsed: ParsedPdu) -> Self {
        Entry {
            tag: Tag::Request,
            ts,
            counter,
            raw,
            body: EntryBody::Frame { slave, parsed },
        }
    }

    pub fn response(ts: DateTime<Local>, counter: u64, raw: Vec<u8>, slave: u8, parsed: ParsedPdu) -> Self {
        Entry {
            tag: Tag::Response,
            ts,
            counter,
            raw,
            body: EntryBody::Frame { slave, parsed },
        }
    }

    pub fn timeout(ts: DateTime<Local>, counter: u64) -> Self {
        Entry {
            tag: Tag::Response,
            ts,
            counter,
            raw: Vec::new(),
            body: EntryBody::Timeout,
        }
    }

    /// A request that got no response because a newer request arrived (RTU
    /// half-duplex turn-around) before it was answered.
    pub fn no_response(ts: DateTime<Local>, counter: u64) -> Self {
        Entry {
            tag: Tag::Response,
            ts,
            counter,
            raw: Vec::new(),
            body: EntryBody::NoResponse,
        }
    }

    pub fn orphan(
        ts: DateTime<Local>,
        counter: u64,
        raw: Vec<u8>,
        slave: u8,
        late: ParsedPdu,
        orig: ParsedPdu,
    ) -> Self {
        Entry {
            tag: Tag::Orphan,
            ts,
            counter,
            raw,
            body: EntryBody::Orphan { slave, late, orig },
        }
    }

    pub fn unsolicited(ts: DateTime<Local>, counter: u64, raw: Vec<u8>, slave: u8, parsed: ParsedPdu) -> Self {
        Entry {
            tag: Tag::Orphan,
            ts,
            counter,
            raw,
            body: EntryBody::Unsolicited { slave, parsed },
        }
    }

    pub fn parse_failure(ts: DateTime<Local>, counter: u64, raw: Vec<u8>, reason: String) -> Self {
        Entry {
            tag: Tag::Parse,
            ts,
            counter,
            raw,
            body: EntryBody::ParseFailure { reason },
        }
    }

    /// The slave address this entry belongs to, if it can be determined from
    /// the entry alone. `None` for [`EntryBody::Timeout`] / [`EntryBody::NoResponse`]
    /// (they carry no slave field — the caller resolves them via a
    /// counter→slave map keyed by the matching request).
    pub fn slave(&self) -> Option<u8> {
        match &self.body {
            EntryBody::Frame { slave, .. }
            | EntryBody::Orphan { slave, .. }
            | EntryBody::Unsolicited { slave, .. } => Some(*slave),
            EntryBody::ParseFailure { .. } => self.raw.first().copied(),
            EntryBody::Timeout | EntryBody::NoResponse => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::pdu::{parse_pdu, Role};
    use chrono::TimeZone;

    fn ts() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap()
    }

    fn req_pdu() -> ParsedPdu {
        parse_pdu(Role::Unknown, 0x03, &[0x00, 0x00, 0x00, 0x0A])
    }

    #[test]
    fn slave_for_each_body_variant() {
        let p = req_pdu();
        assert_eq!(Entry::request(ts(), 1, vec![2, 3], 2, p.clone()).slave(), Some(2));
        assert_eq!(Entry::response(ts(), 1, vec![2, 3], 2, p.clone()).slave(), Some(2));
        assert_eq!(Entry::orphan(ts(), 1, vec![2, 3], 2, p.clone(), p.clone()).slave(), Some(2));
        assert_eq!(Entry::unsolicited(ts(), 1, vec![2, 3], 2, p.clone()).slave(), Some(2));
        // Timeout / NoResponse carry no slave on their own.
        assert_eq!(Entry::timeout(ts(), 1).slave(), None);
        assert_eq!(Entry::no_response(ts(), 1).slave(), None);
        // ParseFailure falls back to the first raw byte (the RTU address field).
        assert_eq!(
            Entry::parse_failure(ts(), 1, vec![0x05, 0xFF], "bad".into()).slave(),
            Some(0x05)
        );
        // Empty raw (e.g. a degenerate flush) → None.
        assert_eq!(Entry::parse_failure(ts(), 1, vec![], "empty".into()).slave(), None);
    }
}
