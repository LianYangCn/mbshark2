//! Rendering helpers: shared formatting (always available) plus egui views
//! behind the `gui` feature.

pub mod format;

#[cfg(feature = "gui")]
pub mod settings;
#[cfg(feature = "gui")]
pub mod ui_view;
