//! The three racks under the transport: beatgrid, effects, stems.
//!
//! Tempo correction and stem separation are optional panels. The unfinished
//! effects preview is kept out of the normal console.

use egui::{vec2, Align2, Rect, Sense, Stroke, Ui};

use super::theme;
use crate::Defalt;

/* ── 5. Beatgrid ─────────────────────────────────────────────────────── */
pub fn beatgrid(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();
    let half = (full.width() - 8.0) / 2.0;
    for deck in 0..2 {
        let rect = Rect::from_min_size(full.min + vec2(deck as f32 * (half + 8.0), 0.0), vec2(half, full.height()));
        super::plate(ui, rect);
        let mut row = super::child(ui, rect.shrink2(vec2(10.0, 4.0)), super::left_row(), if deck == 0 { "gridA" } else { "gridB" });
        row.spacing_mut().item_spacing.x = 10.0;
        row.label(super::rich(if deck == 0 { "Deck A tempo" } else { "Deck B tempo" }, 12.0, theme::TEXT_DIM));
        let bpm = app.decks[deck].record.as_ref().and_then(|r| r.bpm);
        row.label(super::rich(&bpm.map_or("-- BPM".into(), |n| format!("{n:.1} BPM")), 13.0, theme::TEXT));
        if super::chip(&mut row, "Half", vec2(48.0, 26.0), false, bpm.is_some()).clicked() { app.scale_tempo(deck, 0.5); }
        if super::chip(&mut row, "Double", vec2(58.0, 26.0), false, bpm.is_some()).clicked() { app.scale_tempo(deck, 2.0); }
        if super::chip(&mut row, "Reset", vec2(52.0, 26.0), false, bpm.is_some()).clicked() { app.reset_grid(deck); }
    }
}

/* ── 6. Effects ──────────────────────────────────────────────────────── */
pub fn effects(app: &mut Defalt, ui: &mut Ui) {
    if !app.show_fx {
        return;
    }
    let full = ui.max_rect();
    let (left, centre, right) = super::split_thirds(full, 104.0, 6.0);

    fx_slots(ui, left, &["Echo", "Flanger", "Gate"], &["1", "", "1/8"]);
    fx_centre(app, ui, centre);
    fx_slots(ui, right, &["Echo", "Flanger", "Gate"], &["1", "", "1/2"]);
}

fn fx_slots(ui: &mut Ui, rect: Rect, names: &[&str], amounts: &[&str]) {
    let width = (rect.width() - 8.0) / names.len() as f32;
    for (i, name) in names.iter().enumerate() {
        let cell = Rect::from_min_size(
            egui::pos2(rect.left() + i as f32 * (width + 4.0), rect.top()),
            vec2(width, rect.height()),
        );
        super::plate(ui, cell);
        let inner = cell.shrink(4.0);

        let mut head = super::child(ui, Rect::from_min_size(inner.min, vec2(inner.width(), 20.0)),
                                    super::left_row(), "rac3");
        super::dropdown(&mut head, name, inner.width(), false);

        let body = Rect::from_min_max(egui::pos2(inner.left(), inner.top() + 23.0), inner.max);
        let mut row = super::child(ui, body, super::left_row(), "rac4");
        row.spacing_mut().item_spacing.x = 3.0;

        super::chip(&mut row, "ON", vec2(28.0, 18.0), false, false);
        if amounts[i].is_empty() {
            // The flanger takes a sweep rather than a beat division.
            let (slot, response) = row.allocate_exact_size(vec2(58.0, 18.0), Sense::hover());
            ui.painter().rect_filled(
                Rect::from_center_size(slot.center(), vec2(slot.width(), 4.0)),
                2.0,
                theme::SLOT,
            );
            ui.painter().rect_filled(
                Rect::from_center_size(slot.center(), vec2(11.0, 14.0)),
                2.0,
                theme::EDGE_LIT,
            );
            response.on_hover_text(super::NOT_WIRED);
        } else {
            super::chip(&mut row, "‹", vec2(16.0, 18.0), false, false);
            super::chip(&mut row, amounts[i], vec2(26.0, 18.0), false, false);
            super::chip(&mut row, "›", vec2(16.0, 18.0), false, false);
        }
        row.add_space(3.0);
        for letter in ["D", "W"] {
            super::chip(&mut row, letter, vec2(16.0, 18.0), false, false);
        }
    }
}

fn fx_centre(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    let mut column = super::child(ui, rect, egui::Layout::top_down(egui::Align::Center), "rac5");
    column.spacing_mut().item_spacing.y = 3.0;
    column.label(super::rich("FX", 11.0, theme::TEXT_DIM));
    column.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        super::chip(ui, "MANUAL", vec2(48.0, 18.0), app.fx_manual, false);
        super::chip(ui, "INSTANT", vec2(48.0, 18.0), !app.fx_manual, false);
    });
}

/* ── 7. Stems ────────────────────────────────────────────────────────── */
pub fn stems(app: &mut Defalt, ui: &mut Ui) {
    if !app.show_stems {
        return;
    }
    let full = ui.max_rect();
    let (left, centre, right) = super::split_thirds(full, 132.0, 6.0);

    stem_row(app, ui, 0, left);
    stem_centre(app, ui, centre);
    stem_row(app, ui, 1, right);
}

fn stem_row(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let live = app.separated[deck];
    let width = (rect.width() - 9.0) / crate::engine::deck::STEMS as f32;

    for (index, name) in crate::engine::deck::STEM_NAMES.iter().enumerate() {
        let cell = Rect::from_min_size(
            egui::pos2(rect.left() + index as f32 * (width + 3.0), rect.top()),
            vec2(width, rect.height()),
        );
        super::plate(ui, cell);
        let inner = cell.shrink(4.0);
        let muted = app.stem_muted[deck][index];

        let ink = if !live {
            theme::TEXT_MUTE
        } else if muted {
            theme::PLAYHEAD
        } else {
            theme::TEXT_DIM
        };
        super::label(ui, inner.left_top() + vec2(18.0, 1.0), Align2::LEFT_TOP, name, 9.5, ink);
        stem_glyph(ui, inner.left_top() + vec2(7.0, 6.0), index, ink);

        // Mute.
        let mute = Rect::from_min_size(inner.right_top() - vec2(18.0, -1.0), vec2(18.0, 15.0));
        let hit = ui.interact(mute, egui::Id::new(("stemmute", deck, index)), Sense::click());
        if live && hit.hovered() {
            ui.painter().rect_filled(mute, 3.0, theme::RAISED);
        }
        let cross = if muted { theme::PLAYHEAD } else { ink };
        for (a, b) in [((-3.0, -3.0), (3.0, 3.0)), ((3.0, -3.0), (-3.0, 3.0))] {
            ui.painter().line_segment(
                [mute.center() + vec2(a.0, a.1), mute.center() + vec2(b.0, b.1)],
                Stroke::new(1.3, cross),
            );
        }
        if hit.clicked() && live {
            app.toggle_stem_mute(deck, index);
        }
        if !live {
            hit.on_hover_text("Separate the record first.");
        }

        // Level.
        let slot = Rect::from_min_size(
            egui::pos2(inner.left(), inner.bottom() - 14.0),
            vec2(inner.width(), 12.0),
        );
        let mut level = app.stem_gain[deck][index];
        let response = ui.interact(slot, egui::Id::new(("stem", deck, index)), Sense::click_and_drag());
        if live && response.dragged() {
            let travel = slot.width().max(1.0);
            level = (level + response.drag_delta().x / travel).clamp(0.0, 1.0);
            app.set_stem_gain(deck, index, level);
        }
        if live && response.double_clicked() {
            app.set_stem_gain(deck, index, 1.0);
        }

        let track = Rect::from_center_size(slot.center(), vec2(slot.width(), 5.0));
        ui.painter().rect_filled(track, 2.5, theme::SLOT);
        if live && !muted {
            ui.painter().rect_filled(
                Rect::from_min_size(track.min, vec2(track.width() * level, track.height())),
                2.5,
                theme::BLUE_DEEP,
            );
        }
        let cap_x = track.left() + track.width() * level;
        ui.painter().rect_filled(
            Rect::from_center_size(egui::pos2(cap_x, track.center().y), vec2(9.0, 13.0)),
            2.0,
            if live { theme::CAP } else { theme::RAISED_HI },
        );
    }
}

fn stem_glyph(ui: &Ui, at: egui::Pos2, which: usize, c: egui::Color32) {
    match which {
        0 => {
            ui.painter().circle_stroke(at, 4.0, Stroke::new(1.2, c));
            ui.painter().line_segment([at + vec2(-5.0, -4.0), at + vec2(5.0, 4.0)], Stroke::new(1.0, c));
        }
        1 => {
            ui.painter().line_segment([at + vec2(0.0, -5.0), at + vec2(0.0, 3.0)], Stroke::new(1.2, c));
            ui.painter().circle_filled(at + vec2(-1.5, 3.5), 2.6, c);
        }
        2 => {
            for dx in [-3.5f32, 0.0, 3.5] {
                ui.painter().rect_filled(
                    Rect::from_center_size(at + vec2(dx, 0.0), vec2(2.0, 9.0)),
                    0.5,
                    c,
                );
            }
        }
        _ => {
            ui.painter().circle_stroke(at + vec2(0.0, -2.0), 2.6, Stroke::new(1.2, c));
            ui.painter().line_segment([at + vec2(0.0, 1.0), at + vec2(0.0, 5.0)], Stroke::new(1.2, c));
        }
    }
}

fn stem_centre(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    let mut column = super::child(ui, rect.shrink2(vec2(4.0, 0.0)), egui::Layout::top_down(egui::Align::Center), "stemc");
    column.spacing_mut().item_spacing.y = 4.0;
    for deck in 0..2 {
        let name = if deck == 0 { "A" } else { "B" };
        let busy = app.splitting(deck).is_some();
        let text = if busy { format!("Splitting {name}...") }
            else if app.separated[deck] { format!("{name} separated") }
            else { format!("Separate {name}") };
        let live = !busy && !app.separated[deck] && !app.decks[deck].loading
            && app.decks[deck].record.is_some() && app.can_pull();
        let note = app.splitting(deck).map(|job| job.stage.label()).unwrap_or_else(|| "Separate drums, bass, harmony and vocals. Drag their levels or click a cross to mute.".into());
        if super::chip(&mut column, &text, vec2(120.0, 25.0), app.separated[deck], live).on_hover_text(note).clicked() {
            app.begin_split(deck);
        }
    }
}
