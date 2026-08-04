//! eframe application: spawns the tokio thread, owns the channels, drains
//! events each frame, and renders the settings panel + capture view.

use std::collections::VecDeque;
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::capture::engine::{CaptureEngine, Command, Event};
use crate::export;
use crate::render::format::{format_entry, Line};
use crate::render::settings::{SettingsAction, SettingsState};
use crate::render::ui_view::{self, ERROR_RED};
use crate::session::model::Entry;

/// Maximum entries retained in memory (FIFO, drop oldest).
const MAX_ENTRIES: usize = 10_000;

/// Repaint cadence so the UI keeps draining the event channel while idle.
const REPAINT_INTERVAL: Duration = Duration::from_millis(50);

pub fn run() {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(8);
    let (event_tx, event_rx) = mpsc::unbounded_channel::<Event>();

    let tokio_handle = thread::Builder::new()
        .name("mbshark2-tokio".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime");
            rt.block_on(async move {
                let engine = CaptureEngine::new(cmd_rx, event_tx, Duration::from_millis(500));
                engine.run().await;
            });
        })
        .expect("spawn tokio thread");

    let mut viewport = egui::ViewportBuilder::default().with_inner_size([1000.0, 700.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../mbshark2.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let cmd_tx_for_shutdown = cmd_tx.clone();
    let result = eframe::run_native(
        "mbshark2",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cmd_tx, event_rx, cc)))),
    );

    // After the window closes: tell the engine to shut down and join.
    let _ = cmd_tx_for_shutdown.try_send(Command::Shutdown);
    let _ = tokio_handle.join();

    if let Err(e) = result {
        eprintln!("mbshark2: {e}");
    }
}

struct App {
    cmd_tx: mpsc::Sender<Command>,
    event_rx: mpsc::UnboundedReceiver<Event>,
    entries: VecDeque<Entry>,
    lines_cache: VecDeque<Vec<Line>>,
    settings: SettingsState,
    capturing: bool,
    last_error: Option<String>,
    /// Set when `MBSHARK_AUTOSTART_PORT` requests an automatic capture start.
    autostart_pending: bool,
    /// When set, entries are periodically written here as plain text.
    autoexport_path: Option<std::path::PathBuf>,
    /// Frame counter to throttle auto-export (every ~1 s at 50 ms repaint).
    autoexport_tick: u32,
    /// Background port discovery so the window opens instantly.
    port_discovery: Option<std::thread::JoinHandle<Vec<String>>>,
}

impl App {
    fn new(
        cmd_tx: mpsc::Sender<Command>,
        event_rx: mpsc::UnboundedReceiver<Event>,
        cc: &eframe::CreationContext<'_>,
    ) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgb(0x0d, 0x11, 0x17);
        cc.egui_ctx.set_visuals(visuals);

        let mut settings = SettingsState::default();

        // Enumerate serial ports on a background thread so the window
        // opens instantly. Ports will show up in the dropdown once the
        // thread completes (typically within one repaint cycle).
        let port_discovery = Some(std::thread::spawn(|| {
            crate::capture::serial::available_port_names()
        }));

        // Load persisted config first; the autostart env var below still wins
        // for the port (env > config > defaults).
        if let Some(cfg) = crate::config::load() {
            settings.apply_persisted(&cfg);
        }

        // Scriptability hook: `MBSHARK_AUTOSTART_PORT=/path` pre-fills the port
        // and starts capture on the first frame, so automated tests don't need
        // to drive the GUI with xdotool just to begin capturing.
        let autostart_pending = if let Ok(port) = std::env::var("MBSHARK_AUTOSTART_PORT") {
            if !port.is_empty() {
                settings.port = port;
                true
            } else {
                false
            }
        } else {
            false
        };

        // Scriptability hook: `MBSHARK_AUTOEXPORT_PATH=/path.txt` writes the
        // current entries as plain text every ~1 s, so automated tests can
        // verify capture output without interacting with the export dialog.
        let autoexport_path = std::env::var("MBSHARK_AUTOEXPORT_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from);

        App {
            cmd_tx,
            event_rx,
            entries: VecDeque::new(),
            lines_cache: VecDeque::new(),
            settings,
            capturing: false,
            last_error: None,
            autostart_pending,
            autoexport_path,
            autoexport_tick: 0,
            port_discovery,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.event_rx.try_recv() {
            match ev {
                Event::Entry(entry) => {
                    let lines = format_entry(&entry);
                    self.entries.push_back(entry);
                    self.lines_cache.push_back(lines);
                    while self.entries.len() > MAX_ENTRIES {
                        self.entries.pop_front();
                        self.lines_cache.pop_front();
                    }
                }
                Event::Started => {
                    self.capturing = true;
                    self.last_error = None;
                }
                Event::Stopped => {
                    self.capturing = false;
                }
                Event::Error(msg) => {
                    eprintln!("[capture] error: {msg}");
                    self.last_error = Some(msg);
                }
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();

        // Finalize background port discovery (first frame only).
        if let Some(handle) = &self.port_discovery {
            if handle.is_finished() {
                if let Ok(ports) = self.port_discovery.take().unwrap().join() {
                    self.settings.ports_cache = ports;
                }
                // Port discovery is done — Refresh button will re-run
                // synchronously on user request (slow, but explicit).
            }
        }
        ui.ctx().request_repaint_after(REPAINT_INTERVAL);

        // Fire the autostart once the engine is ready (first frame).
        if self.autostart_pending && !self.capturing && !self.settings.port.is_empty() {
            let cfg = self.settings.build_config();
            eprintln!("[autostart] starting capture on {}", cfg.port);
            let _ = self.cmd_tx.try_send(Command::Start(cfg));
            self.autostart_pending = false;
        }

        // Auto-export hook: dump current entries to a file every ~1 s.
        // Unfiltered (None = show all) so scripted output stays complete.
        if let Some(path) = self.autoexport_path.clone() {
            self.autoexport_tick = self.autoexport_tick.wrapping_add(1);
            if self.autoexport_tick.is_multiple_of(20) {
                export::write_entries(&self.entries, &path, None);
            }
        }

        // Read-only copies for the settings closure (avoids multi-borrow of self).
        let capturing = self.capturing;
        let cmd_tx = self.cmd_tx.clone();

        let mut action = SettingsAction::None;
        egui::Panel::left("settings_panel")
            .resizable(true)
            .default_size(280.0)
            .show_inside(ui, |ui| {
                action = self.settings.render(ui, capturing, &cmd_tx, &self.entries);
            });

        match action {
            SettingsAction::Clear => {
                self.entries.clear();
                self.lines_cache.clear();
            }
            SettingsAction::SaveConfig => {
                crate::config::save(&self.settings.to_persisted());
            }
            SettingsAction::None => {}
        }

        egui::CentralPanel::default().show_inside(ui, |ui| {
            if let Some(err) = &self.last_error {
                ui.colored_label(ERROR_RED, format!("⚠ {err}"));
                ui.separator();
            }
            let show_set = self.settings.show_set();
            ui_view::show(
                ui,
                &self.entries,
                &self.lines_cache,
                self.settings.auto_scroll && self.capturing,
                show_set.as_ref(),
            );
        });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.cmd_tx.try_send(Command::Shutdown);
        // Auto-save on exit so the user's last settings persist.
        crate::config::save(&self.settings.to_persisted());
    }
}
