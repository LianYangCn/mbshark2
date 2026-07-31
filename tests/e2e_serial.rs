//! End-to-end test: drive the real `CaptureEngine` over a `socat` PTY pair.
//!
//! This exercises the full pipeline that the unit tests can't reach on their
//! own: `tokio_serial` open → framer inter-frame timing → CRC validation →
//! PDU parse → pairing state machine → emitted `Event`s. Bytes are written to
//! one PTY end; the engine captures the other.
//!
//! Requires `socat` on the host; skipped silently otherwise.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::LocalSet;

use mbshark2::capture::engine::{CaptureEngine, Command as EngineCommand, Event};
use mbshark2::capture::serial::SerialConfig;
use mbshark2::protocol::crc::append_crc;
use mbshark2::session::model::{EntryBody, Tag};

/// Holds the socat process so it is killed on drop.
struct Socat(Option<Child>);
impl Drop for Socat {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn socat_available() -> bool {
    Command::new("socat")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Spawn `socat pty pty` and return the guard plus the two symlink paths.
/// `tag` makes the symlinks unique so parallel tests don't collide.
fn spawn_socat(tag: &str) -> (Socat, String, String) {
    let a = format!("/tmp/mbshark_pty_a_{tag}");
    let b = format!("/tmp/mbshark_pty_b_{tag}");
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let arg_a = format!("pty,raw,echo=0,link={a}");
    let arg_b = format!("pty,raw,echo=0,link={b}");
    let child = Command::new("socat")
        .args([&arg_a, &arg_b])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn socat");
    (Socat(Some(child)), a, b)
}

/// Build a raw RTU frame (slave + fc + data + CRC).
fn frame(slave: u8, fc: u8, data: &[u8]) -> Vec<u8> {
    let mut v = vec![slave, fc];
    v.extend_from_slice(data);
    append_crc(&mut v);
    v
}

/// Wait for the next `Entry` event, skipping Started/Stopped, with a timeout.
async fn next_entry(rx: &mut mpsc::UnboundedReceiver<Event>, timeout_ms: u64) -> mbshark2::session::model::Entry {
    let deadline = Duration::from_millis(timeout_ms);
    loop {
        match tokio::time::timeout(deadline, rx.recv()).await {
            Ok(Some(Event::Entry(e))) => return e,
            Ok(Some(other)) => eprintln!("skip event: {other:?}"),
            Ok(None) => panic!("event channel closed before an Entry arrived"),
            Err(_) => panic!("timed out after {timeout_ms}ms waiting for an Entry"),
        }
    }
}

async fn assert_no_error(rx: &mut mpsc::UnboundedReceiver<Event>, ms: u64) {
    if let Ok(Some(Event::Error(msg))) =
        tokio::time::timeout(Duration::from_millis(ms), rx.recv()).await
    {
        panic!("unexpected capture error: {msg}");
    }
}

fn build_cfg(port: &str, timeout_ms: u64) -> SerialConfig {
    SerialConfig {
        port: port.into(),
        baud: 9600,
        response_timeout: Duration::from_millis(timeout_ms),
        ..Default::default()
    }
}

#[test]
fn e2e_normal_pair() {
    if !socat_available() {
        eprintln!("skip: socat not installed");
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async move {
        let (_socat, pty_a, pty_b) = spawn_socat("normal");
        // Wait for the symlinks to appear.
        for _ in 0..100 {
            if std::path::Path::new(&pty_a).exists() && std::path::Path::new(&pty_b).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>(8);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
        let engine = CaptureEngine::new(cmd_rx, event_tx, Duration::from_millis(300));
        let _engine = tokio::task::spawn_local(engine.run());

        cmd_tx
            .send(EngineCommand::Start(build_cfg(&pty_a, 300)))
            .await
            .unwrap();
        // Give the engine a moment to open the port and start reading.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&pty_b)
            .expect("open pty_b");

        // FC 0x10 request: slave 2, start 0, count 2, values 0x0000 0x0001.
        let req = frame(2, 0x10, &[0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]);
        writer.write_all(&req).unwrap();

        let req_entry = next_entry(&mut event_rx, 1000).await;
        assert_eq!(req_entry.tag, Tag::Request);
        assert_eq!(req_entry.counter, 1);
        assert_eq!(req_entry.raw, req);
        match req_entry.body {
            EntryBody::Frame { slave, .. } => assert_eq!(slave, 2),
            other => panic!("expected Frame body, got {other:?}"),
        }

        // Inter-frame gap (>4ms at 9600 baud), then the response.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let resp = frame(2, 0x10, &[0x00, 0x00, 0x00, 0x02]);
        writer.write_all(&resp).unwrap();

        let resp_entry = next_entry(&mut event_rx, 1000).await;
        assert_eq!(resp_entry.tag, Tag::Response);
        assert_eq!(resp_entry.counter, 1, "response reuses the request counter");
        assert_eq!(resp_entry.raw, resp);

        assert_no_error(&mut event_rx, 200).await;
        let _ = cmd_tx.send(EngineCommand::Shutdown).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
}

#[test]
fn e2e_timeout_then_orphan() {
    if !socat_available() {
        eprintln!("skip: socat not installed");
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async move {
        let (_socat, pty_a, pty_b) = spawn_socat("orphan");
        for _ in 0..100 {
            if std::path::Path::new(&pty_a).exists() && std::path::Path::new(&pty_b).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>(8);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
        // Short timeout so the test doesn't drag.
        let engine = CaptureEngine::new(cmd_rx, event_tx, Duration::from_millis(200));
        let _engine = tokio::task::spawn_local(engine.run());

        cmd_tx
            .send(EngineCommand::Start(build_cfg(&pty_a, 200)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;

        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&pty_b)
            .expect("open pty_b");

        let req = frame(2, 0x10, &[0x00, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x01]);
        writer.write_all(&req).unwrap();

        // Request should be recorded.
        let req_entry = next_entry(&mut event_rx, 1000).await;
        assert_eq!(req_entry.tag, Tag::Request);
        assert_eq!(req_entry.counter, 1);

        // No response → sweeper must emit a synthetic Timeout (counter 1).
        let to_entry = next_entry(&mut event_rx, 1500).await;
        assert_eq!(to_entry.tag, Tag::Response);
        assert_eq!(to_entry.counter, 1);
        assert!(matches!(to_entry.body, EntryBody::Timeout), "expected Timeout");
        assert!(to_entry.raw.is_empty(), "synthetic timeout has no raw bytes");

        // Late response arrives → ORPHAN reusing counter 1.
        let resp = frame(2, 0x10, &[0x00, 0x00, 0x00, 0x02]);
        writer.write_all(&resp).unwrap();
        let orphan_entry = next_entry(&mut event_rx, 1000).await;
        assert_eq!(orphan_entry.tag, Tag::Orphan);
        assert_eq!(orphan_entry.counter, 1, "orphan reuses the original counter");
        assert_eq!(orphan_entry.raw, resp, "orphan hex is the late response's bytes");
        match orphan_entry.body {
            EntryBody::Orphan { slave, .. } => assert_eq!(slave, 2),
            other => panic!("expected Orphan body, got {other:?}"),
        }

        assert_no_error(&mut event_rx, 200).await;
        let _ = cmd_tx.send(EngineCommand::Shutdown).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
}
