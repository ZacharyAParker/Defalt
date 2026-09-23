//! The three racks under the transport: beatgrid, effects, stems.
//!
//! Tempo correction, a beat echo and stem separation, each an optional
//! panel toggled from the toolbar.

use egui::{vec2, Align2, Rect, Sense, Stroke, Ui};

use super::{theme, widgets, Look};
use crate::Defalt;

/* ── 5. Beatgrid ─────────────────────────────────────────────────────── */
pub fn beatgrid(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();
    let half = (full.width() - 8.0) / 2.0;
    for deck in 0..2 {
        let rect = Rect::from_min_size(full.min + vec2(deck as f32 * (half + 8.0), 0.0), vec2(half, full.height()));
        super::plate(ui, rect);
        let mut row = super::child(ui, rect.shrink2(vec2(theme::SP_3, 4.0)), super::left_row(), if deck == 0 { "gridA" } else { "gridB" });
        row.spacing_mut().item_spacing.x = theme::SP_2;
        let (badge, _) = row.allocate_exact_size(vec2(22.0, 20.0), Sense::hover());
        super::deck_badge(&row, badge, deck);
        row.label(super::rich(if deck == 0 { "Deck A tempo" } else { "Deck B tempo" }, theme::SIZE_S, theme::TEXT_DIM));
        let bpm = app.decks[deck].record.as_ref().and_then(|r| r.bpm);
        row.label(egui::RichText::new(bpm.map_or("-- BPM".into(), |n| format!("{n:.1} BPM")))
            .font(egui::FontId::monospace(theme::SIZE_M)).color(theme::TEXT));
        let key = |text: &str| vec2(if text == "Double" { 64.0 } else { 54.0 }, theme::CONTROL_S);
        if super::chip(&mut row, "Half", key("Half"), false, bpm.is_some()).clicked() { app.scale_tempo(deck, 0.5); }
        if super::chip(&mut row, "Double", key("Double"), false, bpm.is_some()).clicked() { app.scale_tempo(deck, 2.0); }
        if super::chip(&mut row, "Reset", key("Reset"), false, bpm.is_some()).clicked() { app.reset_grid(deck); }
    }
}

/* ── 6. Effects ──────────────────────────────────────────────────────── */

/// Beat divisions the echo can repeat at.
const DIVISIONS: [(f64, &str); 6] = [
    (0.25, "1/4"), (0.5, "1/2"), (0.75, "3/4"), (1.0, "1"), (2.0, "2"), (4.0, "4"),
];

/// One deck's beat echo: the same echo the station uses for its transition
/// tails, played by hand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Echo {
    pub on: bool,
    /// Index into `DIVISIONS`.
    pub division: usize,
    /// 0..1 of the echo's wet range.
    pub mix: f32,
    /// 0..1 of the echo's feedback range.
    pub feedback: f32,
    /// What the engine was last told, so a tempo change re-times the repeats
    /// without a command every frame.
    sent: Option<[f32; 3]>,
    /// Post-fader send into the shared reverb, 0..1.
    pub reverb: f32,
    reverb_sent: Option<f32>,
}

impl Default for Echo {
    fn default() -> Self {
        Echo { on: false, division: 1, mix: 0.6, feedback: 0.5, sent: None, reverb: 0.0, reverb_sent: None }
    }
}

impl Echo {
    /// What the engine was last told, if the rack is driving this echo.
    pub fn last_sent(&self) -> Option<[f32; 3]> {
        self.sent
    }

    #[cfg(test)]
    pub fn sent_for_test(command: [f32; 3]) -> Self {
        Echo { on: true, sent: Some(command), ..Echo::default() }
    }

    /// The engine's echo was reset under the rack; send it again when used.
    pub fn forget_sent(&mut self) {
        self.sent = None;
    }

    /// Mix, feedback and delay seconds for the engine, at this beat length.
    pub fn command(&self, beat: f64) -> [f32; 3] {
        let beats = DIVISIONS[self.division.min(DIVISIONS.len() - 1)].0;
        let seconds = (beats * beat).clamp(0.02, 1.95) as f32;
        [if self.on { self.mix * 0.5 } else { 0.0 }, self.feedback * 0.65, seconds]
    }
}

/// Seconds per beat on this deck, at its current pitch; half a second when
/// there is no grid to go on.
fn beat_seconds(app: &Defalt, deck: usize) -> f64 {
    let period = app.decks[deck].record.as_ref().and_then(|r| r.beat_period)
        .filter(|p| *p > 0.05).unwrap_or(0.5);
    period / (1.0 + (app.decks[deck].pitch + app.decks[deck].bend) as f64 / 100.0)
}

pub fn effects(app: &mut Defalt, ui: &mut Ui) {
    if !app.show_fx {
        return;
    }
    let full = ui.max_rect();
    let (left, centre, right) = super::split_thirds(full, 104.0, 6.0);

    echo_rack(app, ui, 0, left);
    fx_centre(ui, centre);
    echo_rack(app, ui, 1, right);
}

fn echo_rack(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    super::plate(ui, rect);
    // The station drives the echo on a deck it is carrying; two hands on one
    // effect is a fight nobody hears the end of.
    let station = app.airtime.on_deck(deck).is_some();
    let live = app.engine_ready() && !station;
    let mut echo = app.view_state.fx[deck];

    let inner = rect.shrink2(vec2(theme::SP_2, 5.0));
    let mut row = super::child(ui, inner, super::left_row(), &format!("fx{deck}"));
    row.spacing_mut().item_spacing.x = theme::SP_1;
    let (badge, _) = row.allocate_exact_size(vec2(22.0, 20.0), Sense::hover());
    super::deck_badge(&row, badge, deck);
    row.label(super::rich("ECHO", theme::SIZE_XS, theme::TEXT_DIM));
    let on = super::button(&mut row, "ON", vec2(34.0, theme::CONTROL_S),
                           Look::secondary(echo.on, live).accent(theme::DECK_COLOURS[deck]));
    let on = if station { on.on_hover_text("The station is using this deck's echo for its mix.") } else { on };
    if on.clicked() {
        echo.on = !echo.on;
    }
    let step = vec2(theme::CONTROL_S, theme::CONTROL_S);
    if super::glyph_chip(&mut row, super::Glyph::Minus, step, live && echo.division > 0)
        .on_hover_text("Shorter repeats").clicked() {
        echo.division -= 1;
    }
    let (_, name) = DIVISIONS[echo.division.min(DIVISIONS.len() - 1)];
    row.add_sized(vec2(30.0, theme::CONTROL_S), egui::Label::new(
        egui::RichText::new(name).font(egui::FontId::monospace(theme::SIZE_S)).color(theme::TEXT)))
        .on_hover_text("Beats between repeats");
    if super::glyph_chip(&mut row, super::Glyph::Plus, step, live && echo.division + 1 < DIVISIONS.len())
        .on_hover_text("Longer repeats").clicked() {
        echo.division += 1;
    }

    // The echo's two levels and the reverb send share whatever width is left.
    let rest = Rect::from_min_max(egui::pos2(row.min_rect().right() + theme::SP_3, inner.top()), inner.max);
    let third = ((rest.width() - 16.0) / 3.0).max(20.0);
    let deck_name = if deck == 0 { "A" } else { "B" };
    for (at, (name, value, reset, lit, what)) in [
        ("MIX", &mut echo.mix, 0.6, echo.on, "echo mix"),
        ("FB", &mut echo.feedback, 0.5, echo.on, "echo feedback"),
        ("VERB", &mut echo.reverb, 0.0, true, "reverb send"),
    ].into_iter().enumerate()
    {
        let cell = Rect::from_min_size(egui::pos2(rest.left() + at as f32 * (third + 8.0), rest.top()),
                                       vec2(third, rest.height()));
        super::label(ui, cell.left_top(), Align2::LEFT_TOP, name, theme::SIZE_XS, theme::TEXT_MUTE);
        let slot = Rect::from_min_size(egui::pos2(cell.left(), cell.bottom() - 16.0), vec2(cell.width(), 14.0));
        let label = format!("Deck {deck_name} {what}");
        widgets::slim(ui, slot, egui::Id::new(("fx", deck, name)), value, reset, live, lit, &label);
    }

    if live {
        let command = echo.command(beat_seconds(app, deck));
        let moved = echo.sent.is_none_or(|sent| {
            sent.iter().zip(command.iter()).any(|(a, b)| (a - b).abs() > 1e-3)
        });
        // Nothing is sent until the echo has been used, so opening the rack
        // does not touch a deck.
        if moved && (echo.on || echo.sent.is_some()) {
            app.set_echo(deck, command);
            echo.sent = Some(command);
        }
        if echo.reverb_sent.is_none_or(|sent| (sent - echo.reverb).abs() > 1e-3)
            && (echo.reverb > 0.0 || echo.reverb_sent.is_some()) {
            app.set_reverb_send(deck, echo.reverb);
            echo.reverb_sent = Some(echo.reverb);
        }
    } else {
        // Whoever has it now owns what it is doing.
        echo.sent = None;
        echo.reverb_sent = None;
    }
    app.view_state.fx[deck] = echo;
}

fn fx_centre(ui: &mut Ui, rect: Rect) {
    let mut column = super::child(ui, rect, egui::Layout::top_down(egui::Align::Center), "fxc");
    column.spacing_mut().item_spacing.y = 3.0;
    column.label(super::rich("FX", theme::SIZE_S, theme::TEXT_DIM));
    column.label(super::rich("Echo + verb", theme::SIZE_XS, theme::TEXT_MUTE))
        .on_hover_text("Repeats timed to each deck's own beat, and a send into the shared reverb. Double-click a level to reset it.");
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
            theme::RED
        } else {
            theme::TEXT_DIM
        };
        // Clipped short of the mute cross, so a long name never runs under it.
        super::clipped_label(ui, Rect::from_min_size(inner.left_top() + vec2(18.0, 0.0),
                                                     vec2((inner.width() - 40.0).max(10.0), 16.0)),
                             name, theme::SIZE_XS, ink);
        stem_glyph(ui, inner.left_top() + vec2(7.0, 6.0), index, ink);

        // Mute.
        let mute = Rect::from_min_size(inner.right_top() - vec2(18.0, -1.0), vec2(18.0, 15.0));
        let hit = ui.interact(mute, egui::Id::new(("stemmute", deck, index)),
                              if live { Sense::click() } else { Sense::hover() });
        hit.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, live, muted,
                                                      format!("Mute {name}")));
        if live && (hit.hovered() || hit.has_focus()) {
            ui.painter().rect_filled(mute, theme::R_S, theme::RAISED);
        }
        let cross = if muted { theme::RED } else { ink };
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
        let id = egui::Id::new(("stem", deck, index));
        if widgets::slim(ui, slot, id, &mut level, 1.0, live, !muted, &format!("{name} level")).changed() {
            app.set_stem_gain(deck, index, level);
        }
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
    let mut column = super::child(ui, rect.shrink2(vec2(4.0, 1.0)), egui::Layout::top_down(egui::Align::Center), "stemc");
    column.spacing_mut().item_spacing.y = theme::SP_1;
    for deck in 0..2 {
        let name = if deck == 0 { "A" } else { "B" };
        let busy = app.splitting(deck).is_some();
        let text = if busy { format!("Splitting {name}...") }
            else if app.separated[deck] { format!("{name} separated") }
            else { format!("Separate {name}") };
        let live = !busy && !app.separated[deck] && !app.decks[deck].loading
            && app.decks[deck].record.is_some() && app.can_pull();
        let note = app.splitting(deck).map(|job| job.stage.label()).unwrap_or_else(|| "Separate drums, bass, harmony and vocals. Drag their levels or click a cross to mute.".into());
        if super::button(&mut column, &text, vec2(120.0, theme::CONTROL_S),
                         Look::secondary(app.separated[deck], live).accent(theme::DECK_COLOURS[deck]))
            .on_hover_text(note).clicked() {
            app.begin_split(deck);
        }
    }
}
