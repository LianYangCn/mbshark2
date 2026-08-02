//! Persistent user configuration (TOML).
//!
//! The settings panel state that a user would reasonably want across restarts
//! (port / baud / framing / timeout / auto-scroll / hidden slaves) is stored
//! at a platform-appropriate config path and reloaded on launch. `load()`
//! always falls back to defaults on any error, so a corrupt file never blocks
//! startup.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Persisted subset of [`crate::render::settings::SettingsState`].
///
/// `#[serde(default)]` lets older config files missing newly-added fields load
/// cleanly from [`PersistedConfig::default`], whose values mirror the
/// [`crate::render::settings::SettingsState`] defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedConfig {
    pub port: String,
    pub baud: String,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub flow_control: String,
    pub timeout_ms: u64,
    pub auto_scroll: bool,
    pub hidden_slaves: Vec<u8>,
}

impl Default for PersistedConfig {
    fn default() -> Self {
        PersistedConfig {
            port: String::new(),
            baud: "9600".into(),
            data_bits: 8,
            parity: "None".into(),
            stop_bits: 1,
            flow_control: "None".into(),
            timeout_ms: 500,
            auto_scroll: true,
            hidden_slaves: Vec::new(),
        }
    }
}

/// Resolve the config file path from the environment (no `dirs` dependency).
///
/// - Linux: `$XDG_CONFIG_HOME/mbshark2/config.toml` or `$HOME/.config/mbshark2/config.toml`
/// - macOS: `$HOME/Library/Application Support/mbshark2/config.toml` (XDG still honoured)
/// - Windows: `%APPDATA%/mbshark2/config.toml`
///
/// Returns `None` only if no home/config env var is set at all.
pub fn config_path() -> Option<PathBuf> {
    let base = config_dir()?;
    Some(base.join("mbshark2").join("config.toml"))
}

fn config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg));
        }
    }
    #[cfg(windows)]
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            return Some(PathBuf::from(appdata));
        }
    }
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    #[cfg(target_os = "macos")]
    {
        return Some(PathBuf::from(home).join("Library").join("Application Support"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(PathBuf::from(home).join(".config"))
    }
}

/// Load the config file. Returns `None` (→ caller falls back to defaults) on
/// any missing file / parse error; the error is logged to stderr.
pub fn load() -> Option<PersistedConfig> {
    load_at(&config_path()?)
}

fn load_at(path: &Path) -> Option<PersistedConfig> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            // Missing file is the normal first-run case — stay quiet for it.
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("mbshark2: config load {}: {e}", path.display());
            }
            return None;
        }
    };
    match toml::from_str::<PersistedConfig>(&text) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            eprintln!("mbshark2: config parse {}: {e}", path.display());
            None
        }
    }
}

/// Serialize and atomically write the config. Writes to a sibling temp file
/// then renames over the target, so a crash mid-write cannot corrupt the
/// existing config. Errors are logged to stderr (no UI at Drop time).
pub fn save(cfg: &PersistedConfig) {
    let Some(path) = config_path() else {
        eprintln!("mbshark2: config save skipped (no config dir resolved)");
        return;
    };
    save_at(&path, cfg);
}

fn save_at(path: &Path, cfg: &PersistedConfig) {
    let text = match toml::to_string_pretty(cfg) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("mbshark2: config serialize: {e}");
            return;
        }
    };
    if let Err(e) = write_atomic(path, text.as_bytes()) {
        eprintln!("mbshark2: config save {}: {e}", path.display());
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.to_path_buf();
    tmp.set_extension("toml.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrips_through_toml() {
        let cfg = PersistedConfig {
            port: "/dev/ttyUSB0".into(),
            baud: "115200".into(),
            data_bits: 8,
            parity: "None".into(),
            stop_bits: 1,
            flow_control: "None".into(),
            timeout_ms: 400,
            auto_scroll: false,
            hidden_slaves: vec![2, 3],
        };
        let text = toml::to_string(&cfg).unwrap();
        let back: PersistedConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.port, "/dev/ttyUSB0");
        assert_eq!(back.baud, "115200");
        assert_eq!(back.timeout_ms, 400);
        assert_eq!(back.hidden_slaves, vec![2, 3]);
    }

    #[test]
    fn missing_field_uses_default() {
        // An older config file lacking `hidden_slaves` and `auto_scroll` must
        // still load via `#[serde(default)]`.
        let text = r#"
port = "/dev/ttyS0"
baud = "9600"
data_bits = 8
parity = "None"
stop_bits = 1
flow_control = "None"
timeout_ms = 500
"#;
        let cfg: PersistedConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.port, "/dev/ttyS0");
        assert!(cfg.auto_scroll, "default auto_scroll is true");
        assert!(cfg.hidden_slaves.is_empty(), "default hidden_slaves is empty");
    }

    #[test]
    fn load_save_roundtrip_via_file() {
        let dir = std::env::temp_dir().join(format!(
            "mbshark2_cfg_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("config.toml");

        // Missing file → None (first-run case).
        assert!(load_at(&path).is_none());

        let cfg = PersistedConfig {
            port: "/tmp/mb_a".into(),
            baud: "115200".into(),
            data_bits: 8,
            parity: "None".into(),
            stop_bits: 1,
            flow_control: "None".into(),
            timeout_ms: 400,
            auto_scroll: false,
            hidden_slaves: vec![2, 3],
        };
        save_at(&path, &cfg);
        let back = load_at(&path).expect("saved config loads back");
        assert_eq!(back.port, "/tmp/mb_a");
        assert_eq!(back.baud, "115200");
        assert_eq!(back.timeout_ms, 400);
        assert_eq!(back.hidden_slaves, vec![2, 3]);
        // The temp file is gone after rename — only config.toml exists.
        assert!(!path.with_extension("toml.tmp").exists());

        // Corrupt file → None (graceful fallback, no panic).
        std::fs::write(&path, "port = ").unwrap();
        assert!(load_at(&path).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
