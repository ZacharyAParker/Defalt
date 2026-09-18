//! The title bar.
//!
//! The window is undecorated, so this row is also the drag handle and carries
//! its own minimise, maximise and close. Dragging anywhere that is not a
//! control moves the window, which is what every application with a custom
//! chrome has to do by hand.

use egui::{vec2, Align2, FontId, Rect, Sense, Stroke, Ui, ViewportCommand};

use super::{theme, widgets};
use crate::Defalt;

pub fn draw(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();

    ui.painter().line_segment(
        [full.left_bottom(), full.right_bottom()],
        Stroke::new(1.0, theme::EDGE),
    );

    // Left cluster.
    let left_rect = Rect::from_min_max(full.min + vec2(12.0, 6.0), full.center_bottom() - vec2(65.0, 6.0));
    let mut left = super::child(ui, left_rect, super::left_row(), "barL");
    left.spacing_mut().item_spacing.x = 6.0;

    super::chip(&mut left, "Beat grid", vec2(76.0, 28.0), app.show_grid, true)
        .clicked()
        .then(|| app.show_grid = !app.show_grid);
    super::chip(&mut left, "Stems", vec2(62.0, 28.0), app.show_stems, true)
        .clicked()
        .then(|| app.show_stems = !app.show_stems);
    if super::chip(&mut left, "Shortcuts", vec2(78.0, 28.0), app.show_help, true).clicked() {
        app.show_help = !app.show_help;
    }

    // Wordmark, dead centre of the window rather than of the leftover space.
    ui.painter().text(
        full.center(),
        Align2::CENTER_CENTER,
        "DEFALT",
        FontId::proportional(15.0),
        theme::TEXT_BRIGHT,
    );

    // Right cluster.
    let right_rect = Rect::from_min_max(full.center_top() + vec2(65.0, 6.0), full.max - vec2(8.0, 6.0));
    let mut right = super::child(ui, right_rect, super::right_row(), "barR");
    right.spacing_mut().item_spacing.x = 4.0;

    window_button(&mut right, Glyph::Close);
    window_button(&mut right, Glyph::Maximise);
    window_button(&mut right, Glyph::Minimise);

    right.add_space(8.0);
    // The reference has a view picker here; ours has two views and they are
    // both real, so it is two buttons rather than a menu.
    for (view, name) in [
        (crate::View::Radio, "Radio"),
        (crate::View::Console, "Console"),
    ] {
        let on = app.view == view;
        if super::chip(&mut right, name, vec2(68.0, 28.0), on, true).clicked() {
            app.view = view;
        }
    }
    output(app, &mut right);

    right.add_space(6.0);
    right.label(
        super::rich(&app.clock, 11.0, theme::TEXT_DIM),
    );

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

fn icon_slot(ui: &mut Ui, live: bool) -> (Rect, egui::Response) {
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 20.0), Sense::click());
    let hovered = response.hovered() && live;
    if hovered {
        ui.painter().rect_filled(rect, 4.0, theme::RAISED);
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

    // Clipping is worth saying without being asked.
    let peak = app.master_peak[0].max(app.master_peak[1]);
    if peak > 0.99 {
        let dot = egui::pos2(response.rect.right() - 3.0, response.rect.top() + 4.0);
        ui.painter().circle_filled(dot, 3.0, theme::RED);
    }

    egui::Popup::from_toggle_button_response(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(190.0);
            ui.label(super::rich("Master", 11.0, theme::TEXT_DIM));
            ui.add_space(4.0);
            widgets::meter(ui, app.master_peak[0], vec2(178.0, 7.0), false);
            widgets::meter(ui, app.master_peak[1], vec2(178.0, 7.0), false);
            ui.add_space(6.0);
            let mut master = app.master;
            if widgets::crossfader(ui, egui::Id::new("master"), &mut master, vec2(178.0, 26.0)).changed() {
                app.set_master(master);
            }
            ui.label(super::rich(
                &format!("{:.0}%", app.master * 100.0),
                10.0,
                theme::TEXT_MUTE,
            ));
            ui.add_space(4.0);
            ui.label(super::rich(
                if app.device.is_empty() { "no output device" } else { &app.device },
                9.0,
                theme::TEXT_MUTE,
            ));
        });
}

fn speaker_icon(ui: &mut Ui, live: bool) -> egui::Response {
    let (rect, response) = icon_slot(ui, live);
    let centre = rect.center();
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
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 22.0), Sense::click());
    let danger = matches!(glyph, Glyph::Close);

    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            4.0,
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
