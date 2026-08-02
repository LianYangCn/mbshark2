//! mbshark2 — a cross-platform Modbus RTU serial capture tool.
//!
//! The crate is split into a pure logic layer (always available) and a GUI
//! layer (behind the `gui` feature, default on).
//!
//! - [`protocol`] — Modbus RTU framing, CRC, and PDU decoding for all FCs.
//! - [`capture::framer`] — RTU frame assembly (length+CRC split primary, 3.5-char gap fallback).
//! - [`session`] — request/response pairing + timeout/orphan state machine.
//! - [`render::format`] — shared text formatting (UI + export).
//!
//! The `gui` feature adds [`app`] (eframe app), [`capture::engine`] (async
//! capture orchestration), [`render::ui_view`] / [`render::settings`], and
//! [`export`].

pub mod protocol;
pub mod session;
pub mod render;
pub mod capture;

#[cfg(feature = "gui")]
pub mod app;
#[cfg(feature = "gui")]
pub mod config;
#[cfg(feature = "gui")]
pub mod export;
