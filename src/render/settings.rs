//! Serial settings panel: port / baud / framing / timeout + Start/Stop/Clear/Export.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_serial::{DataBits, FlowControl, Parity, StopBits};

use crate::capture::engine::Command;
use crate::capture::serial::{available_port_names, SerialConfig};
use crate::config::PersistedConfig;
use crate::export;
use crate::session::model::Entry;

/// Actions the settings panel requests from the app.
#[derive(Debug, Default)]
pub enum SettingsAction {
    #[default]
    None,
    /// Clear all captured entries (and their cached lines).
    Clear,
    /// Persist the current settings to the config file.
    SaveConfig,
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
    /// Slave-address filter: empty or `"*"` = show all; otherwise only
    /// matching slaves listed (supports `1,2,3` and `1-3` range syntax).
    /// Parsed on demand by [`SettingsState::show_set`].
    pub show_slaves_str: String,
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
            show_slaves_str: String::new(),
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

    /// Snapshot the persistable fields into a TOML-serializable config.
    pub fn to_persisted(&self) -> PersistedConfig {
        let mut show: Vec<u8> = self.show_set().into_iter().flatten().collect();
        show.sort_unstable();
        PersistedConfig {
            port: self.port.clone(),
            baud: self.baud.clone(),
            data_bits: self.data_bits,
            parity: self.parity.clone(),
            stop_bits: self.stop_bits,
            flow_control: self.flow_control.clone(),
            timeout_ms: self.timeout_ms,
            auto_scroll: self.auto_scroll,
            show_slaves: show,
        }
    }

    /// Apply a loaded config onto this state (used on startup).
    pub fn apply_persisted(&mut self, cfg: &PersistedConfig) {
        self.port = cfg.port.clone();
        self.baud = cfg.baud.clone();
        self.data_bits = cfg.data_bits;
        self.parity = cfg.parity.clone();
        self.stop_bits = cfg.stop_bits;
        self.flow_control = cfg.flow_control.clone();
        self.timeout_ms = cfg.timeout_ms;
        self.auto_scroll = cfg.auto_scroll;
        // Canonical form: compact ranges when possible.
        self.show_slaves_str = Self::canonicalize_slaves(&cfg.show_slaves);
    }

    /// Parse `show_slaves_str` into an optional slave-address filter.
    ///
    /// - `""` or `"*"` → `None` (show everything).
    /// - Otherwise returns `Some(set)` containing only the listed addresses.
    ///
    /// Supported syntax:
    /// - single: `"1,2,3"`
    /// - range: `"1-3"` (inclusive, order-agnostic)
    /// - combined: `"1-3,5,7-9"`
    ///
    /// Invalid tokens (non-numeric, out of u8 range) are silently skipped;
    /// duplicates automatically collapse into the set.
    /// An empty set after parsing (all garbage tokens) also returns `None`.
    pub fn show_set(&self) -> Option<HashSet<u8>> {
        let trimmed = self.show_slaves_str.trim();
        if trimmed.is_empty() || trimmed == "*" {
            return None;
        }
        let mut set = HashSet::new();
        for token in trimmed.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if let Some((start_str, end_str)) = token.split_once('-') {
                let start: u8 = match start_str.trim().parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let end: u8 = match end_str.trim().parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                for id in start.min(end)..=start.max(end) {
                    set.insert(id);
                }
            } else if let Ok(id) = token.parse::<u8>() {
                set.insert(id);
            }
        }
        if set.is_empty() { None } else { Some(set) }
    }

    /// Convert a sorted list of u8 ids into a compact wire-friendly string
    /// e.g. `[1,2,3,5,7,8,9]` → `"1-3,5,7-9"`.
    pub fn canonicalize_slaves(ids: &[u8]) -> String {
        if ids.is_empty() {
            return String::new();
        }
        let mut parts = Vec::new();
        let mut start = ids[0];
        let mut end = ids[0];
        for &id in ids.iter().skip(1) {
            if id == end.wrapping_add(1) {
                end = id;
            } else {
                if start == end {
                    parts.push(format!("{start}"));
                } else {
                    parts.push(format!("{start}-{end}"));
                }
                start = id;
                end = id;
            }
        }
        if start == end {
            parts.push(format!("{start}"));
        } else {
            parts.push(format!("{start}-{end}"));
        }
        parts.join(",")
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
            egui::ComboBox::from_id_salt("port")
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
            egui::ComboBox::from_id_salt("baud")
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
            egui::ComboBox::from_id_salt("data_bits")
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
            egui::ComboBox::from_id_salt("parity")
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
            egui::ComboBox::from_id_salt("stop_bits")
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
            egui::ComboBox::from_id_salt("flow_control")
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

        // Show slaves: comma-separated ids or ranges (1-3); empty or * = show all.
        // Entries are never discarded at capture time; filtering only affects
        // the view and manual export. Auto-export stays unfiltered.
        ui.horizontal(|ui| {
            ui.label("Show slaves:");
            ui.add(
                egui::TextEdit::singleline(&mut self.show_slaves_str)
                    .desired_width(140.0)
                    .hint_text("* or 1-3,5,7-9"),
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
                export::export_entries(entries, self.show_set().as_ref());
            }
            if ui.button("💾 Save Config").clicked() {
                action = SettingsAction::SaveConfig;
            }
        });

        ui.separator();
        ui.label(format!("Entries: {}", entries.len()));
        if let Some(path) = crate::config::config_path() {
            ui.label(format!("Config: {}", path.display()));
        }

        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_set_none_when_blank() {
        let s = SettingsState {
            show_slaves_str: "".into(),
            ..Default::default()
        };
        assert!(s.show_set().is_none());
    }

    #[test]
    fn show_set_none_when_star() {
        let s = SettingsState {
            show_slaves_str: "*".into(),
            ..Default::default()
        };
        assert!(s.show_set().is_none());
    }

    #[test]
    fn show_set_parses_loose_input() {
        let s = SettingsState {
            show_slaves_str: "2, 3 ,abc,3,300,".into(),
            ..Default::default()
        };
        let set = s.show_set().unwrap();
        assert_eq!(set, [2, 3].into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn show_set_none_when_all_garbage() {
        let s = SettingsState {
            show_slaves_str: "abc,xyz,,".into(),
            ..Default::default()
        };
        // All tokens invalid → returns None (show all) as fallback.
        assert!(s.show_set().is_none());
    }

    #[test]
    fn show_set_parses_range() {
        let s = SettingsState {
            show_slaves_str: "1-3".into(),
            ..Default::default()
        };
        assert_eq!(
            s.show_set().unwrap(),
            [1, 2, 3].into_iter().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn show_set_parses_inverted_range() {
        let s = SettingsState {
            show_slaves_str: "5-2".into(),
            ..Default::default()
        };
        assert_eq!(
            s.show_set().unwrap(),
            [2, 3, 4, 5].into_iter().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn show_set_parses_combined() {
        let s = SettingsState {
            show_slaves_str: "1-3,5,7-9".into(),
            ..Default::default()
        };
        assert_eq!(
            s.show_set().unwrap(),
            [1, 2, 3, 5, 7, 8, 9].into_iter().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn show_set_parses_overlapping_ranges() {
        let s = SettingsState {
            show_slaves_str: "1-5,3-7".into(),
            ..Default::default()
        };
        assert_eq!(
            s.show_set().unwrap(),
            [1, 2, 3, 4, 5, 6, 7].into_iter().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn canonicalize_slaves_compact_ranges() {
        assert_eq!(
            SettingsState::canonicalize_slaves(&[1, 2, 3, 5, 7, 8, 9]),
            "1-3,5,7-9"
        );
    }

    #[test]
    fn canonicalize_slaves_single_values() {
        assert_eq!(
            SettingsState::canonicalize_slaves(&[2, 5, 9]),
            "2,5,9"
        );
    }

    #[test]
    fn canonicalize_slaves_empty() {
        assert_eq!(SettingsState::canonicalize_slaves(&[]), "");
    }

    #[test]
    fn canonicalize_slaves_single() {
        assert_eq!(SettingsState::canonicalize_slaves(&[42]), "42");
    }

    #[test]
    fn canonicalize_slaves_edge_overflow_safe() {
        assert_eq!(
            SettingsState::canonicalize_slaves(&[253, 254, 255]),
            "253-255"
        );
    }

    #[test]
    fn persisted_roundtrip_preserves_show_slaves() {
        let s = SettingsState {
            show_slaves_str: "3,2".into(),
            baud: "115200".into(),
            timeout_ms: 400,
            ..Default::default()
        };
        let cfg = s.to_persisted();
        assert_eq!(cfg.show_slaves, vec![2, 3]);
        assert_eq!(cfg.baud, "115200");

        let mut s2 = SettingsState::default();
        s2.apply_persisted(&cfg);
        assert_eq!(
            s2.show_set().unwrap(),
            [2, 3].into_iter().collect::<HashSet<_>>()
        );
        assert_eq!(s2.baud, "115200");
        assert_eq!(s2.timeout_ms, 400);
    }

    #[test]
    fn persisted_roundtrip_preserves_range_compact() {
        let s = SettingsState {
            show_slaves_str: "1-5".into(),
            ..Default::default()
        };
        let cfg = s.to_persisted();
        assert_eq!(cfg.show_slaves, vec![1, 2, 3, 4, 5]);

        let mut s2 = SettingsState::default();
        s2.apply_persisted(&cfg);
        assert_eq!(s2.show_slaves_str, "1-5");
    }
}
