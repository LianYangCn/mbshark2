//! Plain-text export of captured entries via a native save dialog.

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use chrono::Local;

use crate::render::format::{
    counter_slave_map, format_entry, is_hidden, lines_to_plain, should_separate,
};
use crate::session::model::Entry;

/// Render all `entries` as plain text (no color), one block per entry. Entries
/// whose slave is in `hidden` are skipped (but never removed from `entries`).
pub fn entries_to_text(entries: &VecDeque<Entry>, hidden: &HashSet<u8>) -> String {
    let map = counter_slave_map(entries);
    let mut text = String::new();
    let mut prev_counter: Option<u64> = None;
    for entry in entries {
        if is_hidden(entry, &map, hidden) {
            continue;
        }
        if should_separate(prev_counter, entry.counter, entry.tag) {
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
/// Applies the user's hidden-slave filter. No-op if the user cancels.
pub fn export_entries(entries: &VecDeque<Entry>, hidden: &HashSet<u8>) {
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

    write_entries(entries, &path, hidden);
}

/// Write all `entries` as plain text directly to `path` (no dialog).
///
/// Used by the `MBSHARK_AUTOEXPORT_PATH` scriptability hook. Callers that
/// represent the unfiltered capture (e.g. the autoexport hook) pass an empty
/// `hidden` set so the scripted output stays complete and predictable.
pub fn write_entries(entries: &VecDeque<Entry>, path: &Path, hidden: &HashSet<u8>) {
    let text = entries_to_text(entries, hidden);
    if let Err(e) = std::fs::write(path, text) {
        eprintln!("mbshark2: export to {}: {e}", path.display());
    }
}
