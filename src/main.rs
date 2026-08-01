//! Binary entry point. Only built when the `gui` feature is enabled.

// Suppress the console window on Windows release builds;
// keep it in debug builds so println! / log output is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    mbshark2::app::run();
}
