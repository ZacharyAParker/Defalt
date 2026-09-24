//! The title bar.
//!
//! The window is undecorated, so this row is also the drag handle and carries
//! its own minimise, maximise and close. Dragging anywhere that is not a
//! control moves the window, which is what every application with a custom
//! chrome has to do by hand.

use egui::{vec2, Align2, Rect, Sense, Stroke, Ui, ViewportCommand};

use super::{theme, widgets, Look};
use crate::Defalt;

pub fn draw(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();

    ui.painter().line_segment(
        [full.left_bottom(), full.right_bottom()],
        Stroke::new(theme::LINE, theme::EDGE),
    );

    // Left cluster: the panel's own switches, quiet until they are on.
    let left_rect = Rect::from_min_max(full.min + vec2(theme::SP_3, 6.0), full.center_bottom() - vec2(60.0, 6.0));
    let mut left = super::child(ui, left_rect, super::left_row(), "barL");
    left.spacing_mut().item_spacing.x = theme::SP_1;

    let toggle = |ui: &mut Ui, text: &str, on: bool| {
        let size = super::fit(ui, text, theme::CONTROL_S);
        super::button(ui, text, size, Look::ghost(on, true))
    };
    toggle(&mut left, "Beat grid", app.show_grid)
        .clicked()
        .then(|| app.show_grid = !app.show_grid);
    toggle(&mut left, "Stems", app.show_stems)
        .clicked()
        .then(|| app.show_stems = !app.show_stems);
    if toggle(&mut left, "Shortcuts", app.show_help).clicked() {
        app.show_help = !app.show_help;
    }
    toggle(&mut left, "FX", app.show_fx)
        .on_hover_text("A beat echo on each deck")
        .clicked()
        .then(|| app.show_fx = !app.show_fx);
    let three_band = app.view_state.wave_mode == super::WaveMode::ThreeBand;
    if toggle(&mut left, "3-band", three_band)
        .on_hover_text("Colour the waveforms by band: bass blue, mids amber, highs white")
        .clicked()
    {
        app.view_state.wave_mode = if three_band { super::WaveMode::Blend } else { super::WaveMode::ThreeBand };
    }
    if toggle(&mut left, "Quantize", app.quantize)
        .on_hover_text("Cues, cue jumps and loops land on the beat (Ctrl+Q)")
        .clicked()
    {
        app.toggle_quantize();
    }
    if toggle(&mut left, "Lyrics", !app.view_state.hide_lyrics)
        .on_hover_text("The line being sung under each deck's title, when the station has synced lyrics for it")
        .clicked()
    {
        app.view_state.hide_lyrics = !app.view_state.hide_lyrics;
    }

    // Wordmark, dead centre of the window rather than of the leftover space,
    // in the display face and spaced out, as a maker's name on a faceplate.
    let mut job = egui::text::LayoutJob::default();
    job.append("DEFALT", 0.0, egui::TextFormat {
        font_id: theme::display(theme::SIZE_L),
        color: theme::TEXT_BRIGHT,
        extra_letter_spacing: 3.0,
        ..Default::default()
    });
    let wordmark = ui.painter().layout_job(job);
    ui.painter().galley(full.center() - wordmark.size() / 2.0 + vec2(1.5, 0.0), wordmark, theme::TEXT_BRIGHT);

    // Right cluster.
    let right_rect = Rect::from_min_max(full.center_top() + vec2(56.0, 6.0), full.max - vec2(8.0, 6.0));
    let mut right = super::child(ui, right_rect, super::right_row(), "barR");
    right.spacing_mut().item_spacing.x = theme::SP_1;

    window_button(&mut right, Glyph::Close);
    window_button(&mut right, Glyph::Maximise);
    window_button(&mut right, Glyph::Minimise);

    right.add_space(theme::SP_2);
    // The reference has a view picker here; ours has two views and they are
    // both real, so it is two buttons rather than a menu.
    for (view, name) in [
        (crate::View::Radio, "Radio"),
        (crate::View::Console, "Console"),
    ] {
        let on = app.view == view;
        if super::button(&mut right, name, vec2(62.0, theme::CONTROL_S), Look::ghost(on, true)).clicked() {
            app.view = view;
        }
    }
    right.add_space(theme::SP_1);
    output(app, &mut right);
    limiter(app, &mut right);
    super::remote::indicator(app, &mut right);

    // The clock is the first thing to go when the window is narrow.
    let clock = ui.painter().layout_no_wrap(app.clock.clone(), egui::FontId::monospace(theme::SIZE_S), theme::TEXT_DIM);
    if right.available_width() >= clock.size().x + theme::SP_2 {
        right.add_space(theme::SP_1);
        right.label(egui::RichText::new(&app.clock).font(egui::FontId::monospace(theme::SIZE_S)).color(theme::TEXT_DIM));
    }

    // Native dragging a maximized window restores it. Keep this hit target
    // disjoint from both control clusters instead of layering it behind them.
    // A press alone is not a drag, and a held pointer must not restart the
    // native move operation every frame.
    let drag_rect = Rect::from_min_max(
        egui::pos2(left.min_rect().right() + 6.0, full.top()),
        egui::pos2(right.min_rect().left() - 6.0, full.bottom()),
    );
    if drag_rect.is_positive() {
        let drag = ui.interact(drag_rect, ui.id().with("drag"), Sense::click_and_drag());
        if drag.double_clicked_by(egui::PointerButton::Primary) {
            let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        } else if drag.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
    }
}

/* ── Icons ───────────────────────────────────────────────────────────── */

fn icon_slot(ui: &mut Ui, live: bool, width: f32) -> (Rect, egui::Response) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, theme::CONTROL_S), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, live, "Master output"));
    let hovered = (response.hovered() || response.has_focus()) && live;
    if hovered {
        ui.painter().rect_filled(rect, theme::R_M, theme::RAISED);
    }
    (rect, if live { response } else { response.on_hover_text(super::NOT_WIRED) })
}

/// The output icon, and the master level behind it.
///
/// The reference has no master fader on its toolbar, and it needs one
/// somewhere: this is the one place the reference already puts an output
/// control, so it goes here rather than being invented a row of its own.
fn output(app: &mut Defalt, ui: &mut Ui) {
    let live = app.engine_ready();
    let response = speaker_icon(ui, live);

    // The master, always in view: left over right, each with its held peak
    // and its own clip lamp. Clipping is worth saying without being asked.
    let bars = Rect::from_min_max(response.rect.left_top() + vec2(26.0, 6.0),
                                  response.rect.right_bottom() - vec2(4.0, 6.0));
    let half = (bars.height() - 2.0) / 2.0;
    for channel in 0..2 {
        let row = Rect::from_min_size(bars.min + vec2(0.0, channel as f32 * (half + 2.0)), vec2(bars.width(), half));
        widgets::level_meter(ui, row, egui::Id::new(("master-meter", channel)), app.master_peak[channel],
                             app.master_peak[channel] > 1.0, theme::BLUE, false);
    }

    egui::Popup::from_toggle_button_response(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(190.0);
            ui.label(super::rich("Master", theme::SIZE_S, theme::TEXT_DIM));
            ui.add_space(theme::SP_1);
            widgets::meter(ui, app.master_peak[0], vec2(178.0, 8.0), false);
            widgets::meter(ui, app.master_peak[1], vec2(178.0, 8.0), false);
            if over(app) {
                ui.label(super::rich("Over: the master went past full scale", theme::SIZE_XS, theme::RED));
            }
            ui.add_space(6.0);
            // A level, not a crossfader: unity marked, and a double-click
            // goes back to it rather than to the middle of the travel.
            let mut master = app.master;
            let travel = widgets::Travel::level(MASTER_MAX, 1.0);
            if widgets::level(ui, egui::Id::new("master"), &mut master, vec2(178.0, 26.0), travel, "Master level")
                .on_hover_text("Double-click for unity")
                .changed()
            {
                app.set_master(master);
            }
            ui.label(super::rich(&master_label(app.master), theme::SIZE_XS, theme::TEXT_MUTE));
            ui.add_space(4.0);
            ui.label(super::rich(
                if app.device.is_empty() { "no output device" } else { &app.device },
                theme::SIZE_XS,
                theme::TEXT_MUTE,
            ));
        });
}

/// The master went past full scale in the last two seconds. Only possible
/// with the limiter off: the meter reads before the final clamp.
fn over(app: &Defalt) -> bool {
    app.over_at.is_some_and(|at| at.elapsed().as_secs_f32() < 2.0)
}

/// The limiter: how hard it is pulling the master down, and a click to turn
/// it off or on. A limiter that is always working is a mix that is too hot.
fn limiter(app: &mut Defalt, ui: &mut Ui) {
    let live = app.engine_ready();
    let text = if !app.limiter_on {
        "LIM off".to_string()
    } else if app.limiter_db >= 0.1 {
        format!("LIM -{:.1}", app.limiter_db)
    } else {
        "LIM".to_string()
    };
    let working = app.limiter_on && app.limiter_db >= 0.1;
    let (rect, response) = ui.allocate_exact_size(vec2(76.0, theme::CONTROL_S),
                                                  if live { Sense::click() } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, live, app.limiter_on, "Master limiter"));
    let hovered = live && (response.hovered() || response.has_focus());
    let ink = super::paint_control(ui, rect, Look::ghost(false, live), hovered, live && response.is_pointer_button_down_on());
    // The lamp: green when the limiter is standing by, amber while it is
    // pulling the master down, and a warm warning when it is off.
    let lamp = if !app.limiter_on { theme::WARN } else if working { theme::AMBER } else { theme::GREEN };
    let dot = egui::pos2(rect.left() + 11.0, rect.center().y);
    if live && (working || !app.limiter_on) {
        ui.painter().circle_filled(dot, 5.5, lamp.gamma_multiply(0.22));
    }
    ui.painter().circle_filled(dot, 3.0, if live { lamp } else { theme::TEXT_MUTE });
    ui.painter().text(egui::pos2(rect.left() + 20.0, rect.center().y), Align2::LEFT_CENTER, &text,
                      egui::FontId::monospace(theme::SIZE_XS), if working { theme::TEXT_BRIGHT } else { ink });
    let response = super::hint(response, if app.limiter_on {
        "Master limiter: gain reduction in dB. Click to turn it off."
    } else {
        "The limiter is off, so the master can clip. Click to turn it on."
    });
    if response.clicked() {
        app.toggle_limiter();
    }
}

/// A little headroom over unity, and no more: the master is a trim, not a
/// second gain stage.
const MASTER_MAX: f32 = 1.5;

/// The master as you would read it off a desk: decibels from unity.
fn master_label(value: f32) -> String {
    if value <= 0.001 {
        return "-inf dB".into();
    }
    let db = 20.0 * value.log10();
    if db.abs() < 0.05 { "0.0 dB (unity)".into() } else { format!("{db:+.1} dB") }
}

fn speaker_icon(ui: &mut Ui, live: bool) -> egui::Response {
    let (rect, response) = icon_slot(ui, live, 84.0);
    let centre = rect.left_center() + vec2(12.0, 0.0);
    let colour = if live { theme::TEXT_DIM } else { theme::TEXT_MUTE };

    // Cone.
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            centre + vec2(-6.0, -2.5),
            centre + vec2(-2.5, -2.5),
            centre + vec2(1.5, -6.0),
            centre + vec2(1.5, 6.0),
            centre + vec2(-2.5, 2.5),
            centre + vec2(-6.0, 2.5),
        ],
        colour,
        Stroke::NONE,
    ));
    for (radius, width) in [(4.0, 1.2), (6.5, 1.0)] {
        ui.painter().circle_stroke(
            centre + vec2(1.5, 0.0),
            radius,
            Stroke::new(width, colour.gamma_multiply(0.8)),
        );
    }
    response
}

enum Glyph {
    Minimise,
    Maximise,
    Close,
}

fn window_button(ui: &mut Ui, glyph: Glyph) {
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, theme::CONTROL_S), Sense::click());
    let danger = matches!(glyph, Glyph::Close);

    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            theme::R_M,
            if danger { theme::RED.gamma_multiply(0.75) } else { theme::RAISED },
        );
    }
    let ink = if response.hovered() && danger { theme::TEXT_BRIGHT } else { theme::TEXT_DIM };
    let centre = rect.center();
    let stroke = Stroke::new(1.2, ink);

    match glyph {
        Glyph::Minimise => {
            ui.painter().line_segment(
                [centre + vec2(-5.0, 0.0), centre + vec2(5.0, 0.0)],
                stroke,
            );
        }
        Glyph::Maximise => {
            ui.painter().rect_stroke(
                Rect::from_center_size(centre, vec2(9.0, 9.0)),
                1.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        Glyph::Close => {
            ui.painter().line_segment(
                [centre + vec2(-4.5, -4.5), centre + vec2(4.5, 4.5)],
                stroke,
            );
            ui.painter().line_segment(
                [centre + vec2(4.5, -4.5), centre + vec2(-4.5, 4.5)],
                stroke,
            );
        }
    }

    if response.clicked() {
        let ctx = ui.ctx();
        match glyph {
            Glyph::Minimise => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
            Glyph::Maximise => {
                let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
            }
            Glyph::Close => ctx.send_viewport_cmd(ViewportCommand::Close),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, Event, Modifiers, PointerButton, Pos2, RawInput, ViewportId};

    fn frame(ctx: &Context, app: &mut Defalt, time: f64, events: Vec<Event>) -> Vec<ViewportCommand> {
        let mut input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440.0, 900.0))),
            time: Some(time), events, ..Default::default()
        };
        input.viewports.get_mut(&ViewportId::ROOT).unwrap().maximized = Some(true);
        theme::apply(ctx);
        let output = ctx.run_ui(input, |ui| {
            egui::Panel::top("toolbar").exact_size(42.0)
                .frame(egui::Frame::NONE).show(ui, |ui| draw(app, ui));
        });
        let commands = output.viewport_output[&ViewportId::ROOT].commands.clone();
        output.drop_without_applying_deltas();
        commands
    }

    fn button(at: Pos2, pressed: bool) -> Vec<Event> {
        vec![Event::PointerMoved(at), Event::PointerButton {
            pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE,
        }]
    }

    fn app() -> Defalt {
        Defalt::from_root(std::env::temp_dir().join("defalt-no-fixture"), false)
    }

    #[test]
    fn toolbar_controls_never_move_or_restore_the_window_even_on_double_clicks() {
        for (x, name) in [(50.0, "grid"), (123.0, "stems"), (201.0, "help"),
                          (1216.0, "console"), (1288.0, "radio")] {
            let ctx = Context::default();
            let mut app = app();
            if name == "console" { app.view = crate::View::Radio; }
            frame(&ctx, &mut app, 0.0, vec![]);
            for (time, down) in [(0.1, true), (0.15, false), (0.2, true), (0.25, false)] {
                let commands = frame(&ctx, &mut app, time, button(egui::pos2(x, 20.0), down));
                assert!(!commands.iter().any(|c| matches!(c, ViewportCommand::StartDrag | ViewportCommand::Maximized(_))),
                        "{name} sent a window command: {commands:?}");
                if time == 0.15 {
                    assert!(match name {
                        "grid" => app.show_grid, "stems" => app.show_stems, "help" => app.show_help,
                        "console" => app.view == crate::View::Console,
                        _ => app.view == crate::View::Radio,
                    }, "{name} must still activate");
                }
            }
        }
    }

    #[test]
    fn the_master_reads_in_decibels_from_unity() {
        assert_eq!(master_label(1.0), "0.0 dB (unity)");
        assert_eq!(master_label(0.5), "-6.0 dB");
        assert_eq!(master_label(0.0), "-inf dB");
    }

    #[test]
    fn title_drag_requires_movement_and_dispatches_only_once() {
        let ctx = Context::default();
        let mut app = app();
        frame(&ctx, &mut app, 0.0, vec![]);
        let down = frame(&ctx, &mut app, 0.1, button(egui::pos2(720.0, 20.0), true));
        assert!(!down.iter().any(|c| matches!(c, ViewportCommand::StartDrag)));
        let moved = frame(&ctx, &mut app, 0.2, vec![Event::PointerMoved(egui::pos2(745.0, 20.0))]);
        assert_eq!(moved.iter().filter(|c| matches!(c, ViewportCommand::StartDrag)).count(), 1);
        let held = frame(&ctx, &mut app, 0.3, vec![Event::PointerMoved(egui::pos2(748.0, 20.0))]);
        assert!(!held.iter().any(|c| matches!(c, ViewportCommand::StartDrag)));
    }

    #[test]
    fn moving_a_pressed_toolbar_button_does_not_drag_the_window() {
        for x in [50.0, 123.0, 201.0, 1216.0, 1288.0] {
            let ctx = Context::default();
            let mut app = app();
            frame(&ctx, &mut app, 0.0, vec![]);
            let mut commands = frame(&ctx, &mut app, 0.1, button(egui::pos2(x, 20.0), true));
            commands.extend(frame(&ctx, &mut app, 0.2, vec![Event::PointerMoved(egui::pos2(x + 20.0, 20.0))]));
            commands.extend(frame(&ctx, &mut app, 0.3, button(egui::pos2(x + 20.0, 20.0), false)));
            assert!(!commands.iter().any(|c| matches!(c, ViewportCommand::StartDrag | ViewportCommand::Maximized(_))));
        }
    }

    #[test]
    fn double_clicking_blank_title_still_restores_a_maximized_window() {
        let ctx = Context::default();
        let mut app = app();
        frame(&ctx, &mut app, 0.0, vec![]);
        let mut commands = Vec::new();
        for (time, down) in [(0.1, true), (0.15, false), (0.2, true), (0.25, false)] {
            commands.extend(frame(&ctx, &mut app, time, button(egui::pos2(720.0, 20.0), down)));
        }
        assert_eq!(commands.iter().filter(|c| matches!(c, ViewportCommand::Maximized(false))).count(), 1);
        assert!(!commands.iter().any(|c| matches!(c, ViewportCommand::StartDrag)));
    }
}
