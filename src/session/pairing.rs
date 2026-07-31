//! Request/response pairing state machine — pure functions over
//! [`PairingState`].
//!
//! RTU has no transaction id, so we key pending requests by slave address
//! (Modbus convention: one outstanding request per slave at a time). When a
//! response arrives it is matched to the pending request for that slave; if
//! the request already timed out, the late response becomes an ORPHAN. **No
//! received frame is ever discarded** — see [`on_frame`].

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use chrono::{DateTime, Local};

use crate::protocol::frame::ModbusFrame;
use crate::protocol::pdu::{ParsedPdu, Role};
use crate::session::model::Entry;

/// Maximum number of timed-out requests retained per slave for late-response
/// matching. Overflow drops the oldest *association* only — the late response
/// itself would still be displayed (as an unsolicited ORPHAN).
const TIMED_OUT_PER_SLAVE: usize = 32;

/// A request awaiting a response (or, after timeout, awaiting a late one).
#[derive(Debug, Clone)]
pub struct PendingReq {
    pub opened_at: DateTime<Local>,
    pub counter: u64,
    pub slave: u8,
    pub parsed: ParsedPdu,
    pub raw: Vec<u8>,
}

#[derive(Debug)]
pub struct PairingState {
    pub next_counter: u64,
    pub response_timeout: Duration,
    pub pending: HashMap<u8, PendingReq>,
    pub timed_out: HashMap<u8, VecDeque<PendingReq>>,
}

impl PairingState {
    pub fn new(response_timeout: Duration) -> Self {
        PairingState {
            next_counter: 1,
            response_timeout,
            pending: HashMap::new(),
            timed_out: HashMap::new(),
        }
    }

    /// Reset all state (used on Stop so the next Start is clean).
    pub fn reset(&mut self) {
        self.next_counter = 1;
        self.pending.clear();
        self.timed_out.clear();
    }
}

fn push_timed_out(timed_out: &mut HashMap<u8, VecDeque<PendingReq>>, slave: u8, req: PendingReq) {
    let q = timed_out.entry(slave).or_default();
    q.push_back(req);
    while q.len() > TIMED_OUT_PER_SLAVE {
        q.pop_front();
    }
}

fn pop_timed_out(timed_out: &mut HashMap<u8, VecDeque<PendingReq>>, slave: u8) -> Option<PendingReq> {
    let q = timed_out.get_mut(&slave)?;
    let req = q.pop_front();
    if q.is_empty() {
        timed_out.remove(&slave);
    }
    req
}

/// Process one validated frame. Returns the updated state and any entries to
/// emit.
pub fn on_frame(
    mut state: PairingState,
    frame: ModbusFrame,
    parsed: ParsedPdu,
    now: DateTime<Local>,
) -> (PairingState, Vec<Entry>) {
    let mut out = Vec::new();
    let slave = frame.slave;
    let raw = frame.raw.clone();
    let ts = frame.timestamp;

    // Resolve role: exception responses are always responses; otherwise use
    // the inferred role, falling back to context (a pending request implies
    // the next frame for that slave is its response).
    let role = match parsed.role {
        Role::Request => Role::Request,
        Role::Response => Role::Response,
        Role::Unknown => {
            if state.pending.contains_key(&slave) {
                Role::Response
            } else {
                Role::Request
            }
        }
    };

    match role {
        Role::Request => {
            // Overlapping request (master violated one-outstanding, or we
            // missed a response): synthesize a timeout for the old one first.
            if let Some(old) = state.pending.remove(&slave) {
                out.push(Entry::timeout(now, old.counter));
                push_timed_out(&mut state.timed_out, slave, old);
            }
            let counter = state.next_counter;
            state.next_counter += 1;
            let req = PendingReq {
                opened_at: ts,
                counter,
                slave,
                parsed: parsed.clone(),
                raw: raw.clone(),
            };
            state.pending.insert(slave, req);
            out.push(Entry::request(ts, counter, raw, slave, parsed));
        }
        Role::Response => {
            if let Some(req) = state.pending.remove(&slave) {
                out.push(Entry::response(ts, req.counter, raw, slave, parsed));
            } else if let Some(orig) = pop_timed_out(&mut state.timed_out, slave) {
                out.push(Entry::orphan(ts, orig.counter, raw, slave, parsed, orig.parsed));
            } else {
                let counter = state.next_counter;
                state.next_counter += 1;
                out.push(Entry::unsolicited(ts, counter, raw, slave, parsed));
            }
        }
        Role::Unknown => unreachable!("role resolved above"),
    }

    (state, out)
}

/// Periodic sweep: expire pending requests whose response_timeout has
/// elapsed, emitting synthetic timeout entries and moving them to the
/// timed-out pool for possible late-response matching.
pub fn sweep(mut state: PairingState, now: DateTime<Local>) -> (PairingState, Vec<Entry>) {
    let mut out = Vec::new();
    let limit_ms = state.response_timeout.as_millis() as i64;

    let expired: Vec<u8> = state
        .pending
        .iter()
        .filter(|(_, r)| now.signed_duration_since(r.opened_at).num_milliseconds() > limit_ms)
        .map(|(&s, _)| s)
        .collect();

    for slave in expired {
        if let Some(req) = state.pending.remove(&slave) {
            out.push(Entry::timeout(now, req.counter));
            push_timed_out(&mut state.timed_out, slave, req);
        }
    }

    (state, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::pdu::parse_pdu;
    use crate::session::model::{EntryBody, Tag};
    use chrono::TimeZone;

    fn pdu_req(fc: u8, data: &[u8]) -> ParsedPdu {
        parse_pdu(Role::Unknown, fc, data)
    }

    /// Build a frame carrying `payload` (slave+fc+data, no CRC) with a valid
    /// CRC and the given timestamp.
    fn frame(slave: u8, fc: u8, data: &[u8], ts: DateTime<Local>) -> (ModbusFrame, ParsedPdu) {
        let mut raw = vec![slave, fc];
        raw.extend_from_slice(data);
        let c = crate::protocol::crc::crc16(&raw);
        raw.push((c & 0xFF) as u8);
        raw.push((c >> 8) as u8);
        let f = ModbusFrame::from_bytes(raw, ts).unwrap();
        let p = parse_pdu(Role::Unknown, fc, &f.data);
        (f, p)
    }

    fn t0() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 7, 31, 22, 3, 40).unwrap()
    }

    #[test]
    fn normal_pair() {
        let st = PairingState::new(Duration::from_millis(500));
        let (st, out) = on_frame(st, frame(2, 0x10, &[0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01], t0()).0, pdu_req(0x10, &[0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]), t0());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tag, Tag::Request);
        assert_eq!(out[0].counter, 1);

        let (st, out2) = on_frame(st, frame(2, 0x10, &[0x00, 0x00, 0x00, 0x02], t0()).0, pdu_req(0x10, &[0x00, 0x00, 0x00, 0x02]), t0());
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].tag, Tag::Response);
        assert_eq!(out2[0].counter, 1); // same counter as its request
        assert!(st.pending.is_empty());
    }

    #[test]
    fn timeout_then_orphan() {
        let st = PairingState::new(Duration::from_millis(500));
        let (st, out) = on_frame(st, frame(2, 0x10, &[0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01], t0()).0, pdu_req(0x10, &[0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]), t0());
        assert_eq!(out[0].counter, 1);

        // After timeout, sweep emits a synthetic timeout response.
        let later = t0() + chrono::Duration::milliseconds(600);
        let (st, out2) = sweep(st, later);
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].tag, Tag::Response);
        assert!(matches!(out2[0].body, EntryBody::Timeout));
        assert_eq!(out2[0].counter, 1);
        assert!(st.pending.is_empty());
        assert!(st.timed_out.contains_key(&2));

        // Late response arrives → ORPHAN with the original counter.
        let (st, out3) = on_frame(st, frame(2, 0x10, &[0x00, 0x00, 0x00, 0x02], later).0, pdu_req(0x10, &[0x00, 0x00, 0x00, 0x02]), later);
        assert_eq!(out3.len(), 1);
        assert_eq!(out3[0].tag, Tag::Orphan);
        assert_eq!(out3[0].counter, 1); // reuses original request counter
        assert!(st.timed_out.is_empty());
    }

    #[test]
    fn unsolicited_response() {
        let st = PairingState::new(Duration::from_millis(500));
        // A response with no prior request and no timed-out entry. FC 0x03
        // response shape: byte_count=4 + two registers (0x0000, 0x1234).
        let (_st, out) = on_frame(
            st,
            frame(2, 0x03, &[0x04, 0x00, 0x00, 0x12, 0x34], t0()).0,
            pdu_req(0x03, &[0x04, 0x00, 0x00, 0x12, 0x34]),
            t0(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tag, Tag::Orphan);
        assert!(matches!(out[0].body, EntryBody::Unsolicited { .. }));
        assert_eq!(out[0].counter, 1);
    }

    #[test]
    fn exception_response_pairs() {
        let st = PairingState::new(Duration::from_millis(500));
        let (st, _) = on_frame(st, frame(2, 0x03, &[0x00, 0x00, 0x00, 0x0A], t0()).0, pdu_req(0x03, &[0x00, 0x00, 0x00, 0x0A]), t0());
        // Exception response: fc 0x83, code 2.
        let (f, p) = frame(2, 0x83, &[0x02], t0());
        let (st, out) = on_frame(st, f, p, t0());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tag, Tag::Response);
        assert_eq!(out[0].counter, 1);
        assert!(st.pending.is_empty());
    }

    #[test]
    fn overlapping_request_forces_timeout() {
        let st = PairingState::new(Duration::from_millis(500));
        let (st, _) = on_frame(st, frame(2, 0x03, &[0x00, 0x00, 0x00, 0x0A], t0()).0, pdu_req(0x03, &[0x00, 0x00, 0x00, 0x0A]), t0());
        // Second request for same slave before the first is answered.
        let (st, out) = on_frame(st, frame(2, 0x03, &[0x00, 0x10, 0x00, 0x01], t0()).0, pdu_req(0x03, &[0x00, 0x10, 0x00, 0x01]), t0());
        // Expect: a synthetic timeout for counter 1, then a new request counter 2.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].counter, 1);
        assert!(matches!(out[0].body, EntryBody::Timeout));
        assert_eq!(out[1].counter, 2);
        assert_eq!(out[1].tag, Tag::Request);
        assert!(st.timed_out.contains_key(&2));
    }

    #[test]
    fn sweep_keeps_pending_within_timeout() {
        let st = PairingState::new(Duration::from_millis(500));
        let (st, _) = on_frame(st, frame(2, 0x03, &[0x00, 0x00, 0x00, 0x0A], t0()).0, pdu_req(0x03, &[0x00, 0x00, 0x00, 0x0A]), t0());
        let (st, out) = sweep(st, t0() + chrono::Duration::milliseconds(100));
        assert!(out.is_empty());
        assert!(st.pending.contains_key(&2));
    }

    #[test]
    fn reset_clears_state() {
        let mut st = PairingState::new(Duration::from_millis(500));
        st.next_counter = 42;
        st.pending.insert(2, PendingReq {
            opened_at: t0(),
            counter: 1,
            slave: 2,
            parsed: pdu_req(0x03, &[0x00, 0x00, 0x00, 0x0A]),
            raw: vec![],
        });
        st.reset();
        assert_eq!(st.next_counter, 1);
        assert!(st.pending.is_empty());
        assert!(st.timed_out.is_empty());
    }
}
