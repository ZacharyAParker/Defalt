//! The overview strip, the deck row and the transport.
//!
//! Both decks are mirrored about the centre, which is the whole reason a DJ
//! panel is laid out this way: your left hand and your right hand reach the
//! same control at the same distance from the middle.

use egui::{vec2, Align2, FontId, Rect, Sense, Stroke, Ui};

use super::{theme, waveform, widgets};
use crate::Defalt;

const JOG: f32 = 204.0;

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
    }
}

fn overview_pane(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let loader_side = if deck == 0 { rect.left() + 8.0 } else { rect.right() - 76.0 };
    let loader = Rect::from_min_size(egui::pos2(loader_side, rect.top() + 8.0), vec2(68.0, 30.0));

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
            wave.left_top() + vec2(6.0, 4.0),
            Align2::LEFT_TOP,
            "Loading...",
            12.0,
            theme::TEXT_DIM,
        );
    } else if let Some(error) = &state.error {
        super::label(ui, wave.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP,
                     "Would not load", 12.0, theme::RED);
        super::label(ui, wave.left_top() + vec2(6.0, 20.0), Align2::LEFT_TOP,
                     &super::elide(error, 70), 10.0, theme::TEXT_DIM);
    } else if state.record.is_some() {
        // The waveform fills the well and the text sits on it, as on the
        // reference. A strip this short cannot afford a separate text row.
        waveform::overview(
            ui.painter(),
            wave,
            state.peaks.as_deref(),
            state.position,
            state.length,
            theme::DECK_COLOURS[deck],
        );

        // A scrim under the text so it stays legible over a loud record.
        let scrim = Rect::from_min_size(wave.min, vec2(wave.width(), 39.0));
        ui.painter().rect_filled(
            scrim,
            egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 },
            egui::Color32::from_black_alpha(150),
        );

        let title = state.record.as_ref().map_or(String::new(), |r| r.title.clone());
        let artist = state.record.as_ref().map_or(String::new(), |r| r.artist.clone());
        let text_at = wave.left_top() + vec2(6.0, 3.0);
        let title_width = (wave.width() - 102.0).max(40.0);
        super::clipped_label(ui, Rect::from_min_size(text_at, vec2(title_width, 20.0)), &title, 15.0, theme::TEXT);
        super::clipped_label(ui, Rect::from_min_size(text_at + vec2(0.0, 21.0), vec2(title_width, 16.0)), &artist, 12.0, theme::TEXT_DIM);

        // A record the station put here says so, because a deck that starts
        // playing on its own is alarming if nothing on screen claims it.
        if app.airtime.on_deck(deck).is_some() {
            let held = app.airtime.held.tone[deck].iter().any(|h| *h)
                || app.airtime.held.gain[deck];
            let (text, colour) = if held {
                ("AUTO / YOU", theme::PLAYHEAD)
            } else {
                ("AUTO", theme::BLUE)
            };
            super::mono(ui, text_at + vec2(0.0, 40.0), Align2::LEFT_TOP, text, 10.0, colour);
        }

        let remaining = (state.length - state.position).max(0.0);
        super::mono(ui, egui::pos2(wave.right() - 6.0, wave.top() + 3.0), Align2::RIGHT_TOP,
                    &format!("-{}", super::mmss(remaining)), 13.0, theme::TEXT);
        if let Some(camelot) = state.record.as_ref().and_then(|r| r.camelot.clone()) {
            super::mono(ui, egui::pos2(wave.right() - 6.0, wave.top() + 19.0), Align2::RIGHT_TOP,
                        &camelot, 10.0, theme::CYAN);
        }

        let windows = app.airtime.transition_windows(deck);
        let marker_rect = Rect::from_min_max(
            egui::pos2(wave.left(), (wave.top() + 55.0).min(wave.bottom() - 18.0)), wave.max);
        waveform::transition_marks(ui.painter(), marker_rect, &windows, 0.0, state.length);
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
        super::label(ui, wave.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER,
            &format!("Deck {} is empty", if deck == 0 { "A" } else { "B" }), 15.0, theme::TEXT);
        super::label(ui, wave.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER,
            "Choose a track below, then load it here", 12.0, theme::TEXT_DIM);
    }

    load_button(app, ui, deck, loader);
}

/// The rounded-square note button. Loads whatever the crate has selected.
fn load_button(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let has_pick = app.selected.is_some() && app.engine_ready();
    let response = ui.interact(rect, ui.id().with(("load", deck)), if has_pick { Sense::click() } else { Sense::hover() });

    let fill = if response.hovered() && has_pick { theme::RAISED_HI } else { theme::RAISED };
    ui.painter().rect_filled(rect, 5.0, fill);
    ui.painter()
        .rect_stroke(rect, 5.0, Stroke::new(1.0, theme::EDGE), egui::StrokeKind::Inside);

    let ink = if has_pick { theme::TEXT } else { theme::TEXT_MUTE };
    super::label(ui, rect.center(), Align2::CENTER_CENTER, if deck == 0 { "Load A" } else { "Load B" }, 12.0, ink);

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
    let side = (56.0 + JOG + 150.0 + 40.0).min((full.width() - 180.0) / 2.0);
    let (left, centre, right) = super::split_thirds(full, full.width() - side * 2.0 - 12.0, 6.0);

    deck_side(app, ui, 0, left);
    beat_view(app, ui, centre);
    deck_side(app, ui, 1, right);
}

fn deck_side(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let mut scoped = super::child(ui, rect, super::left_row(), if deck == 0 { "deckA" } else { "deckB" });
    let ui = &mut scoped;
    super::plate(ui, rect);
    let inner = rect.shrink(8.0);
    let mirrored = deck == 1;

    // Outermost: pitch. Then the jog. Then the EQ, nearest the middle.
    let pitch_x = if mirrored { inner.right() - 56.0 } else { inner.left() };
    let pitch = Rect::from_min_size(egui::pos2(pitch_x, inner.top()), vec2(56.0, inner.height()));

    let jog_size = JOG.min((inner.width() - 56.0 - 16.0 - 130.0).max(116.0));
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

    let mut column = super::child(ui, rect, egui::Layout::top_down(egui::Align::Center), "pitch");
    column.spacing_mut().item_spacing.y = 5.0;

    if super::chip(&mut column, "SYNC", vec2(50.0, 20.0), false, ready).clicked() {
        if let Err(error) = app.sync(deck) {
            app.decks[deck].error = Some(error);
        }
    }

    let tempo = app.decks[deck]
        .tempo()
        .map_or("--".to_string(), |t| format!("{t:.1}"));
    column.label(super::rich(&tempo, 10.5, theme::TEXT_DIM));
    column.label(super::rich(
        &format!("{:+.1}%", app.decks[deck].pitch),
        10.5,
        if app.decks[deck].pitch.abs() > 0.01 { theme::BLUE } else { theme::TEXT_DIM },
    ));

    if super::chip(&mut column, "RESET", vec2(50.0, 18.0), false, ready)
        .on_hover_text("Return tempo to 0% and release pitch bend").clicked() {
        app.reset_tempo(deck);
    }
    if super::chip(&mut column, "KEY", vec2(50.0, 18.0), app.decks[deck].key_lock, ready)
        .on_hover_text("Key lock: preserve musical pitch when changing tempo. Scratching and reverse use the original deck path.").clicked() {
        app.toggle_key_lock(deck);
    }
    let fader_height = (column.available_height() - 30.0).clamp(40.0, JOG - 119.0);
    let mut pitch = app.decks[deck].pitch;
    let id = egui::Id::new(("pitch", deck));
    if widgets::fader(&mut column, id, &mut pitch, vec2(44.0, fader_height), -8.0, 8.0, true)
        .changed()
    {
        app.set_pitch(deck, pitch);
    }

    column.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if super::glyph_chip(ui, super::Glyph::Minus, vec2(22.0, 18.0), ready).clicked() {
            let next = (app.decks[deck].pitch - 0.1).max(-8.0);
            app.set_pitch(deck, next);
        }
        if super::glyph_chip(ui, super::Glyph::Plus, vec2(22.0, 18.0), ready).clicked() {
            let next = (app.decks[deck].pitch + 0.1).min(8.0);
            app.set_pitch(deck, next);
        }
    });
}

fn jog_wheel(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let state = &app.decks[deck];
    let progress = if state.length > 0.0 { (state.position / state.length) as f32 } else { 0.0 };
    let loaded = state.record.is_some();
    let spin = state.spin;

    let mut here = super::child(ui, rect, super::left_row(), "jog");
    let out = widgets::platter(
        &mut here,
        egui::Id::new(("jog", deck)),
        rect.width().min(rect.height()),
        spin,
        progress,
        loaded,
    );

    if out.response.drag_started() && loaded {
        app.scrub(deck, Some(0.0));
    }
    if out.response.dragged() && loaded {
        // A full turn moves 1.8 seconds, which is where the strobe was set.
        let moved = (out.turned / std::f32::consts::TAU) as f64 * 1.8;
        let to = app.decks[deck].position + moved;
        app.decks[deck].spin += out.turned;
        app.seek(deck, to);
    }
    if out.response.drag_stopped() {
        app.scrub(deck, None);
    }

    // The wordmark sits in the middle of the platter, as on the reference,
    // and gives way to the numbers once there is a record to report.
    let centre = out.response.rect.center();
    if loaded {
        super::mono(ui, centre - vec2(0.0, 8.0), Align2::CENTER_CENTER,
                    &app.decks[deck].tempo().map_or("--".into(), |t| format!("{t:.1}")),
                    17.0, theme::TEXT);
        super::mono(ui, centre + vec2(0.0, 10.0), Align2::CENTER_CENTER,
                    &super::tenths(app.decks[deck].position), 10.5, theme::TEXT_DIM);
    } else {
        ui.painter().text(
            centre,
            Align2::CENTER_CENTER,
            "defalt",
            FontId::proportional(14.0),
            theme::TEXT_MUTE,
        );
    }
}

fn eq_cluster(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    if rect.width() < 70.0 {
        return;
    }
    let mirrored = deck == 1;

    // EQ dropdown across the top, knobs in a column, filter fader beside them
    // on whichever side faces away from the middle.
    let head = Rect::from_min_size(rect.min, vec2(rect.width().min(96.0), 20.0));
    let mut head_ui = super::child(ui, head, super::left_row(), "eqhead");
    head_ui.label(super::rich("Equalizer", 12.0, theme::TEXT_DIM));

    let body = Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 26.0), rect.max);

    // Knobs nearest the jog, then filter, then volume hard against the middle
    // -- so both decks' volume faders sit either side of the crossfader and
    // your hands find all three without looking. Mirrored for deck B.
    let knob_width = 52.0;
    let fader_width = ((body.width() - knob_width) / 2.0).clamp(30.0, 52.0);
    let mut columns = [knob_width, fader_width, fader_width];
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
    column.spacing_mut().item_spacing.y = 1.0;
    for (slot, name) in [(2usize, "HIGH"), (1, "MID"), (0, "LOW")] {
        let mut value = app.decks[deck].tone[slot];
        let id = egui::Id::new(("eq", deck, slot));
        let killed = app.decks[deck].killed[slot];
        // A killed band shows its kill rather than its knob, so the panel
        // never disagrees with what you are hearing.
        let accent = if killed {
            theme::PLAYHEAD
        } else if app.airtime.held.tone[deck][slot] {
            // Yours. Distinct from a kill, which is red, and from the
            // station's blue.
            theme::CYAN
        } else {
            theme::BLUE
        };
        if widgets::knob(&mut column, id, &mut value, false, name, accent).changed() {
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
        if widgets::fader(ui, id, &mut sweep, vec2(width, height), -1.0, 1.0, true).changed() {
            app.take_over(crate::Take::Tone(deck, 3));
            app.decks[deck].tone[3] = sweep;
            app.push_tone(deck);
        }
    });

    labelled_fader(ui, volume, "VOL", |ui, height| {
        let mut gain = app.decks[deck].gain;
        let id = egui::Id::new(("vol", deck));
        let width = volume.width().min(44.0);
        if widgets::fader(ui, id, &mut gain, vec2(width, height), 0.0, 2.0, false).changed() {
            app.take_over(crate::Take::Gain(deck));
            app.decks[deck].gain = gain;
            app.push_gains();
        }
    });
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
    column.add_space(11.0);
    let height = (rect.height() - 18.0).clamp(40.0, 172.0);
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
        waveform::lane(
            ui.painter(),
            lane.shrink2(vec2(0.0, 2.0)),
            state.peaks.as_deref(),
            state.record.as_ref(),
            state.position,
            app.window_seconds(deck),
            theme::DECK_COLOURS[deck],
        );

        let window = app.window_seconds(deck);
        waveform::transition_marks(ui.painter(), lane.shrink2(vec2(0.0, 2.0)),
            &app.airtime.transition_windows(deck), state.position - window / 2.0, state.position + window / 2.0);

        let tag = Rect::from_min_size(lane.min + vec2(6.0, 4.0), vec2(26.0, 22.0));
        ui.painter().rect_filled(tag, 4.0, theme::PANEL);
        super::label(ui, tag.center(), Align2::CENTER_CENTER,
            if deck == 0 { "A" } else { "B" }, 13.0, theme::DECK_COLOURS[deck]);
    }

    // One playhead, dead centre, both lanes: two grids either line up on it
    // or visibly do not.
    let x = inner.center().x;
    ui.painter().line_segment(
        [egui::pos2(x, inner.top()), egui::pos2(x, inner.bottom())],
        Stroke::new(1.6, theme::PLAYHEAD),
    );
    ui.painter().line_segment(
        [egui::pos2(inner.left(), inner.center().y), egui::pos2(inner.right(), inner.center().y)],
        Stroke::new(1.0, egui::Color32::from_black_alpha(170)),
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
    let rect = Rect::from_min_size(inner.right_top() + vec2(-104.0, 4.0), vec2(100.0, 20.0));
    ui.painter().rect_filled(rect, 4.0, theme::PANEL.gamma_multiply(0.85));
    let mut bar = super::child(ui, rect.shrink2(vec2(3.0, 0.0)), super::left_row(), "zoom");
    bar.spacing_mut().item_spacing.x = 3.0;

    if super::glyph_chip(&mut bar, super::Glyph::Minus, vec2(18.0, 18.0), true).clicked() {
        app.bars = (app.bars * 2).min(32);
    }
    let label = format!("{} bar{}", app.bars, if app.bars == 1 { "" } else { "s" });
    bar.label(super::rich(&label, 9.5, theme::TEXT_MUTE));
    if super::glyph_chip(&mut bar, super::Glyph::Plus, vec2(18.0, 18.0), true).clicked() {
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

fn deck_transport(app: &mut Defalt, ui: &mut Ui, deck: usize, rect: Rect) {
    let ready = app.decks[deck].record.is_some();
    let playing = app.decks[deck].playing;
    let layout = if deck == 0 { super::left_row() } else { super::right_row() };
    let mut bar = super::child(ui, rect, layout, if deck == 0 { "tpA" } else { "tpB" });
    bar.spacing_mut().item_spacing.x = 5.0;

    // Play, then cue, then the loop controls, reading outward from the jog.
    if play_button(&mut bar, playing, ready).clicked() {
        app.play_pause(deck);
    }
    if super::chip(&mut bar, "Start", vec2(48.0, 28.0), false, ready).on_hover_text("Return to the start of this track").clicked() {
        app.cue(deck);
    }
    bar.add_space(8.0);
    for slot in 0..4 {
        let set = app.decks[deck].cues[slot].is_some();
        let hit = super::chip(&mut bar, &format!("C{}", slot + 1), vec2(36.0, 28.0), set, ready)
            .on_hover_text("Click to set an empty cue or jump to it. Alt-click to replace it.");
        if hit.clicked() {
            if !set || ui.input(|i| i.modifiers.alt) { app.set_cue(deck, slot); }
            else { app.jump_to_cue(deck, slot); }
        }
    }
}

fn play_button(ui: &mut Ui, playing: bool, live: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(54.0, 32.0), if live { Sense::click() } else { Sense::hover() });
    let fill = if !live {
        theme::PANEL
    } else if playing {
        theme::BLUE_DEEP
    } else if response.hovered() {
        theme::RAISED_HI
    } else {
        theme::RAISED
    };
    ui.painter().rect_filled(rect, 5.0, fill);
    ui.painter().rect_stroke(
        rect,
        5.0,
        Stroke::new(1.0, if playing { theme::BLUE } else { theme::EDGE }),
        egui::StrokeKind::Inside,
    );

    let ink = if live { theme::TEXT } else { theme::TEXT_MUTE };
    let centre = rect.center();
    if playing {
        for dx in [-3.5, 3.5] {
            ui.painter().rect_filled(
                Rect::from_center_size(centre + vec2(dx, 0.0), vec2(3.0, 11.0)),
                1.0,
                theme::TEXT_BRIGHT,
            );
        }
    } else {
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                centre + vec2(-4.0, -6.0),
                centre + vec2(6.0, 0.0),
                centre + vec2(-4.0, 6.0),
            ],
            ink,
            Stroke::NONE,
        ));
    }
    response
}

fn crossfader(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    let mut bar = super::child(ui, rect, super::left_row(), "xf");
    bar.spacing_mut().item_spacing.x = 6.0;

    bar.label(super::rich("A", 10.5, theme::TEXT_MUTE));
    let width = (bar.available_width() - 26.0).max(60.0);
    let mut crossfade = app.crossfade;
    if widgets::crossfader(&mut bar, egui::Id::new("crossfader"), &mut crossfade, vec2(width, 30.0))
        .changed()
    {
        app.take_over(crate::Take::Crossfade);
        app.crossfade = crossfade;
        app.push_gains();
    }
    bar.label(super::rich("B", 10.5, theme::TEXT_MUTE));
}
