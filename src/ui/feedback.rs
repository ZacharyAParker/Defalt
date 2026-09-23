//! Report a bug or a suggestion, from anywhere in the window. F8.
//!
//! The report goes to reports/: through the
//! station when it is up, written here by `crate::reports` when it is not.
//! Whatever the console can see goes with it -- the decks, what the radio
//! was doing, the last few things it told you -- and a picture of the window
//! as it was before this dialog covered it.
//!
//! This also keeps the console's log honest: notices, radio notes and
//! station health changes are written to logs/console.log as they happen,
//! so the log a report attaches says what the screen said.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use egui::{Align, Layout, RichText};
use serde_json::{json, Value};

use super::theme;
use crate::reports::{Filed, Report};
use crate::station::Health;
use crate::Defalt;

/// Marks the screenshots this asks for, so the F12 handler leaves them alone.
pub struct Snap;

pub fn is_ours(data: &egui::UserData) -> bool {
    data.data.as_ref().is_some_and(|data| data.is::<Snap>())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bug,
    Suggestion,
}

pub struct Feedback {
    pub open: bool,
    kind: Kind,
    title: String,
    description: String,
    expected: String,
    attach_logs: bool,
    include_screen: bool,
    shot: Option<Arc<egui::ColorImage>>,
    /// Frames spent waiting for the window's picture before opening anyway.
    waiting: Option<u32>,
    sending: bool,
    status: Option<(String, bool)>,
    results: Receiver<Result<Filed, String>>,
    sender: Sender<Result<Filed, String>>,
    focus_title: bool,

    /// What has already been written to the log, so it is written once.
    seen: Seen,
    /// The last few things the console said, for the report's context.
    recent: VecDeque<Value>,
}

#[derive(Default)]
struct Seen {
    started: bool,
    notice: Option<String>,
    note: Option<String>,
    health: String,
    engine_error: Option<String>,
    library_error: Option<String>,
    deck_errors: [Option<String>; 2],
}

impl Default for Feedback {
    fn default() -> Self {
        let (sender, results) = channel();
        Feedback {
            open: false,
            kind: Kind::Bug,
            title: String::new(),
            description: String::new(),
            expected: String::new(),
            attach_logs: true,
            include_screen: true,
            shot: None,
            waiting: None,
            sending: false,
            status: None,
            results,
            sender,
            focus_title: false,
            seen: Seen::default(),
            recent: VecDeque::new(),
        }
    }
}

const RECENT: usize = 30;

impl Feedback {
    /// Ask for a picture of the window, and open once it arrives.
    pub fn begin(&mut self, ctx: &egui::Context) {
        if self.open || self.waiting.is_some() {
            return;
        }
        self.shot = None;
        self.waiting = Some(0);
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(Snap)));
    }

    fn remember(&mut self, text: String) {
        crate::logfile::write("console", &text);
        let at = crate::logfile::stamp(crate::logfile::now_ms(), crate::logfile::offset());
        self.recent.push_back(json!({"at": at, "text": text}));
        while self.recent.len() > RECENT {
            self.recent.pop_front();
        }
    }
}

fn health(station: &Health) -> String {
    match station {
        Health::Off => "off".into(),
        Health::Starting => "starting".into(),
        Health::Live(_) => "live".into(),
        Health::Degraded(_, why) => format!("degraded: {why}"),
        Health::Failed(why) => format!("failed: {why}"),
    }
}

/// Write down whatever changed on screen since last frame.
fn watch(app: &mut Defalt) {
    let mut said = Vec::new();
    let seen = &mut app.feedback.seen;
    if !seen.started {
        seen.started = true;
        said.push(if app.device.is_empty() {
            "audio: no output device".to_string()
        } else {
            format!("audio: {} at {} Hz", app.device, app.sample_rate)
        });
        said.push(format!("library: {} records", app.records.len()));
    }
    let notice = app.notice.as_ref().map(|(text, _)| text.clone());
    if notice.is_some() && notice != seen.notice {
        said.push(format!("notice: {}", notice.clone().unwrap_or_default()));
    }
    seen.notice = notice;
    if app.airtime.note != seen.note {
        if let Some(note) = &app.airtime.note {
            said.push(format!("radio: {note}"));
        }
        seen.note = app.airtime.note.clone();
    }
    let now = health(&app.station.health);
    if now != seen.health {
        said.push(format!("station: {now}"));
        seen.health = now;
    }
    if app.engine_error != seen.engine_error {
        if let Some(error) = &app.engine_error {
            said.push(format!("audio error: {error}"));
        }
        seen.engine_error = app.engine_error.clone();
    }
    if app.library_error != seen.library_error {
        if let Some(error) = &app.library_error {
            said.push(format!("library error: {error}"));
        }
        seen.library_error = app.library_error.clone();
    }
    for deck in 0..2 {
        if app.decks[deck].error != seen.deck_errors[deck] {
            if let Some(error) = &app.decks[deck].error {
                said.push(format!("deck {}: {error}", crate::label(deck)));
            }
            seen.deck_errors[deck] = app.decks[deck].error.clone();
        }
    }
    for line in said {
        app.feedback.remember(line);
    }
}

/// The settings as a flat key: value map, not the whole form description.
fn settings(value: &Value) -> Value {
    match value["fields"].as_array() {
        Some(fields) => Value::Object(fields.iter().filter_map(|field| {
            Some((field["key"].as_str()?.to_string(), field["value"].clone()))
        }).collect()),
        None => Value::Null,
    }
}

/// Everything the console can see that might explain a bug.
pub fn context(app: &Defalt) -> Value {
    let decks: Vec<Value> = app.decks.iter().enumerate().map(|(index, deck)| json!({
        "deck": crate::label(index),
        "record": deck.record.as_ref().map(|r| json!({
            "key": r.key, "title": r.title, "artist": r.artist, "bpm": r.bpm, "camelot": r.camelot,
            "lufs": r.lufs, "duration": r.duration, "file": r.file.display().to_string()})),
        "position": (deck.position * 10.0).round() / 10.0, "length": (deck.length * 10.0).round() / 10.0,
        "playing": deck.playing, "loading": deck.loading, "pitch": deck.pitch, "bend": deck.bend,
        "gain": deck.gain, "trim": deck.trim, "tone": deck.tone, "killed": deck.killed,
        "key_lock": deck.key_lock, "reversed": deck.reversed, "error": deck.error,
        "separated": app.separated[index], "stem_gain": app.stem_gain[index], "stem_muted": app.stem_muted[index],
    })).collect();

    let station = match (&app.station.health, app.station.status()) {
        (health_now, Some(on_air)) => json!({
            "health": health(health_now), "ours": app.station.ours(), "url": app.station.url(),
            "state": on_air.state, "title": on_air.title, "artist": on_air.artist, "note": on_air.note,
            "vibe": on_air.vibe, "track_key": on_air.track_key,
            "position": on_air.position, "duration": on_air.duration,
            "ad": {"note": on_air.ad_note, "busy": on_air.ad_busy, "enabled": on_air.ads_enabled},
            "transcript": on_air.transcript.iter().rev().take(12).rev().map(|line| json!({
                "host": line.host, "text": line.text, "active": line.active})).collect::<Vec<_>>(),
            "mix_settings": settings(&on_air.mix_config),
        }),
        (other, None) => json!({"health": health(other), "ours": app.station.ours(), "url": app.station.url()}),
    };
    let station_output: Vec<&String> = app.station.log.iter().rev().take(40).rev().collect();

    let now = app.airtime.station_now;
    let schedule: Vec<Value> = app.airtime.schedule.iter()
        .filter(|item| item.ends_at() > now).take(12)
        .map(|item| json!({
            "id": item.id, "kind": item.kind, "title": item.title, "artist": item.artist,
            "host": item.host, "segment": item.segment, "starts_in": ((item.start_at - now) * 10.0).round() / 10.0,
            "duration": (item.duration * 10.0).round() / 10.0,
            "transition": item.transition.as_ref().map(|t| json!({"preset": t.preset, "reason": t.reason})),
        })).collect();
    let queue: Vec<Value> = app.airtime.queue.iter().take(20).map(|row| json!({
        "id": row.id, "stage": row.stage, "playing": row.playing, "eta": row.eta, "label": row.label(),
        "note": row.note, "picked_by": row.picked_by, "why": row.selection_reason,
    })).collect();
    let chat = &app.airtime.chat.state;
    let messages: Vec<Value> = chat["messages"].as_array().map(|all| {
        all.iter().rev().take(10).rev()
            .map(|m| json!({"role": m["role"], "text": m["text"]})).collect()
    }).unwrap_or_default();

    json!({
        "view": if app.view == crate::View::Radio { "radio" } else { "console" },
        "audio": {
            "device": app.device, "sample_rate": app.sample_rate, "engine_error": app.engine_error,
            "underruns": app.underruns, "frame_ms": (app.frame_ms * 10.0).round() / 10.0,
            "master": app.master, "crossfade": app.crossfade, "music_duck": app.music_duck,
        },
        "decks": decks,
        "library": {"records": app.records.len(), "error": app.library_error, "pulls": app.pulls.len()},
        "station": station,
        "station_output": station_output,
        "radio": {
            "on_this_output": app.airtime.on, "live": app.airtime.live(), "note": app.airtime.note,
            "station_now": (now * 10.0).round() / 10.0, "schedule": schedule, "queue": queue,
        },
        "director_chat": {"messages": messages, "direction": chat["direction"], "busy": chat["busy"]},
        "mix_settings": settings(&app.mix_settings),
        "recent_notices": app.feedback.recent.iter().collect::<Vec<_>>(),
    })
}

fn send(app: &mut Defalt) {
    let feedback = &mut app.feedback;
    if feedback.sending {
        return;
    }
    if feedback.title.trim().is_empty() && feedback.description.trim().is_empty() {
        feedback.status = Some(("Give it a title or describe what happened.".into(), true));
        return;
    }
    let shot = if feedback.include_screen { feedback.shot.clone() } else { None };
    let mut report = Report {
        kind: if feedback.kind == Kind::Bug { "bug" } else { "suggestion" }.into(),
        title: feedback.title.trim().to_string(),
        description: feedback.description.trim().to_string(),
        expected: if feedback.kind == Kind::Bug { feedback.expected.trim().to_string() } else { String::new() },
        attach_logs: feedback.attach_logs,
        context: Value::Null,
        screenshot_png: None,
    };
    feedback.sending = true;
    feedback.status = Some(("Filing…".into(), false));
    report.context = context(app);
    let station = app.station.ready().then(|| app.station.url());
    let root = app.root.clone();
    let sender = app.feedback.sender.clone();
    std::thread::spawn(move || {
        report.screenshot_png = shot.and_then(|image| crate::reports::png(&image));
        let result = crate::reports::file(&root, station.as_deref(), &report);
        let _ = sender.send(result);
    });
}

/// Once a frame, before the panels: watches, takes F8, and draws the dialog.
pub fn show(app: &mut Defalt, ctx: &egui::Context) {
    watch(app);

    while let Ok(result) = app.feedback.results.try_recv() {
        app.feedback.sending = false;
        match result {
            Ok(filed) => {
                let line = format!("feedback: filed {} ({})", filed.id,
                    if filed.by_station { "through the station" } else { "by the console" });
                app.feedback.remember(line);
                app.say(&format!("Report filed: {}", filed.path));
                let feedback = &mut app.feedback;
                feedback.open = false;
                feedback.title.clear();
                feedback.description.clear();
                feedback.expected.clear();
                feedback.shot = None;
                feedback.status = None;
            }
            Err(error) => {
                app.feedback.remember(format!("feedback: could not file a report: {error}"));
                app.feedback.status = Some((error, true));
            }
        }
    }

    // DEFALT_SHOT_FEEDBACK poses the dialog for the launch screenshot.
    if app.shot_on_launch.is_some() && app.frames == 5 && std::env::var_os("DEFALT_SHOT_FEEDBACK").is_some() {
        let feedback = &mut app.feedback;
        feedback.open = true;
        feedback.title = "Skip stopped the music".into();
        feedback.description = "Pressed skip during a host break. The voice cut off and nothing played for about ten seconds.".into();
        feedback.expected = "The next record starts after the break.".into();
    }

    if ctx.input(|i| i.key_pressed(egui::Key::F8)) {
        if app.feedback.open { app.feedback.open = false; } else { app.feedback.begin(ctx); }
    }

    let shot = ctx.input(|i| i.events.iter().find_map(|event| match event {
        egui::Event::Screenshot { image, user_data, .. } if is_ours(user_data) => Some(image.clone()),
        _ => None,
    }));
    if let Some(waiting) = app.feedback.waiting {
        // A picture of a hidden window never comes; do not wait for it forever.
        if shot.is_some() || waiting > 30 {
            app.feedback.shot = shot;
            app.feedback.waiting = None;
            app.feedback.open = true;
            app.feedback.focus_title = true;
            app.feedback.status = None;
        } else {
            app.feedback.waiting = Some(waiting + 1);
            ctx.request_repaint();
        }
    }

    if !app.feedback.open {
        return;
    }
    crate::keys::release_bends(app);
    draw(app, ctx);

    // Keys typed here are for the dialog, never for the decks underneath.
    ctx.input_mut(|i| {
        i.events.retain(|event| !matches!(event, egui::Event::Key { .. }));
        i.keys_down.clear();
    });
}

fn draw(app: &mut Defalt, ctx: &egui::Context) {
    let bounds = ctx.content_rect();
    let live = app.station.ready();
    let mut close = false;
    let mut submit = false;
    let id = egui::Id::new("feedback");
    let response = egui::Modal::new(id)
        .area(egui::Modal::default_area(id).fade_in(false))
        .frame(egui::Frame::popup(&ctx.style_of(egui::Theme::Dark)).fill(theme::PANEL).inner_margin(20))
        .show(ctx, |ui| {
            let feedback = &mut app.feedback;
            ui.set_width((bounds.width() - 72.0).clamp(280.0, 560.0));
            ui.horizontal(|ui| {
                ui.heading("Report a bug or suggestion");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = ui.button("Cancel").clicked();
                });
            });
            ui.label(RichText::new(if live {
                "Filed to reports/ through the station."
            } else {
                "The station is off, so the console files this to reports/ itself."
            }).size(11.0).color(theme::TEXT_DIM));
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.selectable_value(&mut feedback.kind, Kind::Bug, "Bug");
                ui.selectable_value(&mut feedback.kind, Kind::Suggestion, "Suggestion");
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Title").size(11.0).color(theme::TEXT_DIM));
            let title = ui.add(egui::TextEdit::singleline(&mut feedback.title)
                .hint_text(if feedback.kind == Kind::Bug { "Skip stopped the music" } else { "Show the next record's key" })
                .char_limit(200)
                .desired_width(f32::INFINITY));
            if std::mem::take(&mut feedback.focus_title) {
                title.request_focus();
            }
            ui.add_space(4.0);
            ui.label(RichText::new(if feedback.kind == Kind::Bug { "What happened" } else { "The idea" })
                .size(11.0).color(theme::TEXT_DIM));
            ui.add(egui::TextEdit::multiline(&mut feedback.description)
                .desired_rows(6).char_limit(20_000).desired_width(f32::INFINITY));
            if feedback.kind == Kind::Bug {
                ui.add_space(4.0);
                ui.label(RichText::new("What I expected (optional)").size(11.0).color(theme::TEXT_DIM));
                ui.add(egui::TextEdit::multiline(&mut feedback.expected)
                    .desired_rows(2).char_limit(20_000).desired_width(f32::INFINITY));
            }
            ui.add_space(6.0);
            ui.checkbox(&mut feedback.attach_logs, "Attach logs from the last ten minutes");
            let has_shot = feedback.shot.is_some();
            ui.add_enabled_ui(has_shot, |ui| {
                ui.checkbox(&mut feedback.include_screen, "Include the current screen")
                    .on_disabled_hover_text("No picture of the window was taken.");
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if feedback.sending { "Filing…" } else { "File report" };
                submit = ui.add_enabled(!feedback.sending, egui::Button::new(RichText::new(label).color(theme::TEXT_BRIGHT))
                    .fill(theme::BLUE_DEEP)).clicked();
                ui.label(RichText::new("Ctrl+Enter").size(10.5).color(theme::TEXT_MUTE));
                if let Some((message, error)) = &feedback.status {
                    ui.label(RichText::new(message).size(11.0).color(if *error { theme::RED } else { theme::TEXT_DIM }));
                }
            });
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
        });
    if submit {
        send(app);
    }
    if close || (response.should_close() && !app.feedback.sending) {
        app.feedback.open = false;
    }
}

/// The footer's way in.
pub fn footer_button(app: &mut Defalt, ui: &mut egui::Ui) {
    let button = egui::Button::new(RichText::new("Report a bug").size(11.0)).frame(false);
    if ui.add(button).on_hover_text("Report a bug or suggestion (F8)").clicked() {
        app.feedback.begin(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_dialogs_screenshots_are_claimed() {
        assert!(is_ours(&egui::UserData::new(Snap)));
        assert!(!is_ours(&egui::UserData::default()));
        assert!(!is_ours(&egui::UserData::new(7_u8)));
    }

    #[test]
    fn mix_settings_flatten_to_their_values() {
        let flat = settings(&json!({"profiles": {"x": {}}, "fields": [
            {"key": "crossfade.duration", "value": 6.0, "label": "Crossfade"},
            {"key": "transitions.echo_enabled", "value": true}]}));
        assert_eq!(flat, json!({"crossfade.duration": 6.0, "transitions.echo_enabled": true}));
        assert_eq!(settings(&Value::Null), Value::Null);
    }
}
