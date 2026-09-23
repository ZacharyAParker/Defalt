//! The radio view.
//!
//! Side Room in the console: run it, see what it is doing, hear it.
//!
//! All three now. `Play here` puts the station on the console's own decks --
//! it loads them, plays them, and performs the transitions on the real
//! crossfader and EQ, so the panel shows the mix happening and you can reach
//! in and take any part of it. The browser page is still one button away for
//! when you would rather have it in a tab.

use egui::{vec2, Align, Align2, FontId, Layout, Rect, RichText, Sense, Ui};

use super::{theme, widgets, Look};
use crate::station::Health;
use crate::Defalt;

pub fn draw(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();
    let rail = Rect::from_min_size(full.min, vec2(240.0f32.min(full.width() * 0.32), full.height()));

    // The queue only earns a column of its own when there is room for one,
    // and something to show in it: while the station is off air an empty
    // column is only a box saying so, and the booth has better use for the
    // width. Narrower than this it would be a list of truncated titles.
    let queue_width = 300.0f32.min(full.width() * 0.34);
    let has_queue = full.width() - rail.width() > 420.0
        && (app.station.ready() || !app.airtime.queue.is_empty());
    let queue = Rect::from_min_size(
        egui::pos2(full.right() - queue_width, full.top()),
        vec2(queue_width, full.height()),
    );
    let main = Rect::from_min_max(
        egui::pos2(rail.right(), full.top()),
        egui::pos2(if has_queue { queue.left() } else { full.right() }, full.bottom()),
    );

    controls(app, ui, rail);
    ui.painter().line_segment(
        [rail.right_top(), rail.right_bottom()],
        egui::Stroke::new(theme::LINE, theme::BOOTH_EDGE),
    );

    // The spectrum feeds the booth as well as its own strip, so it is kept
    // up to date whenever either is showing.
    if app.studio.visualizer || app.studio.enabled {
        super::visualizer::update(app);
    }
    let preview = app.airtime.on
        && super::transition_preview::next_pair(&app.airtime.schedule, app.airtime.station_now).is_some();
    if app.studio.enabled {
        // The booth lays out its own column: art, record, transcript, the
        // next mix and the spectrum, one under the other.
        super::studio::draw(app, ui, main, preview);
    } else {
        let spectrum_height = if app.studio.visualizer { (main.height() * 0.13).clamp(84., 148.) } else { 0. };
        let preview_height = if preview { super::transition_preview::HEIGHT + theme::SP_3 } else { 0.0 };
        let content = Rect::from_min_max(main.min, main.max - vec2(0., spectrum_height + preview_height));
        on_air(app, ui, content);
        if preview {
            let strip = Rect::from_min_size(egui::pos2(main.left() + theme::SP_4, content.bottom()),
                                            vec2(main.width() - theme::SP_4 * 2., super::transition_preview::HEIGHT));
            super::transition_preview::draw(ui, strip, &app.airtime.schedule, app.airtime.station_now);
        }
        if app.studio.visualizer {
            let top = content.bottom() + preview_height;
            let spectrum = Rect::from_min_max(egui::pos2(main.left() + theme::SP_4, top), main.max - vec2(theme::SP_4, theme::SP_3));
            super::visualizer::draw(app, ui, spectrum);
        }
    }
    if has_queue {
        ui.painter().line_segment(
            [queue.left_top(), queue.left_bottom()],
            egui::Stroke::new(theme::LINE, theme::BOOTH_EDGE),
        );
        queue_panel(app, ui, queue);
    }
    mix_settings(app, ui.ctx());
    app.airtime.chat.draw(ui.ctx(), app.station.running());
}

/* ── Mix settings ────────────────────────────────────────────────────── */

/// The groups the settings are shown in, in order. A group the station names
/// that is not here is shown after these, under its own name.
const GROUPS: [&str; 5] = [
    "Song choice and variety",
    "Timing and beat matching",
    "Musical timing and playback",
    "EQ and effects",
    "Hosts and speech",
];

/// Open the settings on what the station is running now.
pub fn open_mix_settings(app: &mut Defalt) {
    // What the station last gave in full, now; the whole status is asked
    // for again, since the settings are only on the full one, and the
    // window takes it when it lands.
    if !app.station.mix_config.is_null() {
        app.mix_settings = app.station.mix_config.clone();
    } else if let Some(status) = app.station.status() {
        app.mix_settings = status.mix_config.clone();
    }
    app.station.want_full_status();
    app.mix_settings_open = true;
    app.view_state.mix_dirty = false;
    app.view_state.mix_confirm_close = false;
}

fn mix_settings(app: &mut Defalt, ctx: &egui::Context) {
    if !app.mix_settings_open { return; }
    let mut open = true;
    egui::Window::new("Radio mix settings").open(&mut open)
        .default_pos(egui::pos2(260.0, 80.0))
        .default_width(480.0).default_height(610.0).vscroll(true).show(ctx, |ui| {
            ui.label(super::rich("Start with a style, then tune it", theme::SIZE_XL, theme::TEXT));
            if app.view_state.mix_confirm_close {
                close_prompt(app, ui);
            } else if app.view_state.mix_dirty {
                ui.label(super::rich("Unsaved changes", theme::SIZE_S, theme::WARN));
            }
            profiles_row(app, ui);
            ui.label(super::rich("Transition changes apply to future pairs. Ducking updates now; voice loudness applies to newly prepared lines.", theme::SIZE_S, theme::TEXT_DIM));
            ui.label(super::rich("Key lock preserves pitch on console decks. Browser playback changes pitch with tempo.", theme::SIZE_S, theme::TEXT_DIM));
            ui.separator();
            let mut edited = false;
            if let Some(fields) = app.mix_settings["fields"].as_array_mut() {
                for title in group_order(fields) {
                    egui::CollapsingHeader::new(title.as_str()).default_open(false).show(ui, |ui| {
                        if title == GROUPS[0] {
                            ui.label(super::rich("Connections influence the odds. Requests keep priority, and missing tags or lyrics never exclude a track.", theme::SIZE_S, theme::TEXT_DIM));
                        }
                        for field in fields.iter_mut().filter(|field| mix_group(field) == title) {
                            edited |= field_editor(ui, field);
                            ui.add_space(4.0);
                        }
                    });
                }
            }
            if edited {
                app.view_state.mix_dirty = true;
            }
            ui.separator();
            if ui.button("Apply settings").clicked() {
                apply_mix_settings(app);
            }
        });
    if !open && may_close(&mut app.view_state) {
        app.mix_settings_open = false;
    }
}

/// The settings window's close button was pressed: may it close? Not with
/// edits in it -- those get asked about, inside the window, rather than
/// quietly thrown away.
fn may_close(view: &mut super::ViewState) -> bool {
    if view.mix_dirty {
        view.mix_confirm_close = true;
        return false;
    }
    true
}

/// Apply, discard, or keep editing: asked when the window is closed with
/// edits nobody applied.
fn close_prompt(app: &mut Defalt, ui: &mut Ui) {
    egui::Frame::NONE
        .fill(theme::RAISED)
        .stroke(egui::Stroke::new(1.0, theme::WARN))
        .corner_radius(5.0)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.label(super::rich("These changes have not been applied.", theme::SIZE_M, theme::TEXT));
            ui.horizontal(|ui| {
                if ui.button("Apply and close").clicked() {
                    apply_mix_settings(app);
                    app.mix_settings_open = false;
                }
                if ui.button("Discard").clicked() {
                    app.view_state.mix_dirty = false;
                    app.mix_settings_open = false;
                }
                if ui.button("Keep editing").clicked() {
                    app.view_state.mix_confirm_close = false;
                }
            });
        });
    if !app.mix_settings_open {
        app.view_state.mix_confirm_close = false;
    }
}

/// The three starting styles, and Apply.
fn profiles_row(app: &mut Defalt, ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        for name in ["Clean radio", "Smooth DJ", "Expressive club"] {
            if ui.button(name).clicked() {
                // Only the chosen profile is copied, and only when chosen.
                let profile = app.mix_settings["profiles"][name].clone();
                if let Some(fields) = app.mix_settings["fields"].as_array_mut() {
                    for field in fields {
                        let key = field["key"].as_str().unwrap_or("").to_owned();
                        if let Some(value) = profile.get(&key) { field["value"] = value.clone(); }
                    }
                }
                app.view_state.mix_dirty = true;
            }
        }
        if ui.button("Apply").clicked() { apply_mix_settings(app); }
    });
}

/// One setting's control. Reports whether it was changed.
fn field_editor(ui: &mut Ui, field: &mut serde_json::Value) -> bool {
    let label = field["label"].as_str().unwrap_or("").replace(" (fraction)", "");
    let key = field["key"].as_str().unwrap_or("").to_owned();
    match field["kind"].as_str().unwrap_or("") {
        "bool" => {
            let mut value = field["value"].as_bool().unwrap_or(false);
            let changed = ui.checkbox(&mut value, label).changed();
            if changed { field["value"] = value.into(); }
            changed
        }
        "choice" => {
            let before = field["value"].as_str().unwrap_or("auto").to_owned();
            let mut value = before.clone();
            egui::ComboBox::from_id_salt(&key).selected_text(&value).show_ui(ui, |ui| {
                for option in field["bounds"].as_array().into_iter().flatten() {
                    if let Some(option) = option.as_str() { ui.selectable_value(&mut value, option.to_owned(), option); }
                }
            });
            ui.label(label);
            let changed = value != before;
            if changed { field["value"] = value.into(); }
            changed
        }
        kind => {
            let percent = mix_percent(field, kind);
            let scale = if percent { 100.0 } else { 1.0 };
            let mut value = field["value"].as_f64().unwrap_or(0.0) * scale;
            let min = field["bounds"][0].as_f64().unwrap_or(0.0) * scale;
            let max = field["bounds"][1].as_f64().unwrap_or(1.0) * scale;
            let integer = kind == "int";
            let mut changed = false;
            ui.horizontal(|ui| {
                if ui.add(egui::DragValue::new(&mut value).range(min..=max)
                    .suffix(if percent { "%" } else { "" })
                    .speed(if integer { 1.0 } else { (max - min) / 200.0 })
                    .max_decimals(if integer { 0 } else { 3 })).changed() {
                    field["value"] = if integer { serde_json::json!(value.round() as i64) } else { serde_json::json!(value / scale) };
                    changed = true;
                }
                ui.label(label);
            });
            changed
        }
    }
}

/// Every group in the order it is shown: the known ones first, then any the
/// station has added since.
fn group_order(fields: &[serde_json::Value]) -> Vec<String> {
    let mut order: Vec<String> = GROUPS.iter().map(|g| g.to_string()).collect();
    for field in fields {
        let group = mix_group(field);
        if !order.contains(&group) {
            order.push(group);
        }
    }
    order
}

/// Shown as a percentage: the schema's own `unit` when it gives one, and the
/// console's list of fractional settings when it does not.
fn mix_percent(field: &serde_json::Value, kind: &str) -> bool {
    if let Some(unit) = field["unit"].as_str() {
        return matches!(unit, "percent" | "%" | "fraction");
    }
    let key = field["key"].as_str().unwrap_or("");
    matches!(key, "transitions.tempo_match_limit" | "transitions.tempo_tolerance"
        | "transitions.slam_distance" | "transitions.eq_strength" | "transitions.echo_mix"
        | "transitions.echo_feedback" | "ducking.target_gain"
        | "transitions.minimum_play_fraction" | "transitions.max_entry_skip_fraction"
        | "hosts.song_comment_chance" | "transitions.feedback_max_adjustment")
        || (key.starts_with("selection.compatibility.") && kind == "number"
            && key != "selection.compatibility.energy_step_lufs")
}

/// Which group a setting is shown under: the schema's own `group` when it
/// gives one, and the console's mapping of keys when it does not.
fn mix_group(field: &serde_json::Value) -> String {
    if let Some(group) = field["group"].as_str().filter(|g| !g.trim().is_empty()) {
        return group.to_string();
    }
    let key = field["key"].as_str().unwrap_or("");
    let group = if key.starts_with("selection.") || key == "learning.ignore_skips" {
        GROUPS[0]
    } else if key.starts_with("ducking.") || key.starts_with("tts.") || key.starts_with("hosts.") {
        GROUPS[4]
    } else if key.starts_with("transitions.eq_") || key.starts_with("transitions.echo_")
        || (key.starts_with("transitions.vocal_") && key != "transitions.vocal_collision_weight")
        || matches!(key, "transitions.adaptive_eq_fx" | "transitions.filters_enabled" | "transitions.lpf_floor_hz" | "transitions.hpf_ceiling_hz") {
        GROUPS[3]
    } else if key.starts_with("transitions.feedback_")
        || matches!(key, "transitions.overlap_scoring" | "transitions.vocal_collision_weight" | "transitions.energy_dip_weight" | "transitions.bass_collision_weight")
        || matches!(key, "transitions.smart_cues" | "transitions.exit_search_seconds" | "transitions.max_intro_skip" | "transitions.minimum_play_fraction" | "transitions.mid_song_cues" | "transitions.max_entry_skip_fraction" | "transitions.prepare_tracks_ahead" | "skip.lead_in") {
        GROUPS[2]
    } else {
        GROUPS[1]
    };
    group.to_string()
}

fn apply_mix_settings(app: &mut Defalt) {
    let values: serde_json::Map<String, serde_json::Value> = app.mix_settings["fields"].as_array()
        .into_iter().flatten().filter_map(|field| Some((field["key"].as_str()?.to_owned(), field["value"].clone()))).collect();
    app.airtime.save_mix_settings(serde_json::Value::Object(values));
    app.view_state.mix_dirty = false;
}

/* ── The queue ───────────────────────────────────────────────────────── */

/// mm:ss, or a word where a number would be wrong.
fn eta(seconds: Option<f64>, playing: bool) -> String {
    if playing {
        return "on air".into();
    }
    match seconds {
        Some(left) if left < 1.0 => "next".into(),
        Some(left) => {
            let left = left as i64;
            format!("{}:{:02}", left / 60, left % 60)
        }
        // A waiting record has no air time yet, because that depends on
        // everything in front of it. A dash is honest; a number would not be.
        None => "--".into(),
    }
}

/// The next few things on the station clock, as lines: `+m:ss  what`, and
/// whether it is a record.
///
/// A run of talk segments of one kind -- a news break read in several parts,
/// say -- is one line, not one per part. Stops at `limit`.
fn upcoming(schedule: &[crate::airtime::Scheduled], now: f64, limit: usize) -> Vec<(String, bool)> {
    let mut items: Vec<_> = schedule.iter().filter(|item| item.start_at > now).collect();
    items.sort_by(|a, b| a.start_at.total_cmp(&b.start_at));

    let mut lines = Vec::new();
    // The kind and end of the talk break just listed, to fold its parts.
    let mut previous_break: Option<(&str, f64)> = None;
    for item in items {
        if item.is_music() {
            previous_break = None;
        } else {
            let continues = previous_break
                .is_some_and(|(kind, end)| kind == item.segment && item.start_at - end < 3.0);
            previous_break = Some((&item.segment, item.ends_at()));
            if continues {
                continue;
            }
        }
        let label = if item.is_music() {
            format!("{} · {}", item.title, item.artist)
        } else {
            item.segment.clone()
        };
        lines.push((format!("+{}  {}", super::mmss(item.start_at - now), super::elide(&label, 45)), item.is_music()));
        if lines.len() >= limit {
            break;
        }
    }
    lines
}

/// What is coming, and what you can do to it.
fn queue_panel(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, theme::BOOTH_GROUND);

    let head = Rect::from_min_size(rect.min, vec2(rect.width(), 40.0));
    let running = app.station.running();
    let waiting = app.airtime.queue.iter().filter(|row| row.stage == "queued").count();

    let mut bar = super::child(ui, head.shrink2(vec2(theme::SP_3, 8.0)), super::left_row(), "qhead");
    bar.label(RichText::new("Queue").font(theme::display(theme::SIZE_M)).color(theme::TEXT_BRIGHT));
    bar.add_space(theme::SP_2);
    bar.label(
        RichText::new(format!("{}", app.airtime.queue.len()))
            .font(FontId::monospace(theme::SIZE_XS))
            .color(theme::TEXT_MUTE),
    );

    // Clearing is only live when there is something clearable, so the button
    // is never a lie.
    let mut clear = super::child(
        ui,
        Rect::from_min_size(egui::pos2(head.right() - 72.0, head.top() + 8.0), vec2(60.0, theme::CONTROL_S)),
        super::left_row(),
        "qclear",
    );
    if super::button(&mut clear, "Clear", vec2(60.0, theme::CONTROL_S), Look::ghost(false, running && waiting > 0))
        .on_hover_text("Drop what is waiting. Anything you asked for is kept.")
        .clicked()
    {
        app.airtime.queue_clear();
    }

    ui.painter().line_segment(
        [head.left_bottom(), head.right_bottom()],
        egui::Stroke::new(theme::LINE, theme::BOOTH_EDGE),
    );

    let body = Rect::from_min_max(egui::pos2(rect.left(), head.bottom()), rect.max);
    if !running {
        super::label(ui, body.center(), Align2::CENTER_CENTER,
                     "The station is not running", theme::SIZE_S, theme::TEXT_MUTE);
        return;
    }
    if app.airtime.queue.is_empty() && app.airtime.schedule.is_empty() {
        super::label(ui, body.center(), Align2::CENTER_CENTER,
                     "Nothing lined up yet", theme::SIZE_S, theme::TEXT_MUTE);
        return;
    }

    let mut area = super::child(ui, body.shrink2(vec2(theme::SP_2, theme::SP_2)),
                                Layout::top_down(Align::Min), "queue");
    let mut action: Option<(String, &'static str)> = None;
    let airtime = &app.airtime;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(&mut area, |ui| {
            ui.spacing_mut().item_spacing.y = theme::SP_1;
            ui.label(RichText::new("Coming up").font(theme::display(theme::SIZE_M)).color(theme::TEXT));
            let lines = upcoming(&airtime.schedule, airtime.station_now, 6);
            for (line, music) in &lines {
                ui.label(super::rich(line, theme::SIZE_S, if *music { theme::TEXT_DIM } else { theme::CYAN }));
            }
            if lines.is_empty() {
                ui.small("The next segments are being prepared.");
            }
            ui.add_space(theme::SP_2);
            ui.separator();

            let movable = airtime.queue.iter().filter(|row| row.stage == "queued").count();
            let mut seen_movable = 0usize;
            for (index, row) in airtime.queue.iter().enumerate() {
                let first = seen_movable == 0;
                if row.stage == "queued" {
                    seen_movable += 1;
                }
                let last = seen_movable == movable;
                if let Some(wanted) = queue_row(ui, index, row, first, last) {
                    action = Some((row.id.clone(), wanted));
                }
            }
        });

    if let Some((id, what)) = action {
        app.airtime.queue_action(&id, what);
    }
}

/// One queued record, with the moves it allows. Returns the move asked for.
fn queue_row(ui: &mut Ui, index: usize, row: &crate::airtime::QueueRow, first: bool, last: bool)
    -> Option<&'static str> {
    let width = ui.available_width();
    let (slot, response) = ui.allocate_exact_size(vec2(width, 60.0), Sense::hover());
    // Off screen, the space is kept and nothing is drawn.
    if !ui.is_rect_visible(slot) {
        return None;
    }
    if !row.note.is_empty() { response.on_hover_text(&row.note); }
    else if row.stage == "article" { response.on_hover_text(format!("{}\n{}", row.title, row.artist)); }
    else if !row.selection_reason.is_empty() { response.on_hover_text(&row.selection_reason); }

    // Stage reads as colour: on air, on the clock, waiting, still being found.
    let (tint, edge) = if row.playing {
        (theme::tint(theme::BOOTH_PANEL, theme::AMBER, 0.12), theme::AMBER)
    } else {
        match row.stage.as_str() {
            "on_deck" => (theme::BOOTH_PANEL, theme::EDGE_LIT),
            "finding" => (theme::BOOTH_GROUND, theme::BOOTH_EDGE),
            "failed" => (theme::BOOTH_GROUND, theme::RED),
            _ => (theme::BOOTH_PANEL, theme::BOOTH_EDGE),
        }
    };
    ui.painter().rect_filled(slot, theme::R_M, tint);
    ui.painter().rect_stroke(slot, theme::R_M, egui::Stroke::new(theme::LINE, edge), egui::StrokeKind::Inside);

    let text_at = slot.left_top() + vec2(theme::SP_2, 6.0);
    super::clipped_label(ui, Rect::from_min_size(text_at, vec2(slot.width() - theme::SP_4, 20.0)),
                         &row.label(), theme::SIZE_M, if row.playing { theme::TEXT_BRIGHT } else { theme::TEXT });

    let under = match row.stage.as_str() {
        "finding" => "finding...".to_string(),
        "article" => "news article".to_string(),
        "failed" => "failed - hover for details".to_string(),
        _ => {
            let mut parts = Vec::new();
            match row.picked_by.as_str() {
                "listener" => parts.push("YOU".to_string()),
                "director" => parts.push("AUTO".to_string()),
                _ => {}
            }
            if let Some(bpm) = row.bpm {
                parts.push(format!("{bpm:.0}"));
            }
            if let Some(camelot) = &row.camelot {
                parts.push(camelot.clone());
            }
            parts.join("  ")
        }
    };
    super::mono(ui, text_at + vec2(0.0, 30.0), Align2::LEFT_TOP,
                &format!("{}  {}", eta(row.eta, row.playing), under), theme::SIZE_XS, theme::TEXT_DIM);

    let mut buttons = super::child(
        ui,
        Rect::from_min_size(egui::pos2(slot.right() - 110.0, slot.bottom() - 28.0), vec2(104.0, theme::CONTROL_S)),
        super::right_row(),
        &format!("qa{index}"),
    );
    buttons.spacing_mut().item_spacing.x = 2.0;
    let key = vec2(theme::CONTROL_S, theme::CONTROL_S);
    let mut action = None;

    let remove_hint = if row.playing {
        "Too late, this one is playing. Use Skip."
    } else if row.stage == "finding" {
        "Call it off"
    } else {
        "Drop it"
    };
    if super::glyph_button(&mut buttons, super::Glyph::Cross, key, Look::ghost(false, row.can_remove), "Drop it")
        .on_hover_text(remove_hint).clicked()
    {
        action = Some("remove");
    }

    // Only a waiting record can be reordered. One already on the clock
    // cannot: the air times and transitions of everything after it were
    // worked out against where it sits. The station says so per row, and
    // this obeys it rather than guessing.
    if row.stage == "queued" {
        // Laid out from the right: read left to right they are next, up,
        // down, then the cross.
        if super::glyph_button(&mut buttons, super::Glyph::Down, key, Look::ghost(false, row.can_move && !last), "Move down")
            .on_hover_text("Move down").clicked()
        {
            action = Some("down");
        }
        if super::glyph_button(&mut buttons, super::Glyph::Up, key, Look::ghost(false, row.can_move && !first), "Move up")
            .on_hover_text("Move up").clicked()
        {
            action = Some("up");
        }
        if super::glyph_button(&mut buttons, super::Glyph::First, key, Look::ghost(false, row.can_move && !first), "Play next")
            .on_hover_text("Play this one next").clicked()
        {
            action = Some("next");
        }
    }
    action
}

/* ── The rail ────────────────────────────────────────────────────────── */

fn controls(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, theme::BOOTH_GROUND);
    let mut column = super::child(ui, rect.shrink2(vec2(theme::SP_3, theme::SP_3)),
                                  Layout::top_down(Align::Min), "radiorail");
    egui::ScrollArea::vertical().id_salt("radio_controls_scroll").show(&mut column, |column| {
        column.spacing_mut().item_spacing.y = theme::SP_2 - 2.0;
        station_section(app, column);
        section(column, "Listen");
        listen_section(app, column);
        if app.airtime.on {
            autopilot_section(app, column);
        }
        section(column, "Shape");
        shape_section(app, column);
        request_section(app, column);
        vibe_section(app, column);
        section(column, "Breaks");
        ad_section(app, column);
    });
}

/// A group of the rail's controls starts here: a rule and its name.
fn section(column: &mut Ui, name: &str) {
    column.add_space(theme::SP_3);
    let (rect, _) = column.allocate_exact_size(vec2(column.available_width(), 1.0), Sense::hover());
    column.painter().line_segment([rect.left_center(), rect.right_center()],
                                  egui::Stroke::new(theme::LINE, theme::BOOTH_EDGE));
    column.add_space(theme::SP_1);
    column.label(RichText::new(name).font(theme::display(theme::SIZE_S)).color(theme::TEXT_DIM));
}

/// One of the rail's keys, the full width of the rail and the standard height.
fn rail_key(column: &mut Ui, text: &str, look: Look) -> egui::Response {
    let width = column.available_width();
    super::button(column, text, vec2(width, theme::CONTROL_M), look)
}

/// Whether the station is up, and the buttons that start, stop and route it.
fn station_section(app: &mut Defalt, column: &mut Ui) {
    column.label(
        RichText::new("Side Room")
            .font(theme::display(theme::SIZE_M))
            .color(theme::TEXT_DIM),
    );

    let (label, colour) = match &app.station.health {
        Health::Off => ("Off air", theme::TEXT_MUTE),
        Health::Starting => ("Coming up", theme::BLUE),
        Health::Live(_) => ("On air", theme::AMBER),
        // Still on air: the music plays through a station that is slow to
        // answer, or restarting.
        Health::Degraded(..) => ("On air", theme::WARN),
        Health::Failed(_) => ("Stopped", theme::RED),
    };
    column.label(RichText::new(label).font(theme::display(theme::SIZE_XL)).color(colour));

    match &app.station.health {
        Health::Failed(error) => {
            column.label(
                RichText::new(super::elide(error.as_str(), 40))
                    .font(FontId::proportional(theme::SIZE_XS))
                    .color(theme::RED),
            );
        }
        Health::Degraded(_, why) => {
            column.label(
                RichText::new(super::elide(why.as_str(), 40))
                    .font(FontId::proportional(theme::SIZE_XS))
                    .color(theme::WARN),
            );
        }
        _ => {}
    }

    column.add_space(theme::SP_1);
    if column.checkbox(&mut app.studio.enabled, "Studio view").changed() { app.studio.save(); }
    if column.checkbox(&mut app.studio.visualizer, "Audio visualizer").on_hover_text("A live spectrum of the sound playing here. Saved between launches.").changed() { app.studio.save(); }
    column.add_space(theme::SP_1);
    // The one amber key in the room: going on air. Once the station is up
    // the same place stops it, as an ordinary key.
    let running = app.station.running();
    let width = column.available_width();
    let starting = matches!(app.station.health, Health::Starting);
    let look = if running { Look::secondary(false, !starting) } else { Look::primary(!starting) };
    if super::button(column, if running { "Stop" } else { "Go on air" }, vec2(width, theme::CONTROL_L), look).clicked() {
        if running {
            app.stop_radio();
        } else {
            app.start_radio();
        }
    }
}

/// Where the station is heard.
fn listen_section(app: &mut Defalt, column: &mut Ui) {
    let running = app.station.running();
    let on = app.airtime.on;
    if rail_key(column, if on { "Playing here" } else { "Play here" },
                Look::secondary(on, running && app.engine_ready()).accent(theme::GREEN))
        .on_hover_text("Play the station through this console's own output.")
        .clicked()
    {
        app.set_radio_playback(!on);
    }

    if rail_key(column, "Listen in browser", Look::secondary(false, running))
        .on_hover_text("The same station on its own page, if you would rather.")
        .clicked()
    {
        crate::process::open_url(&app.station.url());
    }
}

/// How the station sounds and what it plays next.
fn shape_section(app: &mut Defalt, column: &mut Ui) {
    if rail_key(column, "Mix settings", Look::secondary(false, app.station.status().is_some())).clicked() {
        open_mix_settings(app);
    }
    if rail_key(column, "Director chat", Look::secondary(false, true)).clicked() {
        app.airtime.chat.open = true;
    }
}

fn ad_section(app: &mut Defalt, column: &mut Ui) {
    let running = app.station.running();
    let width = column.available_width();
    let (ad_busy, ads_enabled, ad_note) = match app.station.status() {
        Some(status) => (status.ad_busy, status.ads_enabled, status.ad_note.clone()),
        None => (false, true, String::new()),
    };
    let live = running && ads_enabled && !ad_busy;
    column.horizontal(|row| {
        row.spacing_mut().item_spacing.x = theme::SP_2 - 2.0;
        let half = (width - theme::SP_2 + 2.0) / 2.0;
        if super::chip(row, "Next host break", vec2(half, theme::CONTROL_M), false, live)
            .on_hover_text("Prepare an ad and add it to the next host break, including one already scheduled.").clicked() {
            app.airtime.request_ad(false);
        }
        if super::chip(row, "Play now", vec2(half, theme::CONTROL_M), false, live)
            .on_hover_text("Play as soon as writing and voices are ready. Music ducks; existing host speech finishes first.").clicked() {
            app.airtime.request_ad(true);
        }
    });
    if !ad_note.is_empty() {
        column.label(super::rich(&ad_note, theme::SIZE_XS, theme::TEXT_DIM));
    } else if !ads_enabled {
        column.label(super::rich("Ads disabled in games.yaml", theme::SIZE_XS, theme::TEXT_DIM));
    }
}

/// While the station has the decks: what it holds, and a little control.
fn autopilot_section(app: &mut Defalt, column: &mut Ui) {
    let running = app.station.running();
    let width = column.available_width();
    column.add_space(8.0);

    // What the station has actually got hold of. The decks are the truth
    // here, so this names them rather than inventing a second display.
    for deck in 0..crate::DECKS {
        let name = if deck == 0 { "A" } else { "B" };
        let held = app.airtime.on_deck(deck);
        let line = match held {
            Some(item) => super::elide(&format!("{name}  {}", item.title), 26),
            None => format!("{name}  --"),
        };
        column.label(
            RichText::new(line)
                .font(FontId::monospace(theme::SIZE_XS))
                .color(if held.is_some() { theme::TEXT_DIM } else { theme::TEXT_MUTE }),
        );
    }

    // Skip winds forward to just before the next transition rather than
    // cutting the record dead, so you still get the mix, only sooner.
    column.add_space(theme::SP_1);
    let has_air = app.airtime.current().is_some();
    if rail_key(column, "Skip", Look::secondary(false, running))
        .on_hover_text("Jump to the configured lead-in before the planned mix. Waits for the next deck to finish loading.")
        .clicked()
    {
        app.airtime.skip();
    }

    let half = (width - theme::SP_2 + 2.0) / 2.0;
    let (up, down) = column
        .horizontal(|row| {
            row.spacing_mut().item_spacing.x = theme::SP_2 - 2.0;
            let size = vec2(half, theme::CONTROL_M);
            let up = super::glyph_button(row, super::Glyph::Plus, size, Look::secondary(false, has_air), "More like this")
                .on_hover_text("More like this one.")
                .clicked();
            let down = super::glyph_button(row, super::Glyph::Minus, size, Look::secondary(false, has_air), "Less like this")
                .on_hover_text("Less like this one, and not again tonight.")
                .clicked();
            (up, down)
        })
        .inner;
    if up {
        app.airtime.rate(true);
    }
    if down {
        app.airtime.rate(false);
    }

    // The breaks are what make this a station rather than a playlist, and
    // they are invisible in the queue, which only lists music. So they get
    // their own line.
    if let Some(brk) = app.airtime.next_break() {
        let seconds = (brk.start_at - app.airtime.station_now).max(0.0) as i64;
        column.add_space(6.0);
        column.label(
            RichText::new(if seconds > 0 {
                format!("Break in {}:{:02}", seconds / 60, seconds % 60)
            } else {
                "On the mic".to_string()
            })
            .font(FontId::monospace(theme::SIZE_XS))
            .color(theme::CYAN),
        );
        if !brk.title.is_empty() {
            column.label(
                RichText::new(super::elide(&brk.title, 30))
                    .font(FontId::proportional(theme::SIZE_XS))
                    .color(theme::TEXT_MUTE),
            );
        }
    }

    if let Some(next) = app.airtime.coming_up() {
        let seconds = (next.start_at - app.airtime.station_now).max(0.0) as i64;
        column.add_space(4.0);
        column.label(
            RichText::new(super::elide(
                &format!("Next  {}  in {}:{:02}", next.title, seconds / 60, seconds % 60),
                30,
            ))
            .font(FontId::proportional(theme::SIZE_XS))
            .color(theme::TEXT_MUTE),
        );
        if let Some(transition) = &next.transition {
            if !transition.reason.is_empty() {
                column.label(
                    RichText::new(super::elide(&transition.reason, 30))
                        .font(FontId::proportional(theme::SIZE_XS))
                        .color(theme::BLUE),
                );
            }
        }
    }

    // Anything you have taken is worth saying out loud, because a knob
    // that has quietly stopped following the mix is confusing otherwise.
    if app.airtime.held.any() {
        column.add_space(theme::SP_1);
        if rail_key(column, "Back to auto", Look::secondary(false, true))
            .on_hover_text("Give the controls you have taken back to the station.")
            .clicked()
        {
            app.return_to_auto();
        }
    }

    column.add_space(theme::SP_1);
    column.label(
        RichText::new("Voice").font(FontId::proportional(theme::SIZE_XS)).color(theme::TEXT_MUTE),
    );
    widgets::meter(column, app.air_peak, vec2(width, 8.0), false);
}

/// The one text box. It takes a song, an artist, a genre, a topic for the
/// next break, "do the news", or "play less niko b" -- the station works
/// out which, so this does not have to and must not pretend otherwise.
fn request_section(app: &mut Defalt, column: &mut Ui) {
    let running = app.station.running();
    column.add_space(theme::SP_1);
    let previous_mode = (app.airtime.request_is_vibe, app.airtime.request_is_article);
    // Three modes of the one box, as a segmented row the width of the rail.
    column.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let third = ((ui.available_width() - 4.0) / 3.0).floor();
        let size = vec2(third, theme::CONTROL_S);
        let song = !app.airtime.request_is_vibe && !app.airtime.request_is_article;
        if super::button(ui, "Request", size, Look::ghost(song, true)).clicked() {
            app.airtime.request_is_vibe = false;
            app.airtime.request_is_article = false;
        }
        if super::button(ui, "Set vibe", size, Look::ghost(app.airtime.request_is_vibe, true)).clicked() {
            app.airtime.request_is_vibe = true;
            app.airtime.request_is_article = false;
        }
        if super::button(ui, "Article", size, Look::ghost(app.airtime.request_is_article, true)).clicked() {
            app.airtime.request_is_vibe = false;
            app.airtime.request_is_article = true;
        }
    });
    let vibe_mode = app.airtime.request_is_vibe;
    let article_mode = app.airtime.request_is_article;
    let song_mode = !vibe_mode && !article_mode;
    if previous_mode != (vibe_mode, article_mode) {
        app.airtime.request_selection = None;
        app.airtime.catalogue.clear();
        if song_mode { app.airtime.catalogue.typed(&app.airtime.request.clone()); }
    }
    let box_width = column.available_width();
    let entry = if article_mode {
        column.label(super::rich("Article link or pasted text", theme::SIZE_XS, theme::TEXT_DIM));
        column.add_enabled(running, egui::TextEdit::multiline(&mut app.airtime.article)
            .hint_text("Paste a news URL or the article itself")
            .char_limit(24000).desired_rows(5).desired_width(box_width)
            .font(FontId::proportional(theme::SIZE_S)))
    } else {
        column.add_enabled(
            running,
            egui::TextEdit::singleline(&mut app.airtime.request)
                .hint_text(if vibe_mode { "Studying, calm and jazzy" } else { "Song, YouTube link, or topic" })
                .char_limit(240)
                .desired_width(box_width)
                .min_size(vec2(box_width, theme::CONTROL_M))
                .vertical_align(Align::Center)
                .font(FontId::proportional(theme::SIZE_S)),
        )
    };
    let sent = !article_mode && entry.lost_focus() && column.input(|i| i.key_pressed(egui::Key::Enter));
    if entry.changed() {
        app.airtime.request_selection = None;
        if song_mode { app.airtime.catalogue.typed(&app.airtime.request.clone()); }
    }
    let has_text = !(if article_mode { &app.airtime.article } else { &app.airtime.request }).trim().is_empty();
    let (button, hint) = if article_mode {
        ("Send article", "The director writes a sourced news break. Already planned breaks finish first.")
    } else if vibe_mode {
        ("Keep this vibe", "Guides future picks until changed or cleared. Planned mixes finish first.")
    } else {
        ("Send request", "Ask for a song, artist, genre, or something to talk about.")
    };
    let pressed = super::chip(column, button, vec2(box_width, theme::CONTROL_M), false, running && has_text)
        .on_hover_text(hint)
        .clicked();
    if (sent && has_text && running) || pressed {
        app.airtime.submit_request();
        entry.request_focus();
    }
    if article_mode {
        column.label(super::rich("Next unwritten host break. Draft stays here; clear it when done.", theme::SIZE_XS, theme::TEXT_DIM));
        if super::button(column, "Clear draft", super::fit(column, "Clear draft", theme::CONTROL_S), Look::ghost(false, true)).clicked() {
            app.airtime.article.clear();
        }
    }
    if song_mode {
        request_suggestions(app, column, box_width);
    }
}

/// Spotify's reading of the request, to pick the exact record from.
fn request_suggestions(app: &mut Defalt, column: &mut Ui, width: f32) {
    if let Some(selected) = &app.airtime.request_selection {
        column.label(super::rich(&format!("Spotify selection · {}", selected.length()), theme::SIZE_XS, theme::CYAN));
    } else {
        super::suggestion_status(column, &app.airtime.catalogue, !app.airtime.request.trim().is_empty());
    }
    if app.airtime.catalogue.showing.is_empty() {
        return;
    }
    column.label(super::rich("Choose a Spotify result", theme::SIZE_XS, theme::TEXT_DIM));
    let chosen = super::suggestion_list(column, &app.airtime.catalogue.showing, width, 5);
    if let Some(found) = chosen.and_then(|i| app.airtime.catalogue.showing.get(i).cloned()) {
        app.airtime.request = found.query();
        app.airtime.request_selection = Some(found);
        app.airtime.catalogue.clear();
    }
}

fn vibe_section(app: &mut Defalt, column: &mut Ui) {
    let Some(status) = app.station.status() else { return };
    if let Some(description) = &status.vibe {
        column.add_space(theme::SP_2);
        column.label(super::rich("Current vibe", theme::SIZE_XS, theme::TEXT_DIM));
        column.label(super::rich(&super::elide(description, 80), theme::SIZE_S, theme::TEXT))
            .on_hover_text(description);
        if super::button(column, "Clear vibe", super::fit(column, "Clear vibe", theme::CONTROL_S), Look::ghost(false, true)).clicked() {
            app.airtime.clear_vibe();
        }
    } else if app.airtime.request_is_vibe {
        column.label(super::rich("Tell us the mood or what you are doing. Stays on until cleared.", theme::SIZE_S, theme::TEXT_DIM));
    }
}

/* ── On air, without the booth ───────────────────────────────────────── */

fn on_air(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), 44.0));
    let mut bar = super::child(ui, head.shrink2(vec2(theme::SP_3, 5.0)), super::left_row(), "radiohead");
    bar.label(RichText::new("On air").font(theme::display(theme::SIZE_L)).color(theme::TEXT_BRIGHT));

    ui.painter().line_segment(
        [head.left_bottom(), head.right_bottom()],
        egui::Stroke::new(theme::LINE, theme::BOOTH_EDGE),
    );

    let body = Rect::from_min_max(egui::pos2(rect.left(), head.bottom()), rect.max);
    let Some(status) = app.station.status() else {
        off_air(app, ui, body);
        return;
    };

    let inner = body.shrink2(vec2(14.0, 10.0));
    super::clipped_label(ui, Rect::from_min_size(inner.min, vec2(inner.width(), 28.0)),
        status.title.as_deref().unwrap_or("Waiting for the first track"), theme::SIZE_XXL, theme::TEXT);
    super::label(
        ui,
        inner.left_top() + vec2(0.0, 30.0),
        Align2::LEFT_TOP,
        &super::elide(status.artist.as_deref().unwrap_or(""), 56),
        theme::SIZE_S,
        theme::TEXT_DIM,
    );
    if let Some(note) = &status.note {
        super::label(
            ui,
            inner.left_top() + vec2(0.0, 51.0),
            Align2::LEFT_TOP,
            &super::elide(note, 40),
            theme::SIZE_XS,
            theme::TEXT_MUTE,
        );
    }

    // What they have been saying. Newest last, because that is how a
    // conversation reads.
    let lines = Rect::from_min_max(inner.min + vec2(0.0, 82.0), inner.max);
    let mut area = super::child(ui, lines, Layout::top_down(Align::Min), "transcript");
    super::transcript_header(&mut area, &status.transcript, &mut app.transcript_follow, app.music_duck < 0.99);
    area.add_space(8.0);
    let style = super::TranscriptStyle { body: theme::SIZE_L, accent: theme::BLUE };
    super::transcript_view(&mut area, "on-air-transcript", &status.transcript, app.transcript_follow, style,
        "Host lines will appear here as they air. You can scroll back or copy the conversation.");

    let _ = ui.allocate_rect(body, Sense::hover());
}

fn off_air(app: &mut Defalt, ui: &mut Ui, body: Rect) {
    let starting = matches!(app.station.health, Health::Starting);
    let area = Rect::from_center_size(body.center() - vec2(0.0, 48.0), vec2(body.width().min(440.0) - 32.0, 190.0));
    let mut empty = super::child(ui, area, Layout::top_down(Align::Center), "radio_empty");
    empty.spacing_mut().item_spacing.y = theme::SP_3;
    empty.label(RichText::new(if starting { "Bringing Side Room on air" } else { "Your station. Your soundtrack." })
        .font(theme::display(theme::SIZE_XXL)).color(theme::TEXT_BRIGHT));
    if let Health::Failed(error) = &app.station.health {
        empty.label(super::rich(error, theme::SIZE_S, theme::RED));
    } else {
        empty.label(super::rich("Music, live mixes and two hosts between records.", theme::SIZE_M, theme::TEXT_DIM));
    }
    if starting {
        empty.spinner();
    } else if super::chip(&mut empty, if matches!(app.station.health, Health::Failed(_)) { "Retry station" } else { "Start station" }, vec2(150.0, theme::CONTROL_M), false, true).clicked() {
        app.start_radio();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_groups_and_units_win_over_the_fallback_mapping() {
        let field = serde_json::json!({"key": "transitions.echo_mix", "kind": "number"});
        assert_eq!(mix_group(&field), "EQ and effects");
        assert!(mix_percent(&field, "number"));
        let field = serde_json::json!({"key": "transitions.echo_mix", "kind": "number",
                                       "group": "Echo and delay", "unit": "seconds"});
        assert_eq!(mix_group(&field), "Echo and delay");
        assert!(!mix_percent(&field, "number"));
        let order = group_order(&[field]);
        assert_eq!(order.last().map(String::as_str), Some("Echo and delay"));
        assert_eq!(&order[..5], GROUPS.map(String::from).as_slice());
    }

    #[test]
    fn a_multi_part_break_is_one_line_in_coming_up() {
        let body = serde_json::json!({"now": 0, "items": [
            {"id": "n1", "kind": "voice", "url": "/1", "start_at": 10, "duration": 5, "meta": {"segment": "news"}},
            {"id": "n2", "kind": "voice", "url": "/2", "start_at": 16, "duration": 5, "meta": {"segment": "news"}},
            {"id": "m", "kind": "music", "url": "/m", "start_at": 22, "duration": 100,
             "meta": {"title": "Song", "artist": "Band"}},
            {"id": "n3", "kind": "voice", "url": "/3", "start_at": 130, "duration": 5, "meta": {"segment": "news"}}
        ]});
        let items = crate::airtime::snapshot_from(&body, 0).items;
        let lines = upcoming(&items, 0.0, 6);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[1].0.contains("Song · Band") && lines[1].1);
        assert_eq!(upcoming(&items, 0.0, 2).len(), 2);
    }

    #[test]
    fn closing_edited_settings_asks_before_dropping_them() {
        let mut view = crate::ui::ViewState::default();
        assert!(may_close(&mut view), "nothing edited, nothing to ask");
        view.mix_dirty = true;
        assert!(!may_close(&mut view));
        assert!(view.mix_confirm_close, "the inline prompt was not raised");
    }
}
