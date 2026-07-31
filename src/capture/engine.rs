//! Async capture engine: a `tokio::select!` loop that orchestrates the
//! serial reader, the RTU framer, the pairing state machine, and a periodic
//! timeout sweep.
//!
//! Everything runs on a single `current_thread` tokio runtime (one OS thread).
//! `tokio_serial::SerialStream` is `!Send` — that's fine here because the
//! runtime never moves the future across threads.

use std::time::{Duration, Instant};

use chrono::Local;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::time::{self, Instant as TokioInstant};

use crate::capture::framer::Framer;
use crate::capture::serial::{open_stream, SerialConfig};
use crate::protocol::frame::ModbusFrame;
use crate::protocol::pdu::{parse_pdu, Role};
use crate::session::model::Entry;
use crate::session::pairing::{on_frame, sweep, PairingState};

/// UI → tokio commands (sent with `try_send`, never blocking).
#[derive(Debug)]
pub enum Command {
    Start(SerialConfig),
    Stop,
    Shutdown,
}

/// tokio → UI events. Sent on an unbounded channel so capture never blocks on
/// the UI; `UnboundedReceiver::try_recv()` works without a runtime context.
#[derive(Debug)]
pub enum Event {
    /// A display entry (request/response/orphan/parse-failure/timeout).
    Entry(Entry),
    /// A capture-level error (port open failure, read error, …).
    Error(String),
    /// Capture started successfully (port opened).
    Started,
    /// Capture stopped (Stop, error, or port closed).
    Stopped,
}

/// How often the timeout sweeper runs.
const SWEEP_INTERVAL: Duration = Duration::from_millis(50);

pub struct CaptureEngine {
    cmd_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::UnboundedSender<Event>,
    state: PairingState,
    /// Set when a `Shutdown` command arrives; the outer loop checks it.
    shutdown: bool,
}

impl CaptureEngine {
    pub fn new(
        cmd_rx: mpsc::Receiver<Command>,
        event_tx: mpsc::UnboundedSender<Event>,
        response_timeout: Duration,
    ) -> Self {
        CaptureEngine {
            cmd_rx,
            event_tx,
            state: PairingState::new(response_timeout),
            shutdown: false,
        }
    }

    /// Outer loop: wait for `Start`, run capture until it ends, repeat.
    pub async fn run(mut self) {
        let mut pending: Option<SerialConfig> = None;
        loop {
            let cfg = if let Some(c) = pending.take() {
                c
            } else {
                match self.cmd_rx.recv().await {
                    Some(Command::Start(c)) => c,
                    Some(Command::Stop) => continue,
                    Some(Command::Shutdown) | None => break,
                }
            };

            self.event_tx.send(Event::Started).ok();
            let stream = match open_stream(&cfg) {
                Ok(s) => s,
                Err(e) => {
                    self.event_tx
                        .send(Event::Error(format!("open {}: {}", cfg.port, e)))
                        .ok();
                    self.event_tx.send(Event::Stopped).ok();
                    if self.shutdown {
                        break;
                    }
                    continue;
                }
            };

            // A fresh `Start` always begins from a clean pairing state.
            self.state = PairingState::new(cfg.response_timeout);
            pending = self.capture(stream, &cfg).await;
            self.event_tx.send(Event::Stopped).ok();
            if self.shutdown {
                break;
            }
        }
    }

    /// Inner capture loop. Returns `Some(cfg)` if a `Start` arrived mid-capture
    /// (restart with that config), `None` for Stop / Shutdown / EOF / error.
    async fn capture(
        &mut self,
        mut stream: tokio_serial::SerialStream,
        cfg: &SerialConfig,
    ) -> Option<SerialConfig> {
        let mut framer = Framer::new(cfg.baud);
        let mut buf = [0u8; 256];
        let mut sweep_ticker = time::interval(SWEEP_INTERVAL);
        // Discard the immediate first tick so we get steady 50ms sweeps.
        sweep_ticker.tick().await;

        loop {
            // Compute the gap deadline *before* the select so the async block
            // owns it by value and doesn't borrow `framer` (which the body
            // needs to mutate). When the buffer is empty we use a pending
            // future to avoid a busy loop on a past deadline.
            let gap_deadline = framer.gap_deadline();

            tokio::select! {
                biased;

                cmd = self.cmd_rx.recv() => {
                    let reset = match cmd {
                        Some(Command::Stop) | None => true,
                        Some(Command::Shutdown) => {
                            self.shutdown = true;
                            true
                        }
                        Some(Command::Start(new_cfg)) => {
                            framer.reset();
                            self.state.reset();
                            return Some(new_cfg);
                        }
                    };
                    if reset {
                        framer.reset();
                        self.state.reset();
                        return None;
                    }
                }

                n = stream.read(&mut buf) => {
                    match n {
                        Ok(0) => {
                            // Serial EOF — port closed under us.
                            framer.reset();
                            self.state.reset();
                            return None;
                        }
                        Ok(n) => {
                            framer.push(&buf[..n], Instant::now());
                            if framer.overflowed() {
                                let raw = framer.flush_due();
                                self.process_frame_raw(raw);
                            }
                        }
                        Err(e) => {
                            self.emit_error(format!("read: {e}"));
                            framer.reset();
                            self.state.reset();
                            return None;
                        }
                    }
                }

                _ = async move {
                    match gap_deadline {
                        Some(d) => time::sleep_until(TokioInstant::from_std(d)).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    if framer.buffer_len() > 0 {
                        let raw = framer.flush_due();
                        self.process_frame_raw(raw);
                    }
                }

                _ = sweep_ticker.tick() => {
                    self.do_sweep();
                }
            }
        }
    }

    /// Validate a candidate frame's bytes and feed it through the pairing
    /// state machine, emitting any resulting entries. CRC/structural failures
    /// become `ParseFailure` entries (still shown — nothing is discarded).
    fn process_frame_raw(&mut self, raw: Vec<u8>) {
        let now = Local::now();
        match ModbusFrame::from_bytes(raw, now) {
            Ok(frame) => {
                let parsed = parse_pdu(Role::Unknown, frame.function, &frame.data);
                let timeout = self.state.response_timeout;
                let state = std::mem::replace(&mut self.state, PairingState::new(timeout));
                let (new_state, entries) = on_frame(state, frame, parsed, now);
                self.state = new_state;
                self.emit_entries(entries);
            }
            Err(err) => {
                let counter = self.state.next_counter;
                self.state.next_counter += 1;
                let entry = Entry::parse_failure(now, counter, err.raw().to_vec(), err.reason());
                self.event_tx.send(Event::Entry(entry)).ok();
            }
        }
    }

    /// Expire pending requests whose response_timeout has elapsed.
    fn do_sweep(&mut self) {
        let now = Local::now();
        let timeout = self.state.response_timeout;
        let state = std::mem::replace(&mut self.state, PairingState::new(timeout));
        let (new_state, entries) = sweep(state, now);
        self.state = new_state;
        self.emit_entries(entries);
    }

    fn emit_entries(&self, entries: Vec<Entry>) {
        for e in entries {
            let _ = self.event_tx.send(Event::Entry(e));
        }
    }

    fn emit_error(&self, msg: String) {
        let _ = self.event_tx.send(Event::Error(msg));
    }
}
