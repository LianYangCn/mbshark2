//! Async serial port setup — a thin wrapper over `tokio-serial`.
//!
//! `tokio-serial` re-exports the full `serialport` config API
//! (`DataBits`/`Parity`/`StopBits`/`FlowControl`), so the whole GUI talks
//! only to `tokio_serial::` and never imports `serialport` directly.

use std::time::Duration;

use tokio_serial::{DataBits, FlowControl, Parity, SerialPortBuilderExt, SerialStream, StopBits};

/// Serial port + timing configuration chosen in the settings panel.
#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub port: String,
    pub baud: u32,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub flow_control: FlowControl,
    /// How long to wait for a response before declaring timeout (and moving
    /// the request to the timed-out pool so a late reply becomes an ORPHAN).
    pub response_timeout: Duration,
}

impl Default for SerialConfig {
    fn default() -> Self {
        SerialConfig {
            port: String::new(),
            baud: 9600,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
            response_timeout: Duration::from_millis(500),
        }
    }
}

/// Enumerate available serial port names (empty on error).
pub fn available_port_names() -> Vec<String> {
    match tokio_serial::available_ports() {
        Ok(ports) => ports.into_iter().map(|p| p.port_name).collect(),
        Err(_) => Vec::new(),
    }
}

/// Open a serial port as an async `SerialStream`.
pub fn open_stream(cfg: &SerialConfig) -> Result<SerialStream, tokio_serial::Error> {
    let builder = tokio_serial::new(&cfg.port, cfg.baud)
        .data_bits(cfg.data_bits)
        .parity(cfg.parity)
        .stop_bits(cfg.stop_bits)
        .flow_control(cfg.flow_control);
    builder.open_native_async()
}
