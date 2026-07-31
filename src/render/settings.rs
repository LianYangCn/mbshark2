//! Serial settings panel: port / baud / framing / timeout + Start/Stop/Clear/Export.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_serial::{DataBits, FlowControl, Parity, StopBits};

use crate::capture::engine::Command;
use crate::capture::serial::{available_port_names, SerialConfig};
use crate::export;
use crate::session::model::Entry;

/// Actions the settings panel requests from the app.
#[derive(Debug, Default)]
pub enum SettingsAction {
    #[default]
    None,
    /// Clear all captured entries (and their cached lines).
    Clear,
}

/// Editable serial settings + cached port list.
#[derive(Debug, Clone)]
pub struct SettingsState {
    pub port: String,
    pub baud: String,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub flow_control: String,
    pub timeout_ms: u64,
    pub ports_cache: Vec<String>,
    pub auto_scroll: bool,
}

impl Default for SettingsState {
    fn default() -> Self {
        SettingsState {
            port: String::new(),
            baud: "9600".into(),
            data_bits: 8,
            parity: "None".into(),
            stop_bits: 1,
            flow_control: "None".into(),
            timeout_ms: 500,
            ports_cache: Vec::new(),
            auto_scroll: true,
        }
    }
}

impl SettingsState {
    /// Refresh the cached list of available serial port names.
    pub fn refresh_ports(&mut self) {
        self.ports_cache = available_port_names();
    }

    /// Build a `SerialConfig` from the current selections.
    pub fn build_config(&self) -> SerialConfig {
        SerialConfig {
            port: self.port.clone(),
            baud: self.baud.parse().unwrap_or(9600),
            data_bits: match self.data_bits {
                5 => DataBits::Five,
                6 => DataBits::Six,
                7 => DataBits::Seven,
                _ => DataBits::Eight,
            },
            parity: match self.parity.as_str() {
                "Even" => Parity::Even,
                "Odd" => Parity::Odd,
                _ => Parity::None,
            },
            stop_bits: if self.stop_bits == 2 {
                StopBits::Two
            } else {
                StopBits::One
            },
            flow_control: match self.flow_control.as_str() {
                "Software" => FlowControl::Software,
                "Hardware" => FlowControl::Hardware,
                _ => FlowControl::None,
            },
            response_timeout: Duration::from_millis(self.timeout_ms.max(10)),
        }
    }

    /// Render the panel. Returns a [`SettingsAction`] for the app to apply.
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        capturing: bool,
        cmd_tx: &mpsc::Sender<Command>,
        entries: &VecDeque<Entry>,
    ) -> SettingsAction {
        ui.heading("Serial Settings");

        // Port: editable text + dropdown of detected ports (PTY/symlink paths
        // that `available_ports()` doesn't list can be typed in by hand).
        ui.horizontal(|ui| {
            ui.label("Port:");
            ui.add(
                egui::TextEdit::singleline(&mut self.port)
                    .desired_width(160.0)
                    .hint_text("/dev/ttyUSB0"),
            );
            egui::ComboBox::from_label("")
                .selected_text(if self.port.is_empty() { "<select>" } else { &self.port })
                .show_ui(ui, |ui| {
                    for p in &self.ports_cache {
                        ui.selectable_value(&mut self.port, p.clone(), p);
                    }
                });
            if ui.button("Refresh").clicked() {
                self.refresh_ports();
            }
        });

        // Baud
        ui.horizontal(|ui| {
            ui.label("Baud:");
            egui::ComboBox::from_label("baud")
                .selected_text(self.baud.clone())
                .show_ui(ui, |ui| {
                    for b in ["9600", "19200", "38400", "57600", "115200", "230400"] {
                        ui.selectable_value(&mut self.baud, b.to_string(), b);
                    }
                });
        });

        // Data bits
        ui.horizontal(|ui| {
            ui.label("Data bits:");
            egui::ComboBox::from_label("data_bits")
                .selected_text(format!("{}", self.data_bits))
                .show_ui(ui, |ui| {
                    for d in [5u8, 6, 7, 8] {
                        ui.selectable_value(&mut self.data_bits, d, format!("{d}"));
                    }
                });
        });

        // Parity
        ui.horizontal(|ui| {
            ui.label("Parity:");
            egui::ComboBox::from_label("parity")
                .selected_text(self.parity.clone())
                .show_ui(ui, |ui| {
                    for p in ["None", "Even", "Odd"] {
                        ui.selectable_value(&mut self.parity, p.to_string(), p);
                    }
                });
        });

        // Stop bits
        ui.horizontal(|ui| {
            ui.label("Stop bits:");
            egui::ComboBox::from_label("stop_bits")
                .selected_text(format!("{}", self.stop_bits))
                .show_ui(ui, |ui| {
                    for s in [1u8, 2] {
                        ui.selectable_value(&mut self.stop_bits, s, format!("{s}"));
                    }
                });
        });

        // Flow control
        ui.horizontal(|ui| {
            ui.label("Flow control:");
            egui::ComboBox::from_label("flow_control")
                .selected_text(self.flow_control.clone())
                .show_ui(ui, |ui| {
                    for f in ["None", "Software", "Hardware"] {
                        ui.selectable_value(&mut self.flow_control, f.to_string(), f);
                    }
                });
        });

        // Response timeout
        ui.horizontal(|ui| {
            ui.label("Timeout:");
            ui.add(
                egui::DragValue::new(&mut self.timeout_ms)
                    .range(10..=60_000)
                    .suffix(" ms"),
            );
        });

        ui.separator();

        // Start / Stop
        ui.horizontal(|ui| {
            if capturing {
                if ui.button("⏹ Stop").clicked() {
                    let _ = cmd_tx.try_send(Command::Stop);
                }
            } else {
                let can_start = !self.port.is_empty();
                ui.add_enabled_ui(can_start, |ui| {
                    if ui.button("▶ Start").clicked() {
                        let cfg = self.build_config();
                        let _ = cmd_tx.try_send(Command::Start(cfg));
                    }
                });
            }
        });

        let mut action = SettingsAction::None;
        ui.horizontal(|ui| {
            if ui.button("🗑 Clear").clicked() {
                action = SettingsAction::Clear;
            }
            if ui.button("💾 Export…").clicked() {
                export::export_entries(entries);
            }
        });

        ui.separator();
        ui.label(format!("Entries: {}", entries.len()));

        action
    }
}
