//! The overview strip, the deck row and the transport.
//!
//! Both decks are mirrored about the centre, which is the whole reason a DJ
//! panel is laid out this way: your left hand and your right hand reach the
//! same control at the same distance from the middle. The transport is the
//! one exception: its keys read left to right on both sides, C1 to C4, so a
//! pad is found by its name rather than by working out which way round it is.

use egui::{vec2, Align2, Color32, FontId, Rect, Sense, Stroke, Ui};

use super::{theme, waveform, widgets, Look};
use crate::Defalt;

/// The platter at the deck row's usual height, and the most it grows to when
/// the library is made smaller.
const JOG: f32 = 204.0;
const JOG_MOST: f32 = 240.0;
/// The tallest the pitch column, platter and EQ are laid out.
const DECK_CONTROLS: f32 = 320.0;

/* ── 2. Overview strip ───────────────────────────────────────────────── */
pub fn overview_strip(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();
    let half = (full.width() - 6.0) / 2.0;

    for deck in 0..2 {
        let rect = Rect::from_min_size(
            egui::pos2(full.left() + deck as f32 * (half + 6.0), full.top()),
            vec2(half, full.height()),
        );
        super::well(ui, rect);
        overview_pane(app, ui, deck, rect);
        // The deck's colour along the top edge: the strip is the deck's name
        // plate, and this is what says whose it is at a glance.
        let edge = Rect::from_min_size(rect.min + vec2(theme::R_M, 0.0), vec2(rect.width() - theme::R_M * 2.0, 2.0));
        ui.painter().rect_filled(edge, 1.0, theme::DECK_COLOURS[deck]);
    }
}

fn overview_pane(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let loader_side = if deck == 0 { rect.left() + 8.0 } else { rect.right() - 80.0 };
    let loader = Rect::from_min_size(egui::pos2(loader_side, rect.top() + 8.0), vec2(72.0, theme::CONTROL_M));
    let colour = theme::DECK_COLOURS[deck];

    // The waveform gets everything except the load button's corner.
    let wave = if deck == 0 {
        Rect::from_min_max(
            egui::pos2(loader.right() + 6.0, rect.top() + 3.0),
            rect.max - vec2(4.0, 3.0),
        )
    } else {
        Rect::from_min_max(
            rect.min + vec2(4.0, 3.0),
            egui::pos2(loader.left() - 6.0, rect.bottom() - 3.0),
        )
    };

    let state = &app.decks[deck];

    if state.loading {
        super::label(
            ui,
            wave.left_top() + vec2(6.0, 6.0),
            Align2::LEFT_TOP,
            "Loading...",
            theme::SIZE_S,
            theme::TEXT_DIM,
        );
    } else if let Some(error) = &state.error {
        super::label(ui, wave.left_top() + vec2(6.0, 6.0), Align2::LEFT_TOP,
                     "Would not load", theme::SIZE_S, theme::RED);
        super::label(ui, wave.left_top() + vec2(6.0, 24.0), Align2::LEFT_TOP,
                     &super::elide(error, 70), theme::SIZE_XS, theme::TEXT_DIM);
    } else if state.record.is_some() {
        // The waveform fills the well and the text sits on it, as on the
        // reference. A strip this short cannot afford a separate text row.
        waveform::overview(
            ui.painter(),
            wave,
            state.peaks.as_ref(),
            state.position,
            state.length,
            &state.cues,
            app.view_state.wave_mode,
        );

        // A scrim under the text so it stays legible over a loud record,
        // faintly in the deck's colour.
        let scrim = Rect::from_min_size(wave.min, vec2(wave.width(), 44.0));
        let tinted = theme::tint(theme::WELL, colour, 0.10);
        ui.painter().rect_filled(
            scrim,
            egui::CornerRadius { nw: theme::R_S as u8, ne: theme::R_S as u8, sw: 0, se: 0 },
            Color32::from_rgba_unmultiplied(tinted.r(), tinted.g(), tinted.b(), 190),
        );

        let title = state.record.as_ref().map_or(String::new(), |r| r.title.clone());
        let artist = state.record.as_ref().map_or(String::new(), |r| r.artist.clone());
        let badge = Rect::from_min_size(wave.left_top() + vec2(6.0, 6.0), vec2(20.0, 20.0));
        super::deck_badge(ui, badge, deck);
        let text_at = wave.left_top() + vec2(32.0, 3.0);
        let title_width = (wave.width() - 32.0 - 104.0).max(40.0);
        super::clipped_text(ui, Rect::from_min_size(text_at, vec2(title_width, 24.0)), &title,
                            theme::display(theme::SIZE_L), theme::TEXT_BRIGHT);
        super::clipped_label(ui, Rect::from_min_size(text_at + vec2(0.0, 24.0), vec2(title_width, 17.0)),
                             &artist, theme::SIZE_S, theme::TEXT_DIM);

        // A record the station put here says so, because a deck that starts
        // playing on its own is alarming if nothing on screen claims it.
        if app.airtime.on_deck(deck).is_some() {
            let held = app.airtime.held.tone[deck].iter().any(|h| *h)
                || app.airtime.held.gain[deck];
            let (text, colour) = if held {
                ("AUTO / YOU", theme::WARN)
            } else {
                ("AUTO", theme::BLUE)
            };
            super::mono(ui, wave.left_top() + vec2(6.0, 46.0), Align2::LEFT_TOP, text, theme::SIZE_XS, colour);
        }

        // Time left is the number you steer by, so it is the big one; time
        // gone and the key sit under it.
        let remaining = (state.length - state.position).max(0.0);
        ui.painter().text(egui::pos2(wave.right() - 6.0, wave.top() + 2.0), Align2::RIGHT_TOP,
                          format!("-{}", super::mmss(remaining)), theme::readout(theme::SIZE_XL), theme::TEXT_BRIGHT);
        let mut under = super::mmss(state.position);
        if let Some(camelot) = state.record.as_ref().and_then(|r| r.camelot.clone()) {
            under = format!("{under}  {camelot}");
        }
        super::mono(ui, egui::pos2(wave.right() - 6.0, wave.top() + 28.0), Align2::RIGHT_TOP,
                    &under, theme::SIZE_XS, theme::TEXT_DIM);

        let windows = &app.view_state.windows[deck];
        let marker_rect = Rect::from_min_max(
            egui::pos2(wave.left(), (wave.top() + 55.0).min(wave.bottom() - 18.0)), wave.max);
        waveform::transition_marks(ui.painter(), marker_rect, windows, 0.0, state.length);
        let mut response = ui.interact(wave, ui.id().with(("ov", deck)), Sense::click_and_drag());
        if !windows.is_empty() {
            let description = windows.iter().map(|w| format!("Planned mix {}: {} to {}",
                if w.incoming { "in" } else { "out" }, super::mmss(w.start), super::mmss(w.end)))
                .collect::<Vec<_>>().join("\n");
            response = response.on_hover_text(description);
        }
        if (response.clicked() || response.dragged()) && app.decks[deck].length > 0.0 {
            if let Some(at) = ui.ctx().pointer_interact_pos() {
                let fraction = ((at.x - wave.left()) / wave.width()).clamp(0.0, 1.0);
                let seconds = fraction as f64 * app.decks[deck].length;
                app.seek(deck, seconds);
            }
        }
    } else {
        ui.painter().text(wave.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER,
            format!("Deck {} is empty", theme::DECK_LETTERS[deck]), theme::display(theme::SIZE_L), theme::TEXT);
        super::label(ui, wave.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER,
            "Choose a track below, then load it here", theme::SIZE_S, theme::TEXT_DIM);
    }

    load_button(app, ui, deck, loader);
}

/// The load key. Loads whatever the crate has selected; its letter is in the
/// deck's colour, so which side it loads is on the key itself.
fn load_button(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let has_pick = app.selected.is_some() && app.engine_ready();
    let name = format!("Load {}", theme::DECK_LETTERS[deck]);
    let response = ui.interact(rect, ui.id().with(("load", deck)), if has_pick { Sense::click() } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, has_pick, &name));

    let look = Look::secondary(false, has_pick).accent(theme::DECK_COLOURS[deck]);
    let hovered = has_pick && (response.hovered() || response.has_focus());
    let ink = super::paint_control(ui, rect, look, hovered, has_pick && response.is_pointer_button_down_on());
    let mut job = egui::text::LayoutJob::default();
    let font = FontId::proportional(theme::SIZE_S);
    job.append("Load ", 0.0, egui::TextFormat::simple(font.clone(), ink));
    job.append(theme::DECK_LETTERS[deck], 0.0,
               egui::TextFormat::simple(theme::display(theme::SIZE_S), if has_pick { theme::DECK_COLOURS[deck] } else { ink }));
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    if response.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), theme::R_M + 2.0, Stroke::new(theme::LINE_MID, theme::BLUE),
                                 egui::StrokeKind::Outside);
    }

    if has_pick && response.clicked() {
        if let Some(index) = app.selected {
            if index < app.records.len() {
                let record = app.records[index].clone();
                app.load(deck, record);
            }
        }
    }
    if !has_pick {
        response.on_hover_text("Pick a record in the crate first.");
    }
}

/* ── 3. Deck row ─────────────────────────────────────────────────────── */
pub fn deck_row(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();
    if full.height() < 60.0 || full.width() < 200.0 {
        return;
    }

    // Each side wants a pitch column, a jog and an EQ cluster; whatever is
    // left in the middle is the beat view, which is the part that benefits
    // most from extra width.
    let jog = (full.height() - 2.0 * theme::SP_2).clamp(JOG, JOG_MOST);
    let side = (56.0 + jog + 180.0 + 40.0).min((full.width() - 180.0) / 2.0);
    let (left, centre, right) = super::split_thirds(full, full.width() - side * 2.0 - 12.0, 6.0);

    deck_side(app, ui, 0, left);
    beat_view(app, ui, centre);
    deck_side(app, ui, 1, right);
}

fn deck_side(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let mut scoped = super::child(ui, rect, super::left_row(), if deck == 0 { "deckA" } else { "deckB" });
    let ui = &mut scoped;
    super::plate(ui, rect);
    // The controls keep to a block of the height they can use, centred in
    // a tall row, so a folded library does not strand them at the top.
    let inner = rect.shrink(theme::SP_2);
    let inner = Rect::from_center_size(inner.center(), vec2(inner.width(), inner.height().min(DECK_CONTROLS)));
    let mirrored = deck == 1;

    // Outermost: pitch. Then the jog. Then the EQ, nearest the middle.
    let pitch_x = if mirrored { inner.right() - 56.0 } else { inner.left() };
    let pitch = Rect::from_min_size(egui::pos2(pitch_x, inner.top()), vec2(56.0, inner.height()));

    let jog_size = (inner.height().clamp(JOG, JOG_MOST)).min((inner.width() - 56.0 - 16.0 - 130.0).max(116.0));
    let jog_x = if mirrored { pitch.left() - 8.0 - jog_size } else { pitch.right() + 8.0 };
    let jog = Rect::from_min_size(
        egui::pos2(jog_x, inner.top() + (inner.height() - jog_size).max(0.0) / 2.0),
        vec2(jog_size, jog_size.min(inner.height())),
    );

    let eq = if mirrored {
        Rect::from_min_max(inner.min, egui::pos2(jog.left() - 8.0, inner.bottom()))
    } else {
        Rect::from_min_max(egui::pos2(jog.right() + 8.0, inner.top()), inner.max)
    };

    pitch_column(app, ui, deck, pitch);
    jog_wheel(app, ui, deck, jog);
    eq_cluster(app, ui, deck, eq);
}

fn pitch_column(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let ready = app.decks[deck].record.is_some();
    let key = vec2(50.0, theme::CONTROL_S);

    let mut column = super::child(ui, rect, egui::Layout::top_down(egui::Align::Center), "pitch");
    column.spacing_mut().item_spacing.y = theme::SP_1;

    if super::chip(&mut column, "SYNC", key, false, ready)
        .on_hover_text("Match this deck's tempo to the other one").clicked() {
        if let Err(error) = app.sync(deck) {
            app.decks[deck].error = Some(error);
        }
    }
    let phased = ready && app.grid(deck).is_some() && app.grid(1 - deck).is_some();
    if super::chip(&mut column, "PHASE", key, false, phased)
        .on_hover_text("Line this deck up on the other deck's beat").clicked() {
        if let Err(error) = app.phase_sync(deck) {
            app.say(&error);
        }
    }

    // The tempo itself is on the platter; this is only how far the fader
    // has moved it.
    column.label(
        egui::RichText::new(format!("{:+.1}%", app.decks[deck].pitch))
            .font(FontId::monospace(theme::SIZE_XS))
            .color(if app.decks[deck].pitch.abs() > 0.01 { theme::DECK_COLOURS[deck] } else { theme::TEXT_DIM }),
    );

    if super::chip(&mut column, "RESET", key, false, ready)
        .on_hover_text("Return tempo to 0% and release pitch bend").clicked() {
        app.reset_tempo(deck);
    }
    if super::button(&mut column, "KEY", key,
                     Look::secondary(app.decks[deck].key_lock, ready).accent(theme::DECK_COLOURS[deck]))
        .on_hover_text("Key lock: preserve musical pitch when changing tempo. Scratching and reverse use the original deck path.").clicked() {
        app.toggle_key_lock(deck);
    }
    let fader_height = (column.available_height() - theme::CONTROL_S - theme::SP_1).clamp(40.0, 200.0);
    let mut pitch = app.decks[deck].pitch;
    let id = egui::Id::new(("pitch", deck));
    let travel = widgets::Travel::bipolar(-8.0, 8.0);
    let readout = |v: f32| format!("{v:+.2}%");
    let scale = widgets::Scale { readout: Some(&readout), ..Default::default() };
    if widgets::fader_with(&mut column, id, &mut pitch, vec2(44.0, fader_height), travel, "Pitch", &scale)
        .changed()
    {
        app.set_pitch(deck, pitch);
    }

    column.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let nudge = vec2(24.0, theme::CONTROL_S);
        if super::glyph_chip(ui, super::Glyph::Minus, nudge, ready).on_hover_text("Tempo down 0.1%").clicked() {
            let next = (app.decks[deck].pitch - 0.1).max(-8.0);
            app.set_pitch(deck, next);
        }
        if super::glyph_chip(ui, super::Glyph::Plus, nudge, ready).on_hover_text("Tempo up 0.1%").clicked() {
            let next = (app.decks[deck].pitch + 0.1).min(8.0);
            app.set_pitch(deck, next);
        }
    });
}

/// How close the end of the record is, as the platter shows it: nothing
/// until the last half minute, then a pulse towards red -- held steady
/// instead when the deck is stopped or motion is reduced.
pub fn ending(remaining: f64, length: f64, playing: bool, reduced: bool, time: f64) -> f32 {
    if length <= 30.0 || remaining > 30.0 || remaining <= 0.0 {
        return 0.0;
    }
    if reduced || !playing {
        return 0.75;
    }
    0.5 + 0.5 * (time * std::f64::consts::TAU * 1.2).sin() as f32
}

fn jog_wheel(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let state = &app.decks[deck];
    let progress = if state.length > 0.0 { (state.position / state.length) as f32 } else { 0.0 };
    let loaded = state.record.is_some();
    let spin = state.spin;
    let time = ui.input(|i| i.time);
    let dress = widgets::Dress {
        colour: theme::DECK_COLOURS[deck],
        ending: if loaded {
            ending(state.length - state.position, state.length, state.playing, app.studio.reduced, time)
        } else {
            0.0
        },
    };

    let mut here = super::child(ui, rect, super::left_row(), "jog");
    let diameter = rect.width().min(rect.height());
    let out = widgets::platter(
        &mut here,
        egui::Id::new(("jog", deck)),
        diameter,
        spin,
        progress,
        loaded,
        dress,
    );

    if out.response.drag_started() && loaded {
        app.scrub(deck, Some(0.0));
    }
    let turned = out.turned + out.nudged;
    if (out.response.dragged() || out.nudged != 0.0) && loaded {
        // A full turn moves 1.8 seconds, which is where the strobe was set.
        let moved = (turned / std::f32::consts::TAU) as f64 * 1.8;
        let to = app.decks[deck].position + moved;
        app.decks[deck].spin = (app.decks[deck].spin + turned).rem_euclid(std::f32::consts::TAU);
        app.seek(deck, to);
    }
    if out.response.drag_stopped() {
        app.scrub(deck, None);
    }

    // The label carries the numbers once there is a record to report: the
    // tempo large, where the record is under it.
    let centre = out.response.rect.center();
    if loaded {
        let size = (diameter * 0.137).clamp(theme::SIZE_L, theme::SIZE_XXL);
        ui.painter().text(centre - vec2(0.0, 7.0), Align2::CENTER_CENTER,
                          app.decks[deck].tempo().map_or("--".into(), |t| format!("{t:.1}")),
                          theme::readout(size), theme::TEXT_BRIGHT);
        super::mono(ui, centre + vec2(0.0, size * 0.5 + 4.0), Align2::CENTER_CENTER,
                    &super::tenths(app.decks[deck].position), theme::SIZE_XS, theme::TEXT_DIM);
    } else {
        ui.painter().text(
            centre,
            Align2::CENTER_CENTER,
            "defalt",
            theme::display(theme::SIZE_M),
            theme::TEXT_MUTE,
        );
    }
}

/// A band's knob position as the isolator hears it.
fn band_readout(position: f32) -> String {
    match crate::engine::filters::band_db(position) {
        None => "kill".into(),
        Some(db) if db.abs() < 0.05 => "0.0 dB".into(),
        Some(db) => format!("{db:+.1} dB"),
    }
}

/// The sweep as the filter hears it.
fn sweep_readout(sweep: f32) -> String {
    match crate::engine::filters::sweep_cutoff(sweep) {
        None => "off".into(),
        Some((low_pass, hz)) => {
            let side = if low_pass { "LP" } else { "HP" };
            if hz >= 1000.0 { format!("{side} {:.1} kHz", hz / 1000.0) } else { format!("{side} {hz:.0} Hz") }
        }
    }
}

/// A channel fader's gain, in dB from unity.
fn gain_readout(gain: f32) -> String {
    if gain <= 0.001 {
        return "-\u{221e} dB".into();
    }
    let db = 20.0 * gain.log10();
    if db.abs() < 0.05 { "0.0 dB".into() } else { format!("{db:+.1} dB") }
}

/// Where the volume fader's marks sit, as linear gain: silence, -12 dB,
/// unity and +6 dB.
const VOL_MARKS: [(f32, &str); 4] = [(0.0, "-\u{221e}"), (0.251, "-12"), (1.0, "0"), (2.0, "+6")];

fn eq_cluster(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    if rect.width() < 70.0 {
        return;
    }
    let mirrored = deck == 1;
    let colour = theme::DECK_COLOURS[deck];

    // EQ dropdown across the top, knobs in a column, filter fader beside them
    // on whichever side faces away from the middle.
    let head = Rect::from_min_size(rect.min, vec2(rect.width().min(96.0), 20.0));
    let mut head_ui = super::child(ui, head, super::left_row(), "eqhead");
    head_ui.label(super::rich("Equalizer", theme::SIZE_S, theme::TEXT_DIM));

    let body = Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 26.0), rect.max);

    // Knobs nearest the jog, then filter, then volume hard against the middle
    // -- so both decks' volume faders sit either side of the crossfader and
    // your hands find all three without looking. Mirrored for deck B. The
    // volume column is the widest: it carries the meter and the dB scale.
    let knob_width = 52.0;
    let rest = (body.width() - knob_width).max(60.0);
    let filter_width = (rest * 0.42).clamp(30.0, 52.0);
    let mut columns = [knob_width, filter_width, rest - filter_width];
    if mirrored {
        columns.reverse();
    }
    let mut x = body.left();
    let mut slots = Vec::with_capacity(3);
    for width in columns {
        slots.push(Rect::from_min_size(egui::pos2(x, body.top()), vec2(width, body.height())));
        x += width;
    }
    let (knobs, filter, volume) = if mirrored {
        (slots[2], slots[1], slots[0])
    } else {
        (slots[0], slots[1], slots[2])
    };

    let mut column = super::child(ui, knobs, egui::Layout::top_down(egui::Align::Center), "eq");
    // Three knobs and their captions fill the column; smaller when the row
    // is short, never larger than their natural size, and spread out rather
    // than bunched at the top when the row is tall.
    let diameter = ((knobs.height() - 3.0 * widgets::KNOB_CAPTION - 2.0) / 3.0).clamp(30.0, widgets::KNOB);
    let spare = knobs.height() - 3.0 * (diameter + widgets::KNOB_CAPTION);
    column.spacing_mut().item_spacing.y = (spare / 3.0).clamp(1.0, theme::SP_4);
    for (slot, name) in [(2usize, "HIGH"), (1, "MID"), (0, "LOW")] {
        let mut value = app.decks[deck].tone[slot];
        let id = egui::Id::new(("eq", deck, slot));
        let killed = app.decks[deck].killed[slot];
        // A killed band shows its kill rather than its knob, so the panel
        // never disagrees with what you are hearing.
        let accent = if killed {
            theme::RED
        } else if app.airtime.held.tone[deck][slot] {
            // Yours. Distinct from a kill, which is red, and from the
            // deck's own colour, which is the station's hand or nobody's.
            theme::TEXT_BRIGHT
        } else {
            colour
        };
        if widgets::knob_with(&mut column, id, &mut value, false, name, accent, diameter, Some(&band_readout))
            .changed()
        {
            app.take_over(crate::Take::Tone(deck, slot));
            app.decks[deck].tone[slot] = value;
            // Touching a killed band takes the kill off: you reached for it.
            app.decks[deck].killed[slot] = false;
            app.push_tone(deck);
        }
    }

    labelled_fader(ui, filter, "FILTER", |ui, height| {
        let mut sweep = app.decks[deck].tone[3];
        let id = egui::Id::new(("filter", deck));
        let width = filter.width().min(44.0);
        let travel = widgets::Travel::bipolar(-1.0, 1.0);
        let scale = widgets::Scale {
            ends: Some(("HP", "LP")),
            readout: Some(&sweep_readout),
            ..Default::default()
        };
        if widgets::fader_with(ui, id, &mut sweep, vec2(width, height), travel, "Filter", &scale).changed() {
            app.take_over(crate::Take::Tone(deck, 3));
            app.decks[deck].tone[3] = sweep;
            app.push_tone(deck);
        }
    });

    // The volume fader with its meter hugging the side nearest the middle,
    // and its scale on the side away from it.
    let meter_width = 6.0;
    let gap = 3.0;
    let labelled = volume.width() >= 54.0 + gap + meter_width;
    let fader_width = if labelled { 54.0 } else { (volume.width() - gap - meter_width).clamp(28.0, 44.0) };
    let group = fader_width + gap + meter_width;
    let left = volume.center().x - group / 2.0;
    let (fader_left, meter_left) = if mirrored {
        (left + meter_width + gap, left)
    } else {
        (left, left + fader_width + gap)
    };
    let fader_slot = Rect::from_min_max(egui::pos2(fader_left, volume.top()), egui::pos2(fader_left + fader_width, volume.bottom()));
    let scale = widgets::Scale {
        marks: &VOL_MARKS,
        home: Some(1.0),
        side: if mirrored { widgets::Side::Right } else { widgets::Side::Left },
        ends: None,
        readout: Some(&gain_readout),
    };
    let mut travel_span = None;
    labelled_fader(ui, volume, "VOL", |ui, height| {
        let mut gain = app.decks[deck].gain;
        let id = egui::Id::new(("vol", deck));
        // Unity is half way up: the top of the travel is +6 dB of headroom,
        // not where a channel rests.
        let travel = widgets::Travel::level(2.0, 1.0);
        let mut seat = super::child(ui, Rect::from_min_size(egui::pos2(fader_slot.left(), ui.cursor().top()),
                                                            vec2(fader_slot.width(), height)),
                                    super::left_row(), "volseat");
        let response = widgets::fader_with(&mut seat, id, &mut gain, vec2(fader_width, height), travel, "Volume", &scale);
        travel_span = Some(widgets::fader_travel(response.rect, &scale));
        if response.changed() {
            app.take_over(crate::Take::Gain(deck));
            app.decks[deck].gain = gain;
            app.push_gains();
        }
    });
    if let Some((bottom, top)) = travel_span {
        let meter = Rect::from_min_max(egui::pos2(meter_left, top - 7.0), egui::pos2(meter_left + meter_width, bottom + 7.0));
        let peak = app.decks[deck].meter;
        widgets::level_meter(ui, meter, egui::Id::new(("deck-meter", deck)), peak, peak >= 0.999, colour, true);
    }
}

/// A vertical fader under a silkscreen caption, filling its column.
fn labelled_fader(ui: &mut Ui, rect: Rect, caption: &str, draw: impl FnOnce(&mut Ui, f32)) {
    let mut column = super::child(
        ui,
        rect,
        egui::Layout::top_down(egui::Align::Center),
        caption,
    );
    super::column_cap(&column, rect, caption);
    column.add_space(theme::SP_3 + 2.0);
    let height = (rect.height() - 18.0).clamp(40.0, 260.0);
    draw(&mut column, height);
}

/* ── The beat view, between the decks ────────────────────────────────── */
fn beat_view(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    super::well(ui, rect);
    if rect.height() < 50.0 {
        return;
    }
    let inner = rect.shrink(3.0);
    let lane_height = inner.height() / 2.0;

    for deck in 0..2 {
        let lane = Rect::from_min_size(
            egui::pos2(inner.left(), inner.top() + deck as f32 * lane_height),
            vec2(inner.width(), lane_height),
        );
        let state = &app.decks[deck];
        let window = app.window_seconds(deck);
        waveform::lane(
            ui.painter(),
            lane.shrink2(vec2(0.0, 2.0)),
            state.peaks.as_deref(),
            state.record.as_ref(),
            state.position,
            window,
            theme::DECK_COLOURS[deck],
            &state.cues,
            app.view_state.wave_mode,
        );

        waveform::transition_marks(ui.painter(), lane.shrink2(vec2(0.0, 2.0)),
            &app.view_state.windows[deck], state.position - window / 2.0, state.position + window / 2.0);

        // The loop, where the deck is really looping.
        if let Some((start, end)) = state.loop_range {
            let from = state.position - window / 2.0;
            let x = |t: f64| lane.left() + (((t - from) / window) as f32).clamp(0.0, 1.0) * lane.width();
            let span = Rect::from_min_max(egui::pos2(x(start), lane.top() + 2.0), egui::pos2(x(end), lane.bottom() - 2.0));
            if span.width() > 0.5 {
                ui.painter().rect_filled(span, theme::R_S, theme::GREEN.gamma_multiply(0.16));
                for edge in [span.left(), span.right()] {
                    ui.painter().line_segment([egui::pos2(edge, span.top()), egui::pos2(edge, span.bottom())],
                                              Stroke::new(theme::LINE_MID, theme::GREEN));
                }
            }
        }

        let tag = Rect::from_min_size(lane.min + vec2(6.0, 5.0), vec2(22.0, 20.0));
        super::deck_badge(ui, tag, deck);
    }

    // One playhead, dead centre, both lanes: two grids either line up on it
    // or visibly do not. A dark keyline either side keeps it distinct over
    // the amber of a busy midrange.
    let x = inner.center().x;
    for dx in [-1.5, 1.5] {
        ui.painter().line_segment(
            [egui::pos2(x + dx, inner.top()), egui::pos2(x + dx, inner.bottom())],
            Stroke::new(theme::LINE, Color32::from_black_alpha(150)),
        );
    }
    ui.painter().line_segment(
        [egui::pos2(x, inner.top()), egui::pos2(x, inner.bottom())],
        Stroke::new(theme::LINE_BOLD, theme::PLAYHEAD),
    );
    ui.painter().line_segment(
        [egui::pos2(inner.left(), inner.center().y), egui::pos2(inner.right(), inner.center().y)],
        Stroke::new(theme::LINE, Color32::from_black_alpha(170)),
    );

    scrub_lanes(app, ui, inner);
    zoom_control(app, ui, inner);
}

fn scrub_lanes(app: &mut Defalt, ui: &mut Ui, inner: Rect) {
    let response = ui.interact(inner, ui.id().with("beatview"), Sense::click_and_drag());
    let key = egui::Id::new("scrub_deck");

    if response.drag_started() {
        if let Some(at) = ui.ctx().pointer_interact_pos() {
            let deck = if at.y < inner.center().y { 0 } else { 1 };
            if app.decks[deck].record.is_some() {
                app.scrub(deck, Some(0.0));
                ui.ctx().data_mut(|d| d.insert_temp(key, deck));
            }
        }
    }
    if response.dragged() {
        if let Some(deck) = ui.ctx().data(|d| d.get_temp::<usize>(key)) {
            let across = app.window_seconds(deck);
            let moved = -response.drag_delta().x as f64 / inner.width() as f64 * across;
            let to = app.decks[deck].position + moved;
            app.seek(deck, to);
        }
    }
    if response.drag_stopped() {
        if let Some(deck) = ui.ctx().data(|d| d.get_temp::<usize>(key)) {
            app.scrub(deck, None);
            ui.ctx().data_mut(|d| d.remove::<usize>(key));
        }
    }
}

fn zoom_control(app: &mut Defalt, ui: &mut Ui, inner: Rect) {
    let rect = Rect::from_min_size(inner.right_top() + vec2(-116.0, 4.0), vec2(112.0, theme::CONTROL_S + 4.0));
    ui.painter().rect_filled(rect, theme::R_M, theme::PANEL.gamma_multiply(0.9));
    let mut bar = super::child(ui, rect.shrink2(vec2(2.0, 0.0)), super::left_row(), "zoom");
    bar.spacing_mut().item_spacing.x = 2.0;

    let size = vec2(theme::CONTROL_S, theme::CONTROL_S);
    if super::glyph_button(&mut bar, super::Glyph::Minus, size, Look::ghost(false, true), "Zoom out").clicked() {
        app.bars = (app.bars * 2).min(32);
    }
    let label = format!("{} bar{}", app.bars, if app.bars == 1 { "" } else { "s" });
    bar.add_sized(vec2(56.0, theme::CONTROL_S),
                  egui::Label::new(super::rich(&label, theme::SIZE_XS, theme::TEXT_DIM)));
    if super::glyph_button(&mut bar, super::Glyph::Plus, size, Look::ghost(false, true), "Zoom in").clicked() {
        app.bars = (app.bars / 2).max(1);
    }
}

/* ── 4. Transport + crossfader ───────────────────────────────────────── */
pub fn transport(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();
    let (left, centre, right) = super::split_thirds(full, 340.0, 8.0);

    deck_transport(app, ui, 0, left);
    crossfader(app, ui, centre);
    deck_transport(app, ui, 1, right);
}

/// One key of a deck's transport, in the order it is read left to right.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Play,
    Cue,
    Gap,
    Pad(usize),
    In,
    Out,
    Loop,
    Halve,
    Double,
}

/// Deck A's transport, left to right from the jog outward.
pub const TRANSPORT_A: [Key; 13] = [
    Key::Play, Key::Cue, Key::Gap, Key::Pad(0), Key::Pad(1), Key::Pad(2), Key::Pad(3),
    Key::Gap, Key::In, Key::Out, Key::Loop, Key::Halve, Key::Double,
];

/// Deck B's, left to right: the groups mirror about the middle so play is
/// still nearest its own platter, but inside each group the keys keep the
/// same order as deck A's.
pub const TRANSPORT_B: [Key; 13] = [
    Key::In, Key::Out, Key::Loop, Key::Halve, Key::Double, Key::Gap,
    Key::Pad(0), Key::Pad(1), Key::Pad(2), Key::Pad(3), Key::Gap, Key::Cue, Key::Play,
];

fn deck_transport(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let ready = app.decks[deck].record.is_some();
    let playing = app.decks[deck].playing;
    // Deck B is laid out from the right, so its keys are placed in reverse
    // and come out reading left to right.
    let (layout, keys): (_, Vec<Key>) = if deck == 0 {
        (super::left_row(), TRANSPORT_A.to_vec())
    } else {
        (super::right_row(), TRANSPORT_B.iter().rev().copied().collect())
    };
    let mut bar = super::child(ui, rect, layout, if deck == 0 { "tpA" } else { "tpB" });
    bar.spacing_mut().item_spacing.x = 5.0;
    let height = theme::CONTROL_M;
    let mut gaps = 0;

    for key in keys {
        match key {
            Key::Gap => {
                gaps += 1;
                // The loop group only when there is room for all of it.
                if gaps == 2 && bar.available_width() < 226.0 {
                    return;
                }
                bar.add_space(theme::SP_2);
            }
            Key::Play => {
                if play_button(&mut bar, playing, ready, theme::DECK_COLOURS[deck]).clicked() {
                    app.play_pause(deck);
                }
            }
            Key::Cue => {
                let look = Look::secondary(false, ready).accent(theme::AMBER).outlined().momentary();
                if super::button(&mut bar, "CUE", vec2(52.0, height), look)
                    .on_hover_text("Back to the start of this track").clicked() {
                    app.cue(deck);
                }
            }
            Key::Pad(slot) => {
                let set = app.decks[deck].cues[slot].is_some();
                let look = if set {
                    Look::secondary(true, ready).accent(theme::CUE_COLOURS[slot])
                } else {
                    Look::ghost(false, ready).accent(theme::CUE_COLOURS[slot].gamma_multiply(0.45)).outlined()
                };
                let hit = super::button(&mut bar, &format!("C{}", slot + 1), vec2(36.0, height), look)
                    .on_hover_text("Click to set an empty cue or jump to it. Alt-click to replace it.");
                if hit.clicked() {
                    if !set || ui.input(|i| i.modifiers.alt) { app.set_cue(deck, slot); }
                    else { app.jump_to_cue(deck, slot); }
                }
            }
            Key::In => {
                let waiting = app.decks[deck].loop_in.is_some();
                if super::button(&mut bar, "IN", vec2(34.0, height), Look::secondary(waiting, ready).accent(theme::GREEN))
                    .on_hover_text("Loop in here").clicked() {
                    app.set_loop_in(deck);
                }
            }
            Key::Out => {
                let waiting = app.decks[deck].loop_in.is_some();
                if super::chip(&mut bar, "OUT", vec2(40.0, height), false, ready && waiting)
                    .on_hover_text("Loop out here").clicked() {
                    app.set_loop_out(deck);
                }
            }
            Key::Loop => {
                let looping = app.decks[deck].loop_range.is_some();
                let beats = app.decks[deck].loop_beats;
                if super::button(&mut bar, &format!("LOOP {beats}"), vec2(62.0, height),
                                 Look::secondary(looping, ready).accent(theme::GREEN))
                    .on_hover_text(if looping { "Leave the loop" } else { "Loop this many beats, from the beat you are in" })
                    .clicked() {
                    app.toggle_loop(deck);
                }
            }
            Key::Halve => {
                let beats = app.decks[deck].loop_beats;
                if super::chip(&mut bar, "\u{f7}2", vec2(32.0, height), false, ready && beats > 1)
                    .on_hover_text("Halve the loop").clicked() {
                    app.halve_loop(deck);
                }
            }
            Key::Double => {
                let beats = app.decks[deck].loop_beats;
                if super::chip(&mut bar, "\u{d7}2", vec2(32.0, height), false, ready && beats < 16)
                    .on_hover_text("Double the loop").clicked() {
                    app.double_loop(deck);
                }
            }
        }
    }
}

/// Play. Solid green and glowing while the record plays; outlined in the
/// deck's colour while it is loaded and waiting.
fn play_button(ui: &mut Ui, playing: bool, live: bool, deck: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(54.0, 36.0), if live { Sense::click() } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, live, playing,
                                                         if playing { "Pause" } else { "Play" }));
    let hovered = live && (response.hovered() || response.has_focus());
    let pressed = live && response.is_pointer_button_down_on();
    let ink = if playing && live {
        let look = Look::primary(true).accent(theme::GREEN);
        super::paint_control(ui, rect, look, hovered, pressed)
    } else {
        let look = Look::secondary(false, live).accent(deck).outlined();
        super::paint_control(ui, rect, look, hovered, pressed)
    };
    let ink = if live && !playing { theme::TEXT_BRIGHT } else { ink };

    let centre = rect.center();
    if playing {
        for dx in [-3.5, 3.5] {
            ui.painter().rect_filled(
                Rect::from_center_size(centre + vec2(dx, 0.0), vec2(3.0, 12.0)),
                1.0,
                ink,
            );
        }
    } else {
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                centre + vec2(-4.5, -7.0),
                centre + vec2(7.0, 0.0),
                centre + vec2(-4.5, 7.0),
            ],
            ink,
            Stroke::NONE,
        ));
    }
    if response.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), theme::R_M + 2.0, Stroke::new(theme::LINE_MID, theme::BLUE),
                                 egui::StrokeKind::Outside);
    }
    response
}

fn crossfader(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    let mut bar = super::child(ui, rect, super::left_row(), "xf");
    bar.spacing_mut().item_spacing.x = 6.0;

    let letter = |deck: usize| egui::RichText::new(theme::DECK_LETTERS[deck])
        .font(theme::display(theme::SIZE_S)).color(theme::DECK_COLOURS[deck]);
    bar.label(letter(0));
    let width = (bar.available_width() - 26.0).max(60.0);
    let mut crossfade = app.crossfade;
    if widgets::crossfader(&mut bar, egui::Id::new("crossfader"), &mut crossfade, vec2(width, 30.0))
        .changed()
    {
        app.take_over(crate::Take::Crossfade);
        app.crossfade = crossfade;
        app.push_gains();
    }
    bar.label(letter(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_transports_read_c1_to_c4_and_in_to_double_left_to_right() {
        let order = |keys: &[Key]| keys.iter().filter_map(|k| match k {
            Key::Pad(slot) => Some(format!("C{}", slot + 1)),
            Key::In => Some("IN".into()),
            Key::Out => Some("OUT".into()),
            Key::Loop => Some("LOOP".into()),
            Key::Halve => Some("/2".into()),
            Key::Double => Some("x2".into()),
            _ => None,
        }).collect::<Vec<_>>();
        let pads = |keys: &[Key]| order(keys).into_iter().filter(|k| k.starts_with('C')).collect::<Vec<_>>();
        let loops = |keys: &[Key]| order(keys).into_iter().filter(|k| !k.starts_with('C')).collect::<Vec<_>>();
        assert_eq!(pads(&TRANSPORT_A), ["C1", "C2", "C3", "C4"]);
        assert_eq!(pads(&TRANSPORT_B), pads(&TRANSPORT_A));
        assert_eq!(loops(&TRANSPORT_B), loops(&TRANSPORT_A));
        // Play stays nearest its own platter: outermost on each side.
        assert_eq!(TRANSPORT_A[0], Key::Play);
        assert_eq!(TRANSPORT_B[TRANSPORT_B.len() - 1], Key::Play);
    }

    #[test]
    fn readouts_speak_in_the_units_the_audio_uses() {
        assert_eq!(gain_readout(1.0), "0.0 dB");
        assert_eq!(gain_readout(2.0), "+6.0 dB");
        assert_eq!(gain_readout(0.0), "-\u{221e} dB");
        assert_eq!(band_readout(0.5), "0.0 dB");
        assert_eq!(band_readout(1.0), "+6.0 dB");
        assert_eq!(band_readout(0.0), "kill");
        assert_eq!(sweep_readout(0.0), "off");
        assert!(sweep_readout(-1.0).starts_with("LP 120 Hz"));
        assert!(sweep_readout(1.0).starts_with("HP 9.0 kHz"));
        // The scale's -12 mark is where -12 dB actually is.
        assert_eq!(gain_readout(VOL_MARKS[1].0), "-12.0 dB");
    }

    #[test]
    fn the_platter_warns_only_in_the_last_half_minute() {
        assert_eq!(ending(31.0, 200.0, true, false, 0.0), 0.0);
        assert_eq!(ending(20.0, 200.0, false, false, 0.0), 0.75);
        assert_eq!(ending(20.0, 200.0, true, true, 3.3), 0.75, "reduced motion holds it steady");
        let pulse: Vec<f32> = (0..10).map(|i| ending(20.0, 200.0, true, false, i as f64 * 0.1)).collect();
        assert!(pulse.iter().any(|p| *p > 0.8) && pulse.iter().any(|p| *p < 0.2), "{pulse:?}");
        assert_eq!(ending(10.0, 25.0, true, false, 0.0), 0.0, "a short record is all ending");
    }
}
