//! egui rendering of the capture list.
//!
//! Each entry is formatted into [`Line`]s (via the shared `format` layer) and
//! rendered as colored monospace spans. A per-entry lines cache is kept by the
//! app so `format_entry` isn't re-run every frame.

use std::collections::{HashSet, VecDeque};

use egui::{Color32, RichText};

use crate::render::format::{counter_slave_map, should_separate, should_show, Line, SpanRole};
use crate::session::model::Tag;

/// Error / parse-failure color shared between the capture view and header banner.
pub const ERROR_RED: Color32 = Color32::from_rgb(0xf8, 0x51, 0x49);

/// Render the capture view inside a vertical `ScrollArea`. Pinned to the
/// bottom while `auto_scroll` is on. Only entries matching the `show_set`
/// filter are rendered (`None` = show all).
pub fn show(
    ui: &mut egui::Ui,
    entries: &VecDeque<crate::session::model::Entry>,
    lines_cache: &VecDeque<Vec<Line>>,
    auto_scroll: bool,
    show_set: Option<&HashSet<u8>>,
) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(auto_scroll)
        .show(ui, |ui| {
            // Monospace + zero item spacing so spans line up into one line.
            let mut layout = ui.style_mut().clone();
            layout
                .text_styles
                .entry(egui::TextStyle::Monospace)
                .or_insert_with(|| egui::FontId::monospace(13.0))
                .size = 13.0;
            ui.set_style(layout);

            let map = counter_slave_map(entries);
            let mut prev_counter: Option<u64> = None;
            for (entry, lines) in entries.iter().zip(lines_cache.iter()) {
                if !should_show(entry, &map, show_set) {
                    continue; // filtered entries don't update prev_counter
                }
                if should_separate(prev_counter, entry.counter, entry.tag) {
                    ui.separator();
                }
                prev_counter = Some(entry.counter);

                let tag = entry.tag;
                for line in lines {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for span in &line.0 {
                            let _ = ui.selectable_label(
                                false,
                                RichText::new(&span.text)
                                    .monospace()
                                    .color(role_color(span.role, tag)),
                            );
                        }
                    });
                }
            }

            if auto_scroll {
                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
            }
        });
}

fn tag_color(tag: Tag) -> Color32 {
    match tag {
        Tag::Request => Color32::from_rgb(0x4a, 0x9f, 0xf0), // blue
        Tag::Response => Color32::from_rgb(0x3f, 0xb9, 0x50), // green
        Tag::Orphan => Color32::from_rgb(0xd2, 0x99, 0x22), // orange
        Tag::Parse => ERROR_RED,
    }
}

fn role_color(role: SpanRole, tag: Tag) -> Color32 {
    match role {
        SpanRole::Tag => tag_color(tag),
        SpanRole::Error => ERROR_RED,
        SpanRole::Hex => Color32::from_rgb(0xb1, 0xba, 0xca), // light gray
        SpanRole::Timestamp => Color32::from_rgb(0x8b, 0x94, 0x9e), // dim gray
        SpanRole::Address => Color32::from_rgb(0xd2, 0xa8, 0xff), // purple
        SpanRole::Value => Color32::from_rgb(0x7e, 0xe7, 0x87), // bright green
        SpanRole::Label => Color32::from_rgb(0xc9, 0xd1, 0xd9), // default text
        SpanRole::Plain => Color32::from_rgb(0xc9, 0xd1, 0xd9),
    }
}
