//! Plain-text export of captured entries via a native save dialog.

use std::collections::VecDeque;
use std::path::Path;

use chrono::Local;

use crate::render::format::{format_entry, lines_to_plain};
use crate::session::model::Entry;

/// Render all `entries` as plain text (no color), one block per entry.
/// Separator rules (matching the UI's line separators):
/// - Insert `---` between different transactions (counter changes)
/// - Insert `---` before any Orphan / Parse entry (it cannot belong to a
///   normal request/response session, even if it shares a counter)
pub fn entries_to_text(entries: &VecDeque<Entry>) -> String {
    let mut text = String::new();
    let mut prev_counter: Option<u64> = None;
    for entry in entries {
        let counter_changed = prev_counter.is_some_and(|pc| pc != entry.counter);
        let is_standalone = matches!(entry.tag, crate::session::model::Tag::Orphan | crate::session::model::Tag::Parse);
        if counter_changed || is_standalone {
            text.push_str("---\n");
        }
        prev_counter = Some(entry.counter);
        let lines = format_entry(entry);
        text.push_str(&lines_to_plain(&lines));
        text.push('\n');
    }
    text
}

/// Open a save dialog and write all `entries` as plain text (no color).
/// No-op if the user cancels.
pub fn export_entries(entries: &VecDeque<Entry>) {
    let stamp = Local::now().format("%Y%m%d_%H%M%S");
    let default_name = format!("mbshark_{stamp}.txt");

    let path = match rfd::FileDialog::new()
        .set_file_name(&default_name)
        .add_filter("Text files", &["txt", "log"])
        .save_file()
    {
        Some(p) => p,
        None => return,
    };

    write_entries(entries, &path);
}

/// Write all `entries` as plain text directly to `path` (no dialog).
/// Used by the `MBSHARK_AUTOEXPORT_PATH` scriptability hook.
pub fn write_entries(entries: &VecDeque<Entry>, path: &Path) {
    let text = entries_to_text(entries);
    if let Err(e) = std::fs::write(path, text) {
        eprintln!("mbshark2: export to {}: {e}", path.display());
    }
}
