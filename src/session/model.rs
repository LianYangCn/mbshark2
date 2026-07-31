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
    Timeout,
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
}
