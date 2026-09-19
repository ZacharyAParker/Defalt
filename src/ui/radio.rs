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

use super::{theme, widgets};
use crate::station::Health;
use crate::Defalt;

pub fn draw(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();
    let rail = Rect::from_min_size(full.min, vec2(240.0f32.min(full.width() * 0.32), full.height()));

    // The queue only earns a column of its own when there is room for one.
    // Narrower than this it would be a list of truncated titles, which is
    // worse than not showing it, so the on-air panel takes the space instead.
    let queue_width = 300.0f32.min(full.width() * 0.34);
    let has_queue = full.width() - rail.width() > 420.0;
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
        egui::Stroke::new(1.0, theme::EDGE),
    );
    on_air(app, ui, main);
    if has_queue {
        ui.painter().line_segment(
            [queue.left_top(), queue.left_bottom()],
            egui::Stroke::new(1.0, theme::EDGE),
        );
        queue_panel(app, ui, queue);
    }
    mix_settings(app, ui.ctx());
}

fn mix_settings(app: &mut Defalt, ctx: &egui::Context) {
    if !app.mix_settings_open { return; }
    let mut open = true;
    egui::Window::new("Radio mix settings").open(&mut open)
        .default_pos(egui::pos2(260.0, 80.0))
        .default_width(480.0).default_height(610.0).vscroll(true).show(ctx, |ui| {
            ui.label(super::rich("Start with a style, then tune it", 19.0, theme::TEXT));
            let profiles = app.mix_settings["profiles"].clone();
            ui.horizontal_wrapped(|ui| {
                for name in ["Clean radio", "Smooth DJ", "Expressive club"] {
                    if ui.button(name).clicked() {
                        if let Some(fields) = app.mix_settings["fields"].as_array_mut() {
                            for field in fields {
                                let key = field["key"].as_str().unwrap_or("").to_owned();
                                if let Some(value) = profiles[name].get(&key) { field["value"] = value.clone(); }
                            }
                        }
                    }
                }
                if ui.button("Apply").clicked() { apply_mix_settings(app); }
            });
            ui.label(super::rich("Transition changes apply to future pairs. Ducking updates now; voice loudness applies to newly prepared lines.", 12.0, theme::TEXT_DIM));
            ui.label(super::rich("Key lock preserves pitch on console decks. Browser playback changes pitch with tempo.", 12.0, theme::TEXT_DIM));
            ui.separator();
            if let Some(fields) = app.mix_settings["fields"].as_array_mut() {
                for title in ["Song choice and variety", "Timing and beat matching", "Musical timing and playback", "EQ and effects", "Hosts and speech"] {
                    egui::CollapsingHeader::new(title).default_open(false).show(ui, |ui| {
                    if title == "Song choice and variety" {
                        ui.label(super::rich("Connections influence the odds. Requests keep priority, and missing tags or lyrics never exclude a track.", 12.0, theme::TEXT_DIM));
                    }
                    for field in fields.iter_mut().filter(|field| mix_group(field["key"].as_str().unwrap_or("")) == title) {
                    let label = field["label"].as_str().unwrap_or("").replace(" (fraction)", "");
                    let key = field["key"].as_str().unwrap_or("").to_owned();
                    match field["kind"].as_str().unwrap_or("") {
                        "bool" => {
                            let mut value = field["value"].as_bool().unwrap_or(false);
                            if ui.checkbox(&mut value, label).changed() { field["value"] = value.into(); }
                        },
                        "choice" => {
                            let mut value = field["value"].as_str().unwrap_or("auto").to_owned();
                            egui::ComboBox::from_id_salt(key).selected_text(&value).show_ui(ui, |ui| {
                                for option in field["bounds"].as_array().into_iter().flatten() {
                                    if let Some(option) = option.as_str() { ui.selectable_value(&mut value, option.to_owned(), option); }
                                }
                            });
                            ui.label(label);
                            field["value"] = value.into();
                        },
                        kind => {
                            let percent = mix_percent(&key, kind);
                            let scale = if percent { 100.0 } else { 1.0 };
                            let mut value = field["value"].as_f64().unwrap_or(0.0);
                            value *= scale;
                            let min = field["bounds"][0].as_f64().unwrap_or(0.0) * scale;
                            let max = field["bounds"][1].as_f64().unwrap_or(1.0) * scale;
                            let integer = kind == "int";
                            ui.horizontal(|ui| {
                                if ui.add(egui::DragValue::new(&mut value).range(min..=max)
                                    .suffix(if percent { "%" } else { "" })
                                    .speed(if integer { 1.0 } else { (max - min) / 200.0 })
                                    .max_decimals(if integer { 0 } else { 3 })).changed() {
                                    field["value"] = if integer { serde_json::json!(value.round() as i64) } else { serde_json::json!(value / scale) };
                                }
                                ui.label(label);
                            });
                        }
                    }
                    ui.add_space(4.0);
                    }
                    });
                }
            }
            ui.separator();
            if ui.button("Apply settings").clicked() {
                apply_mix_settings(app);
            }
        });
    app.mix_settings_open = open;
}

fn mix_percent(key: &str, kind: &str) -> bool {
    matches!(key, "transitions.tempo_match_limit" | "transitions.tempo_tolerance"
        | "transitions.slam_distance" | "transitions.eq_strength" | "transitions.echo_mix"
        | "transitions.echo_feedback" | "ducking.target_gain"
        | "transitions.minimum_play_fraction" | "transitions.max_entry_skip_fraction"
        | "hosts.song_comment_chance" | "transitions.feedback_max_adjustment")
        || (key.starts_with("selection.compatibility.") && kind == "number"
            && key != "selection.compatibility.energy_step_lufs")
}

fn mix_group(key: &str) -> &'static str {
    if key.starts_with("selection.") { return "Song choice and variety"; }
    if key.starts_with("ducking.") || key.starts_with("tts.") || key.starts_with("hosts.") { return "Hosts and speech"; }
    if key.starts_with("transitions.eq_") || key.starts_with("transitions.echo_")
        || (key.starts_with("transitions.vocal_") && key != "transitions.vocal_collision_weight")
        || matches!(key, "transitions.adaptive_eq_fx" | "transitions.filters_enabled" | "transitions.lpf_floor_hz" | "transitions.hpf_ceiling_hz") {
        return "EQ and effects";
    }
    if key.starts_with("transitions.feedback_")
        || matches!(key, "transitions.overlap_scoring" | "transitions.vocal_collision_weight" | "transitions.energy_dip_weight" | "transitions.bass_collision_weight")
        || matches!(key, "transitions.smart_cues" | "transitions.exit_search_seconds" | "transitions.max_intro_skip" | "transitions.minimum_play_fraction" | "transitions.mid_song_cues" | "transitions.max_entry_skip_fraction" | "transitions.prepare_tracks_ahead" | "skip.lead_in") {
        return "Musical timing and playback";
    }
    "Timing and beat matching"
}

fn apply_mix_settings(app: &mut Defalt) {
    let values: serde_json::Map<String, serde_json::Value> = app.mix_settings["fields"].as_array()
        .into_iter().flatten().filter_map(|field| Some((field["key"].as_str()?.to_owned(), field["value"].clone()))).collect();
    app.airtime.save_mix_settings(serde_json::Value::Object(values));
}

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

/// What is coming, and what you can do to it.
fn queue_panel(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, theme::GROUND);

    let head = Rect::from_min_size(rect.min, vec2(rect.width(), 30.0));
    let running = app.station.running();
    let waiting = app.airtime.queue.iter().filter(|row| row.stage == "queued").count();

    let mut bar = super::child(ui, head.shrink2(vec2(10.0, 5.0)), super::left_row(), "qhead");
    bar.label(RichText::new("Queue").font(FontId::proportional(13.0)).color(theme::TEXT));
    bar.add_space(6.0);
    bar.label(
        RichText::new(format!("{}", app.airtime.queue.len()))
            .font(FontId::monospace(9.0))
            .color(theme::TEXT_MUTE),
    );

    // Clearing is only live when there is something clearable, so the button
    // is never a lie.
    let mut clear = super::child(
        ui,
        Rect::from_min_size(egui::pos2(head.right() - 66.0, head.top() + 5.0), vec2(56.0, 20.0)),
        super::left_row(),
        "qclear",
    );
    if super::chip(&mut clear, "Clear", vec2(56.0, 20.0), false, running && waiting > 0)
        .on_hover_text("Drop what is waiting. Anything you asked for is kept.")
        .clicked()
    {
        app.airtime.queue_clear();
    }

    ui.painter().line_segment(
        [head.left_bottom(), head.right_bottom()],
        egui::Stroke::new(1.0, theme::EDGE),
    );

    let body = Rect::from_min_max(egui::pos2(rect.left(), head.bottom()), rect.max);
    if !running {
        super::label(ui, body.center(), Align2::CENTER_CENTER,
                     "the station is not running", 11.0, theme::TEXT_MUTE);
        return;
    }
    if app.airtime.queue.is_empty() {
        super::label(ui, body.center(), Align2::CENTER_CENTER,
                     "nothing lined up yet", 11.0, theme::TEXT_MUTE);
        return;
    }

    let mut area = super::child(ui, body.shrink2(vec2(8.0, 6.0)),
                                Layout::top_down(Align::Min), "queue");
    let mut action: Option<(String, &'static str)> = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(&mut area, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            let rows = app.airtime.queue.clone();
            let movable = rows.iter().filter(|row| row.stage == "queued").count();
            let mut seen_movable = 0usize;

            for (index, row) in rows.iter().enumerate() {
                let width = ui.available_width();
                let (slot, response) = ui.allocate_exact_size(vec2(width, 56.0), Sense::hover());
                if !row.note.is_empty() { response.on_hover_text(&row.note); }
                else if row.stage == "article" { response.on_hover_text(format!("{}\n{}", row.title, row.artist)); }
                else if !row.selection_reason.is_empty() { response.on_hover_text(&row.selection_reason); }

                // Stage reads as colour: on air, on the clock, waiting, still
                // being found.
                let (tint, edge) = if row.playing {
                    (theme::BLUE_DEEP, theme::BLUE)
                } else {
                    match row.stage.as_str() {
                        "on_deck" => (theme::PANEL, theme::EDGE_LIT),
                        "finding" => (theme::WELL, theme::EDGE),
                        "failed" => (theme::WELL, theme::RED),
                        _ => (theme::PANEL, theme::EDGE),
                    }
                };
                ui.painter().rect_filled(slot, 4.0, tint);
                ui.painter().rect_stroke(slot, 4.0, egui::Stroke::new(1.0, edge),
                                         egui::StrokeKind::Inside);

                let text_at = slot.left_top() + vec2(7.0, 4.0);
                super::clipped_label(ui, Rect::from_min_size(text_at, vec2(slot.width() - 14.0, 20.0)),
                             &row.label(), 13.0, if row.playing { theme::TEXT_BRIGHT } else { theme::TEXT });

                let under = if row.stage == "finding" {
                    "finding...".to_string()
                } else if row.stage == "article" {
                    "news article".to_string()
                } else if row.stage == "failed" {
                    "failed - hover for details".to_string()
                } else {
                    let mut parts = Vec::new();
                    if let Some(bpm) = row.bpm {
                        parts.push(format!("{bpm:.0}"));
                    }
                    if let Some(camelot) = &row.camelot {
                        parts.push(camelot.clone());
                    }
                    parts.join("  ")
                };
                super::mono(ui, text_at + vec2(0.0, 26.0), Align2::LEFT_TOP,
                            &format!("{}  {}", eta(row.eta, row.playing), under), 10.0, theme::TEXT_DIM);


                let mut buttons = super::child(
                    ui,
                    Rect::from_min_size(
                        egui::pos2(slot.right() - 92.0, slot.bottom() - 20.0),
                        vec2(85.0, 17.0),
                    ),
                    super::left_row(),
                    &format!("qa{index}"),
                );
                buttons.spacing_mut().item_spacing.x = 3.0;

                // Only a waiting record can be reordered. One already on the
                // clock cannot: the air times and transitions of everything
                // after it were worked out against where it sits. The station
                // says so per row, and this obeys it rather than guessing.
                if row.stage == "queued" {
                    let first = seen_movable == 0;
                    let last = seen_movable + 1 == movable;
                    seen_movable += 1;
                    let size = vec2(18.0, 17.0);
                    if super::chip(&mut buttons, "^", size, false, row.can_move && !first)
                        .on_hover_text("Move up")
                        .clicked()
                    {
                        action = Some((row.id.clone(), "up"));
                    }
                    if super::chip(&mut buttons, "v", size, false, row.can_move && !last)
                        .on_hover_text("Move down")
                        .clicked()
                    {
                        action = Some((row.id.clone(), "down"));
                    }
                    if super::chip(&mut buttons, "1st", vec2(22.0, 17.0), false,
                                   row.can_move && !first)
                        .on_hover_text("Play this one next")
                        .clicked()
                    {
                        action = Some((row.id.clone(), "next"));
                    }
                }

                let remove_hint = if row.playing {
                    "Too late, this one is playing. Use Skip."
                } else if row.stage == "finding" {
                    "Call it off"
                } else {
                    "Drop it"
                };
                if super::chip(&mut buttons, "x", vec2(18.0, 17.0), false, row.can_remove)
                    .on_hover_text(remove_hint)
                    .clicked()
                {
                    action = Some((row.id.clone(), "remove"));
                }
            }
        });

    if let Some((id, what)) = action {
        app.airtime.queue_action(&id, what);
    }
}

fn controls(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, theme::GROUND);
    let mut column = super::child(ui, rect.shrink2(vec2(10.0, 8.0)),
                                  Layout::top_down(Align::Min), "radiorail");
    egui::ScrollArea::vertical().id_salt("radio_controls_scroll").show(&mut column, |column| {
        controls_content(app, column);
    });
}

fn controls_content(app: &mut Defalt, mut column: &mut Ui) {
    column.spacing_mut().item_spacing.y = 6.0;

    column.label(
        RichText::new("SIDE ROOM")
            .font(FontId::monospace(8.5))
            .color(theme::TEXT_MUTE),
    );

    let (label, colour) = match &app.station.health {
        Health::Off => ("off air", theme::TEXT_MUTE),
        Health::Starting => ("coming up", theme::BLUE),
        Health::Live(_) => ("on air", theme::PLAYHEAD),
        Health::Failed(_) => ("stopped", theme::RED),
    };
    column.label(RichText::new(label).font(FontId::proportional(15.0)).color(colour));

    if let Health::Failed(error) = &app.station.health {
        column.label(
            RichText::new(super::elide(error.as_str(), 40))
                .font(FontId::proportional(9.5))
                .color(theme::RED),
        );
    }

    column.add_space(4.0);
    let running = app.station.running();
    let width = column.available_width();
    if super::chip(&mut column, if running { "Stop" } else { "Go on air" },
                   vec2(width, 32.0), running, !matches!(app.station.health, Health::Starting)).clicked() {
        if running {
            app.stop_radio();
        } else {
            app.start_radio();
        }
    }

    let on = app.airtime.on;
    if super::chip(&mut column, "Mix settings", vec2(width, 26.0), false, matches!(app.station.health, Health::Live(_))).clicked() {
        if let Health::Live(status) = &app.station.health { app.mix_settings = status.mix_config.clone(); }
        app.mix_settings_open = true;
    }
    if super::chip(&mut column, if on { "Playing here" } else { "Play here" },
                   vec2(width, 32.0), on, running && app.engine_ready())
        .on_hover_text("Play the station through this console's own output.")
        .clicked()
    {
        app.set_radio_playback(!on);
    }

    if super::chip(&mut column, "Listen in browser", vec2(width, 20.0), false, running)
        .on_hover_text("The same station on its own page, if you would rather.")
        .clicked()
    {
        let url = app.station.url();
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .spawn();
    }

    if app.airtime.on {
        column.add_space(8.0);

        // What the station has actually got hold of. The decks are the truth
        // here, so this names them rather than inventing a second display.
        for deck in 0..crate::DECKS {
            let name = if deck == 0 { "A" } else { "B" };
            let line = match app.airtime.on_deck(deck) {
                Some(item) => super::elide(&format!("{name}  {}", item.title), 26),
                None => format!("{name}  --"),
            };
            column.label(
                RichText::new(line)
                    .font(FontId::monospace(9.0))
                    .color(if app.airtime.on_deck(deck).is_some() {
                        theme::TEXT_DIM
                    } else {
                        theme::TEXT_MUTE
                    }),
            );
        }

        // A little control of what it is doing. Skip winds forward to just
        // before the next transition rather than cutting the record dead, so
        // you still get the mix, only sooner.
        column.add_space(6.0);
        let has_air = app.airtime.current().is_some();
        if super::chip(&mut column, "Skip", vec2(width, 20.0), false, running)
            .on_hover_text("Jump to the configured lead-in before the planned mix. Waits for the next deck to finish loading.")
            .clicked()
        {
            app.airtime.skip();
        }

        let half = (width - 4.0) / 2.0;
        let (up, down) = column
            .horizontal(|row| {
                row.spacing_mut().item_spacing.x = 4.0;
                let up = super::chip(row, "+", vec2(half, 20.0), false, has_air)
                    .on_hover_text("More like this one.")
                    .clicked();
                let down = super::chip(row, "-", vec2(half, 20.0), false, has_air)
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
                    format!("break in {}:{:02}", seconds / 60, seconds % 60)
                } else {
                    "on the mic".to_string()
                })
                .font(FontId::monospace(8.5))
                .color(theme::CYAN),
            );
            if !brk.title.is_empty() {
                column.label(
                    RichText::new(super::elide(&brk.title, 30))
                        .font(FontId::proportional(9.0))
                        .color(theme::TEXT_MUTE),
                );
            }
        }

        if let Some(next) = app.airtime.coming_up() {
            let seconds = (next.start_at - app.airtime.station_now).max(0.0) as i64;
            column.add_space(4.0);
            column.label(
                RichText::new(super::elide(
                    &format!("next  {}  in {}:{:02}", next.title, seconds / 60, seconds % 60),
                    30,
                ))
                .font(FontId::proportional(9.5))
                .color(theme::TEXT_MUTE),
            );
            if let Some(transition) = &next.transition {
                if !transition.reason.is_empty() {
                    column.label(
                        RichText::new(super::elide(&transition.reason, 30))
                            .font(FontId::proportional(9.0))
                            .color(theme::BLUE),
                    );
                }
            }
        }

        // Anything you have taken is worth saying out loud, because a knob
        // that has quietly stopped following the mix is confusing otherwise.
        if app.airtime.held.any() {
            column.add_space(6.0);
            if super::chip(&mut column, "Back to auto", vec2(width, 20.0), false, true)
                .on_hover_text("Give the controls you have taken back to the station.")
                .clicked()
            {
                app.return_to_auto();
            }
        }

        column.add_space(6.0);
        column.label(
            RichText::new("VOICE").font(FontId::monospace(8.0)).color(theme::TEXT_MUTE),
        );
        widgets::meter(&mut column, app.air_peak, vec2(width.min(120.0), 6.0), false);
    }

    // The one text box. It takes a song, an artist, a genre, a topic for the
    // next break, "do the news", or "play less niko b" -- the station works
    // out which, so this does not have to and must not pretend otherwise.
    column.add_space(10.0);
    let previous_mode = (app.airtime.request_is_vibe, app.airtime.request_is_article);
    column.horizontal_wrapped(|ui| {
        if ui.selectable_label(!app.airtime.request_is_vibe && !app.airtime.request_is_article, "Request").clicked() {
            app.airtime.request_is_vibe = false; app.airtime.request_is_article = false;
        }
        if ui.selectable_label(app.airtime.request_is_vibe, "Set vibe").clicked() {
            app.airtime.request_is_vibe = true; app.airtime.request_is_article = false;
        }
        if ui.selectable_label(app.airtime.request_is_article, "Article").clicked() {
            app.airtime.request_is_vibe = false; app.airtime.request_is_article = true;
        }
    });
    let vibe_mode = app.airtime.request_is_vibe;
    let article_mode = app.airtime.request_is_article;
    if previous_mode != (vibe_mode, article_mode) {
        app.airtime.request_selection = None;
        app.airtime.catalogue.clear();
        if !vibe_mode && !article_mode { app.airtime.catalogue.typed(&app.airtime.request.clone()); }
    }
    let box_width = column.available_width().min(180.0);
    let entry = if article_mode {
        column.label(super::rich("Article link or pasted text", 10.5, theme::TEXT_DIM));
        column.add_enabled(running, egui::TextEdit::multiline(&mut app.airtime.article)
            .hint_text("Paste a news URL or the article itself")
            .char_limit(24000).desired_rows(5).desired_width(box_width)
            .font(FontId::proportional(10.5)))
    } else { column.add_enabled(
        running,
        egui::TextEdit::singleline(&mut app.airtime.request)
            .hint_text(if vibe_mode { "Studying, calm and jazzy" } else { "Song, YouTube link, or topic" })
            .char_limit(240)
            .desired_width(box_width)
            .font(FontId::proportional(10.5)),
    ) };
    let sent = !article_mode && entry.lost_focus() && column.input(|i| i.key_pressed(egui::Key::Enter));
    if entry.changed() {
        app.airtime.request_selection = None;
        if !vibe_mode && !article_mode { app.airtime.catalogue.typed(&app.airtime.request.clone()); }
    }
    let has_text = !(if article_mode { &app.airtime.article } else { &app.airtime.request }).trim().is_empty();
    let pressed = super::chip(&mut column, if article_mode { "Send article" } else if vibe_mode { "Keep this vibe" } else { "Send request" }, vec2(box_width, 24.0), false,
                              running && has_text)
        .on_hover_text(if article_mode { "The director writes a sourced news break. Already planned breaks finish first." } else if vibe_mode { "Guides future picks until changed or cleared. Planned mixes finish first." } else { "Ask for a song, artist, genre, or something to talk about." })
        .clicked();
    if (sent && has_text && running) || pressed {
        app.airtime.submit_request();
        entry.request_focus();
    }
    if article_mode {
        column.label(super::rich("Next unwritten host break. Draft stays here; clear it when done.", 10.0, theme::TEXT_DIM));
        if column.small_button("Clear draft").clicked() { app.airtime.article.clear(); }
    }
    if !vibe_mode && !article_mode {
        if let Some(selected) = &app.airtime.request_selection {
            column.label(super::rich(&format!("Spotify selection · {}", selected.length()), 10.0, theme::CYAN));
        } else if !app.airtime.request.trim().is_empty() {
            if !app.airtime.catalogue.available() {
                column.label(super::rich("Spotify suggestions need credentials. Typed requests still work.", 10.0, theme::TEXT_DIM));
            } else if let Some(error) = &app.airtime.catalogue.error {
                column.label(super::rich(error, 10.0, theme::RED));
            } else if app.airtime.catalogue.busy {
                column.label(super::rich("Searching Spotify…", 10.0, theme::TEXT_DIM));
            }
        }
        if !app.airtime.catalogue.showing.is_empty() {
            column.label(super::rich("Choose a Spotify result", 10.0, theme::TEXT_DIM));
            let mut chosen = None;
            for (index, found) in app.airtime.catalogue.showing.iter().take(5).enumerate() {
                let label = format!("{}\n{} · {}", super::elide(&found.title, 25),
                                    super::elide(&found.artist, 22), found.length());
                if column.add_sized([box_width, 38.0], egui::Button::new(super::rich(&label, 10.5, theme::TEXT)))
                    .on_hover_text(format!("{}\n{} · {}", found.query(), found.album.as_deref().unwrap_or(""),
                                           found.year.as_deref().unwrap_or(""))).clicked() {
                    chosen = Some(index);
                }
            }
            if let Some(found) = chosen.and_then(|i| app.airtime.catalogue.showing.get(i).cloned()) {
                app.airtime.request = found.query();
                app.airtime.request_selection = Some(found);
                app.airtime.catalogue.clear();
            }
        }
    }
    if let Health::Live(status) = &app.station.health {
        if let Some(description) = &status.vibe {
            column.add_space(8.0);
            column.label(super::rich("CURRENT VIBE", 9.0, theme::BLUE));
            column.label(super::rich(&super::elide(description, 80), 11.0, theme::TEXT))
                .on_hover_text(description);
            if column.small_button("Clear vibe").clicked() { app.airtime.clear_vibe(); }
        } else if vibe_mode {
            column.label(super::rich("Tell us the mood or what you are doing. Stays on until cleared.", 11.0, theme::TEXT_DIM));
        }
    }
}

fn on_air(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), 44.0));
    let mut bar = super::child(ui, head.shrink2(vec2(10.0, 5.0)), super::left_row(), "radiohead");
    bar.label(RichText::new("On air").font(FontId::proportional(13.0)).color(theme::TEXT));

    ui.painter().line_segment(
        [head.left_bottom(), head.right_bottom()],
        egui::Stroke::new(1.0, theme::EDGE),
    );

    let body = Rect::from_min_max(egui::pos2(rect.left(), head.bottom()), rect.max);
    let Health::Live(status) = &app.station.health else {
        let starting = matches!(app.station.health, Health::Starting);
        let area = Rect::from_center_size(body.center() - vec2(0.0, 48.0), vec2(body.width().min(440.0) - 32.0, 190.0));
        let mut empty = super::child(ui, area, Layout::top_down(Align::Center), "radio_empty");
        empty.spacing_mut().item_spacing.y = 12.0;
        empty.label(super::rich(if starting { "Bringing Side Room on air" } else { "Your station. Your soundtrack." }, 24.0, theme::TEXT));
        empty.label(super::rich("Music, live mixes and two hosts between records.", 14.0, theme::TEXT_DIM));
        if let Health::Failed(error) = &app.station.health {
            empty.label(super::rich(error, 12.0, theme::RED));
        } else {
            empty.label(super::rich("Start radio to play your loaded decks and keep the music going.", 13.0, theme::TEXT_DIM));
        }
        if starting {
            empty.spinner();
        } else if super::chip(&mut empty, if matches!(app.station.health, Health::Failed(_)) { "Retry station" } else { "Start station" }, vec2(150.0, 36.0), true, true).clicked() {
            app.start_radio();
        }
        return;
    };

    let inner = body.shrink2(vec2(14.0, 10.0));
    super::clipped_label(ui, Rect::from_min_size(inner.min, vec2(inner.width(), 28.0)),
        status.title.as_deref().unwrap_or("Waiting for the first track"), 22.0, theme::TEXT);
    super::label(
        ui,
        inner.left_top() + vec2(0.0, 30.0),
        Align2::LEFT_TOP,
        &super::elide(status.artist.as_deref().unwrap_or(""), 56),
        12.0,
        theme::TEXT_DIM,
    );
    if let Some(note) = &status.note {
        super::label(
            ui,
            inner.left_top() + vec2(0.0, 51.0),
            Align2::LEFT_TOP,
            &super::elide(note, 40),
            10.5,
            theme::TEXT_MUTE,
        );
    }

    // What they have been saying. Newest last, because that is how a
    // conversation reads.
    let lines = Rect::from_min_max(inner.min + vec2(0.0, 82.0), inner.max);
    let mut area = super::child(ui, lines, Layout::top_down(Align::Min), "transcript");
    area.horizontal_wrapped(|ui| {
        ui.label(super::rich("Transcript", 16.0, theme::TEXT));
        if app.music_duck < 0.99 { ui.label(super::rich("Music lowered for speech", 11.0, theme::CYAN)); }
        ui.checkbox(&mut app.transcript_follow, "Follow live");
        if ui.add_enabled(!status.transcript.is_empty(), egui::Button::new("Copy")).clicked() {
            ui.ctx().copy_text(status.transcript.iter().map(|line|
                format!("[{}] {}: {}", super::mmss(line.start_at), line.host, line.text)
            ).collect::<Vec<_>>().join("\n\n"));
        }
    });
    area.add_space(8.0);
    egui::ScrollArea::vertical()
        .stick_to_bottom(app.transcript_follow)
        .auto_shrink([false, false])
        .show(&mut area, |ui| {
            if status.transcript.is_empty() {
                ui.label(
                    RichText::new("Host lines will appear here as they air. You can scroll back or copy the conversation.")
                        .font(FontId::proportional(14.0))
                        .color(theme::TEXT_MUTE),
                );
            }
            for line in &status.transcript {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("{}  {}{}", super::mmss(line.start_at), line.host.to_uppercase(), if line.active { "  • SPEAKING" } else { "" }))
                        .font(FontId::monospace(11.0))
                        .color(if line.active { theme::CYAN } else { theme::BLUE }),
                );
                ui.label(
                    RichText::new(&line.text)
                        .font(FontId::proportional(16.0))
                        .color(if line.active { theme::TEXT_BRIGHT } else { theme::TEXT }),
                );
                if let Some(source) = &line.source {
                    if let Some(url) = &line.source_url {
                        ui.hyperlink_to(format!("Source: {source}"), url);
                    } else {
                        ui.small(format!("Source: {source}"));
                    }
                }
            }
        });

    let _ = ui.allocate_rect(body, Sense::hover());
}
