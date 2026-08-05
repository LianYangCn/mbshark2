//! eframe application: spawns the tokio thread, owns the channels, drains
//! events each frame, and renders the settings panel + capture view.

use std::collections::{HashMap, VecDeque};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::capture::engine::{CaptureEngine, Command, Event};
use crate::export;
use crate::render::format::{format_entry, Line};
use crate::render::settings::{SettingsAction, SettingsState};
use crate::render::ui_view::{self, ERROR_RED};
use crate::session::model::Entry;

/// Maximum entries retained in memory (FIFO, drop oldest).
const MAX_ENTRIES: usize = 10_000;

/// Repaint interval while data is flowing (capturing) — fast enough for
/// real-time feel without burning CPU.
const ACTIVE_REPAINT: Duration = Duration::from_millis(100);

/// Repaint interval while idle (no capture, no events) — a long poll to
/// keep CPU near zero.
const IDLE_REPAINT: Duration = Duration::from_millis(500);

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
    /// Cached `counter → slave` map rebuilt only when entries change.
    slave_map_cache: HashMap<u64, u8>,
    settings: SettingsState,
    capturing: bool,
    last_error: Option<String>,
    /// Set when `MBSHARK_AUTOSTART_PORT` requests an automatic capture start.
    autostart_pending: bool,
    /// When set, entries are periodically written here as plain text.
    autoexport_path: Option<std::path::PathBuf>,
    /// Last time auto-export wrote to disk (throttle to every ~1 s).
    last_autoexport: Option<Instant>,
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

        // Set monospace font size once during init so ui_view doesn't
        // need to clone and apply Style every frame.
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, egui::FontId::monospace(13.0));
        cc.egui_ctx.set_global_style(style);

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
            slave_map_cache: HashMap::new(),
            settings,
            capturing: false,
            last_error: None,
            autostart_pending,
            autoexport_path,
            last_autoexport: None,
            port_discovery,
        }
    }

    /// Drain pending events from the channel.
    /// Returns `true` if any entries were added or removed (i.e. the
    /// display data changed), so the caller can request a repaint.
    fn drain_events(&mut self) -> bool {
        let mut entries_changed = false;
        while let Ok(ev) = self.event_rx.try_recv() {
            entries_changed = true;
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
        if entries_changed {
            self.slave_map_cache = crate::render::format::counter_slave_map(&self.entries);
        }
        entries_changed
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let entries_changed = self.drain_events();

        // Finalize background port discovery (first frame only).
        if let Some(handle) = &self.port_discovery {
            if handle.is_finished() {
                if let Ok(ports) = self.port_discovery.take().unwrap().join() {
                    self.settings.ports_cache = ports;
                }
            }
        }

        // Smart repaint: request immediate redraw when new data arrived;
        // use an active interval during capture; drop to idle when quiet.
        if entries_changed {
            ui.ctx().request_repaint();
        } else if self.capturing {
            ui.ctx().request_repaint_after(ACTIVE_REPAINT);
        } else {
            ui.ctx().request_repaint_after(IDLE_REPAINT);
        }

        // Fire the autostart once the engine is ready (first frame).
        if self.autostart_pending && !self.capturing && !self.settings.port.is_empty() {
            let cfg = self.settings.build_config();
            eprintln!("[autostart] starting capture on {}", cfg.port);
            let _ = self.cmd_tx.try_send(Command::Start(cfg));
            self.autostart_pending = false;
        }

        // Auto-export hook: dump current entries to a file every ~1 s.
        // Unfiltered (None = show all) so scripted output stays complete.
        // Uses real elapsed time so it stays ~1 s regardless of repaint cadence.
        if let Some(path) = self.autoexport_path.clone() {
            let now = Instant::now();
            let expired = self
                .last_autoexport
                .map_or(true, |t| now.duration_since(t) >= Duration::from_secs(1));
            if expired {
                export::write_entries(&self.entries, &path, None);
                self.last_autoexport = Some(now);
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
                self.slave_map_cache.clear();
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
                &self.slave_map_cache,
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
