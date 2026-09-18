//! The controls. None of these are stock widgets, because none of the things
//! on a mixer are stock widgets.
//!
//! Everything here is drawn from the palette in `theme` and driven by
//! pointer drags rather than clicks, which is how the physical versions work
//! and how muscle memory expects them to behave.

use egui::{
    epaint::PathStroke, vec2, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke,
    Ui, Vec2,
};

use super::theme;

/// Vertical drag distance that covers a control's whole range. Holding shift
/// quarters it, for the corrections you make with a record already playing.
const TRAVEL: f32 = 170.0;

/// A rotary knob with a detent.
///
/// `bipolar` knobs rest at the centre and run -1..1; the rest run 0..1 and
/// rest at 0.5. The arc is drawn from the detent to the mark, so what the eye
/// reads is the departure from flat rather than the absolute position.
pub fn knob(
    ui: &mut Ui,
    id: egui::Id,
    value: &mut f32,
    bipolar: bool,
    label: &str,
    accent: Color32,
) -> Response {
    let diameter = 44.0;
    let (_, rect) = ui.allocate_space(vec2(diameter, diameter + 13.0));
    let mut response = ui.interact(rect, id, Sense::click_and_drag());

    let detent = if bipolar { 0.0 } else { 0.5 };
    let (low, high) = if bipolar { (-1.0, 1.0) } else { (0.0, 1.0) };
    let before = *value;

    if response.dragged() {
        let span = if ui.input(|i| i.modifiers.shift) { TRAVEL * 4.0 } else { TRAVEL };
        let range = high - low;
        *value = (*value - response.drag_delta().y / span * range).clamp(low, high);
    }
    if response.double_clicked() {
        *value = detent;
    }
    if response.contains_pointer() || response.dragged() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            *value = (*value + scroll.signum() * (high - low) * 0.02).clamp(low, high);
        }
    }
    if *value != before {
        response.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let face = Rect::from_center_size(
            rect.center_top() + vec2(0.0, diameter / 2.0),
            Vec2::splat(diameter),
        );
        let centre = face.center();
        let radius = diameter / 2.0 - 3.0;
        let painter = ui.painter();

        // Body. A soft top-left highlight is what makes a circle read as a
        // physical knob rather than a coloured disc.
        painter.circle_filled(centre, radius, theme::KNOB_BODY);
        painter.circle_stroke(centre, radius, Stroke::new(1.2, theme::EDGE_LIT));
        painter.circle_filled(centre - vec2(radius * 0.25, radius * 0.3), radius * 0.62,
                              theme::KNOB_SHEEN);

        // 300 degrees of travel: the gap at the bottom is what tells a hand
        // where the ends are without looking.
        let sweep = 300f32.to_radians();
        let fraction = (*value - low) / (high - low);
        let detent_fraction = (detent - low) / (high - low);
        let angle_of = |f: f32| -sweep / 2.0 + f * sweep;

        arc(ui, centre, radius + 3.5, angle_of(detent_fraction), angle_of(fraction),
            Stroke::new(2.6, accent));

        let mark = angle_of(fraction);
        let direction = vec2(mark.sin(), -mark.cos());
        painter.line_segment(
            [centre + direction * (radius * 0.42), centre + direction * (radius * 0.86)],
            Stroke::new(2.2, theme::TEXT),
        );

        let moved = (fraction - detent_fraction).abs() > 0.005;
        painter.text(
            rect.center_bottom() - vec2(0.0, 1.0),
            Align2::CENTER_BOTTOM,
            label,
            FontId::monospace(8.5),
            if moved { accent } else { theme::TEXT_MUTE },
        );
    }

    response
}

/// Arc from one angle to another, measured clockwise from twelve o'clock.
fn arc(ui: &Ui, centre: Pos2, radius: f32, from: f32, to: f32, stroke: Stroke) {
    if (to - from).abs() < 0.02 {
        return;
    }
    let steps = ((to - from).abs() / 0.12).ceil().max(2.0) as usize;
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let angle = from + (to - from) * (i as f32 / steps as f32);
            centre + vec2(angle.sin(), -angle.cos()) * radius
        })
        .collect();
    ui.painter().add(egui::Shape::line(points, PathStroke::new(stroke.width, stroke.color)));
}

/// A vertical fader. `bipolar` centres the detent and draws a centre notch.
pub fn fader(
    ui: &mut Ui,
    id: egui::Id,
    value: &mut f32,
    size: Vec2,
    low: f32,
    high: f32,
    bipolar: bool,
) -> Response {
    let (_, rect) = ui.allocate_space(size);
    let mut response = ui.interact(rect, id, Sense::click_and_drag());

    let travel = rect.height() - 20.0;
    let before = *value;
    if response.dragged() {
        let span = if ui.input(|i| i.modifiers.shift) { travel * 4.0 } else { travel };
        *value = (*value - response.drag_delta().y / span * (high - low)).clamp(low, high);
    }
    if response.double_clicked() {
        *value = if bipolar { (low + high) / 2.0 } else { high };
    }
    if *value != before {
        response.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let slot = Rect::from_center_size(rect.center(), vec2(5.0, travel + 6.0));
        painter.rect_filled(slot, 2.5, theme::SLOT);
        painter.rect_stroke(slot, 2.5, Stroke::new(1.0, theme::EDGE), egui::StrokeKind::Inside);

        if bipolar {
            painter.line_segment(
                [rect.center() - vec2(9.0, 0.0), rect.center() + vec2(9.0, 0.0)],
                Stroke::new(1.0, theme::EDGE_LIT),
            );
        }

        let fraction = (*value - low) / (high - low);
        let y = rect.bottom() - 10.0 - fraction * travel;
        let cap = Rect::from_center_size(egui::pos2(rect.center().x, y), vec2(26.0, 14.0));
        painter.rect_filled(cap, 3.0, theme::CAP);
        painter.rect_stroke(cap, 3.0, Stroke::new(1.0, Color32::from_black_alpha(170)),
                            egui::StrokeKind::Inside);
        painter.line_segment(
            [cap.left_center() + vec2(3.0, 0.0), cap.right_center() - vec2(3.0, 0.0)],
            Stroke::new(1.0, theme::CAP_LINE),
        );
    }

    response
}

/// A level meter. `peak` is linear, not decibels; the scale below is.
pub fn meter(ui: &mut Ui, peak: f32, size: Vec2, vertical: bool) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::WELL);
    painter.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::EDGE), egui::StrokeKind::Inside);

    // Linear metering wastes most of the strip on the loudest few dB, which
    // is the part you already know about. This is -60dB to 0 across the run.
    let db = 20.0 * peak.max(1e-4).log10();
    let filled = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    if filled <= 0.0 {
        return;
    }

    let inner = rect.shrink(1.5);
    // Segments rather than a gradient: a real meter is a column of lamps, and
    // you read the count without reading the height.
    let segments = if vertical { 22 } else { 28 };
    for i in 0..segments {
        let at = (i as f32 + 0.5) / segments as f32;
        if at > filled {
            break;
        }
        let colour = if at > 0.94 { theme::RED }
                     else if at > 0.74 { theme::BLUE }
                     else { theme::CYAN };
        let cell = if vertical {
            let h = inner.height() / segments as f32;
            Rect::from_min_size(
                egui::pos2(inner.left(), inner.bottom() - (i as f32 + 1.0) * h),
                vec2(inner.width(), h - 1.0),
            )
        } else {
            let w = inner.width() / segments as f32;
            Rect::from_min_size(
                egui::pos2(inner.left() + i as f32 * w, inner.top()),
                vec2(w - 1.0, inner.height()),
            )
        };
        painter.rect_filled(cell, 0.5, colour);
    }
}

pub struct PlatterOut {
    pub response: Response,
    /// Radians the hand moved this frame, seam-corrected.
    pub turned: f32,
}

/// The platter. Turns because the record is moving, not because a timer is
/// running -- a stopped deck has to look stopped.
pub fn platter(
    ui: &mut Ui,
    id: egui::Id,
    diameter: f32,
    spin: f32,
    progress: f32,
    loaded: bool,
) -> PlatterOut {
    let (_, rect) = ui.allocate_space(Vec2::splat(diameter));
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let centre = rect.center();

    let mut turned = 0.0;
    if response.dragged() && loaded {
        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
            let now = (pointer - centre).angle();
            let before = (pointer - response.drag_delta() - centre).angle();
            let mut delta = now - before;
            // Crossing the seam at pi must not read as most of a turn back.
            if delta > std::f32::consts::PI { delta -= std::f32::consts::TAU; }
            if delta < -std::f32::consts::PI { delta += std::f32::consts::TAU; }
            turned = delta;
        }
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = diameter / 2.0 - 2.0;

        painter.circle_filled(centre, radius, theme::PLATTER_EDGE);
        painter.circle_filled(centre, radius - 3.0, theme::PLATTER_BODY);
        painter.circle_filled(centre - vec2(radius * 0.22, radius * 0.28), radius * 0.7,
                              theme::PLATTER_SHEEN);
        painter.circle_stroke(centre, radius, Stroke::new(1.4, theme::EDGE_LIT));

        // Strobe dots, turning with the record.
        let dots = 44;
        for i in 0..dots {
            let angle = spin + (i as f32 / dots as f32) * std::f32::consts::TAU;
            let at = centre + vec2(angle.cos(), angle.sin()) * (radius - 9.0);
            painter.circle_filled(at, 1.9,
                if i % 4 == 0 { theme::STROBE_LIT } else { theme::STROBE });
        }

        if loaded {
            arc(ui, centre, radius - 20.0, 0.0,
                progress.clamp(0.0, 1.0) * std::f32::consts::TAU,
                Stroke::new(3.5, theme::BLUE));
        }

        // Spindle mark, so a slow turn still reads as turning. Only once a
        // record is on: the wordmark has the middle until then.
        if loaded {
            let mark = vec2(spin.cos(), spin.sin());
            painter.line_segment(
                [centre + mark * (radius * 0.36), centre + mark * (radius - 26.0)],
                Stroke::new(2.5, theme::PLATTER_MARK),
            );
            painter.circle_filled(centre, 4.0, theme::EDGE_LIT);
        }
    }

    PlatterOut { response, turned }
}

/// The crossfader: a long slot with tick marks, cap in the middle.
///
/// Ticks rather than a plain track because the middle has to be findable
/// without looking, and a centre notch alone is not enough on a fader this
/// wide.
pub fn crossfader(ui: &mut Ui, id: egui::Id, value: &mut f32, size: Vec2) -> Response {
    let (_, rect) = ui.allocate_space(size);
    let mut response = ui.interact(rect, id, Sense::click_and_drag());

    let travel = rect.width() - 26.0;
    let before = *value;
    if response.dragged() {
        *value = (*value + response.drag_delta().x / travel).clamp(0.0, 1.0);
    }
    if response.double_clicked() {
        *value = 0.5;
    }
    if *value != before {
        response.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let slot = Rect::from_center_size(rect.center(), vec2(travel + 10.0, 6.0));
        painter.rect_filled(slot, 3.0, theme::SLOT);
        painter.rect_stroke(slot, 3.0, Stroke::new(1.0, theme::EDGE), egui::StrokeKind::Inside);

        let ticks = 25;
        for i in 0..=ticks {
            let at = i as f32 / ticks as f32;
            let x = slot.left() + 5.0 + at * travel;
            let middle = i == ticks / 2;
            let height = if middle { 11.0 } else { 6.0 };
            painter.line_segment(
                [
                    egui::pos2(x, rect.center().y - height / 2.0 - 7.0),
                    egui::pos2(x, rect.center().y + height / 2.0 - 7.0),
                ],
                Stroke::new(
                    if middle { 1.4 } else { 1.0 },
                    if middle { theme::TEXT_MUTE } else { theme::EDGE_LIT },
                ),
            );
        }

        let x = slot.left() + 5.0 + *value * travel;
        let cap = Rect::from_center_size(egui::pos2(x, rect.center().y), vec2(16.0, 22.0));
        painter.rect_filled(cap, 3.0, theme::CAP);
        painter.rect_stroke(cap, 3.0, Stroke::new(1.0, Color32::from_black_alpha(180)),
                            egui::StrokeKind::Inside);
        painter.line_segment(
            [cap.center_top() + vec2(0.0, 3.0), cap.center_bottom() - vec2(0.0, 3.0)],
            Stroke::new(1.0, theme::CAP_LINE),
        );
    }

    response
}

/* â”€â”€ Do the controls actually move? â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
   These drive a real pointer through a headless egui, because the bug they
   guard against was invisible in every other way: the widgets worked, they
   simply took their identity from the parent's child counter, so hiding a
   band renumbered them and a drag landed on a different control. Nothing
   short of an actual drag catches that. */
#[cfg(test)]
mod interaction {
    use super::*;
    use egui::{Context, Event, Modifiers, PointerButton, Pos2, RawInput};

    /// Run one frame, handing the widget a pointer state.
    fn frame(ctx: &Context, events: Vec<Event>, mut build: impl FnMut(&mut Ui)) {
        let input = RawInput { events, ..Default::default() };
        let output = ctx.run_ui(input, |ui| build(ui));
        output.drop_without_applying_deltas();
    }

    fn press(at: Pos2) -> Vec<Event> {
        vec![
            Event::PointerMoved(at),
            Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]
    }

    /// Press inside a control, drag by `delta`, and report where it ended up.
    fn drag(mut build: impl FnMut(&mut Ui, &mut f32), start: f32, delta: egui::Vec2) -> f32 {
        let ctx = Context::default();
        let mut value = start;
        let at = Pos2::new(20.0, 20.0);

        // Three passes: egui needs one to lay the widget out, one to take the
        // press, and one to see the movement.
        frame(&ctx, vec![], |ui| build(ui, &mut value));
        frame(&ctx, press(at), |ui| build(ui, &mut value));
        frame(&ctx, vec![Event::PointerMoved(at + delta)], |ui| {
            build(ui, &mut value)
        });
        value
    }

    #[test]
    fn a_knob_turns_up_when_dragged_up() {
        let id = egui::Id::new("knob-under-test");
        let after = drag(
            |ui, value| {
                knob(ui, id, value, false, "TEST", Color32::WHITE);
            },
            0.5,
            egui::vec2(0.0, -40.0),
        );
        assert!(after > 0.55, "the knob did not turn: {after}");
    }

    #[test]
    fn a_knob_turns_down_when_dragged_down() {
        let id = egui::Id::new("knob-down");
        let after = drag(
            |ui, value| {
                knob(ui, id, value, false, "TEST", Color32::WHITE);
            },
            0.5,
            egui::vec2(0.0, 40.0),
        );
        assert!(after < 0.45, "the knob did not turn: {after}");
    }

    #[test]
    fn two_knobs_do_not_share_a_drag() {
        // The actual bug. Two controls drawn in the same frame, one dragged:
        // the other must not move, whatever order they were built in.
        let ctx = Context::default();
        let (mut first, mut second) = (0.5f32, 0.5f32);
        let at = Pos2::new(20.0, 20.0);

        let build = |ui: &mut Ui, a: &mut f32, b: &mut f32| {
            knob(ui, egui::Id::new("first"), a, false, "A", Color32::WHITE);
            knob(ui, egui::Id::new("second"), b, false, "B", Color32::WHITE);
        };

        frame(&ctx, vec![], |ui| build(ui, &mut first, &mut second));
        frame(&ctx, press(at), |ui| build(ui, &mut first, &mut second));
        frame(&ctx, vec![Event::PointerMoved(at + egui::vec2(0.0, -40.0))], |ui| {
            build(ui, &mut first, &mut second)
        });

        assert!(first > 0.55, "the dragged knob did not move: {first}");
        assert_eq!(second, 0.5, "the other knob moved too: {second}");
    }

    #[test]
    fn a_fader_follows_the_pointer() {
        let id = egui::Id::new("fader-under-test");
        let after = drag(
            |ui, value| {
                fader(ui, id, value, egui::vec2(40.0, 120.0), 0.0, 1.0, false);
            },
            0.5,
            egui::vec2(0.0, -50.0),
        );
        assert!(after > 0.7, "the fader did not move: {after}");
    }

    #[test]
    fn a_crossfader_follows_the_pointer() {
        let id = egui::Id::new("xf-under-test");
        let after = drag(
            |ui, value| {
                crossfader(ui, id, value, egui::vec2(200.0, 30.0));
            },
            0.5,
            egui::vec2(60.0, 0.0),
        );
        assert!(after > 0.7, "the crossfader did not move: {after}");
    }

    /// The exact nesting the pitch column uses: a panel, a child laid out
    /// top-down and centred, a chip above and a fader below. The chip works
    /// in the running application and the fader does not, and both live here.
    /* Every control in the panel is read out of the engine each frame into a
       local, handed to the widget, and written back only when the response
       says it changed. A widget that moves its local but never reports the
       change is a control that snaps back to the engine's value on the next
       frame -- which is exactly what a dead dial looks like from a chair.
       These tests hold the widgets to the contract their callers use. */

    /// Drive a real drag and report what the *caller* would have seen: the
    /// value, and whether the response said so.
    fn drag_reporting(
        mut build: impl FnMut(&mut Ui, &mut f32) -> Response,
        start: f32,
        delta: egui::Vec2,
    ) -> (f32, bool) {
        let ctx = Context::default();
        let mut value = start;
        let mut changed = false;
        let at = Pos2::new(20.0, 20.0);

        frame(&ctx, vec![], |ui| { build(ui, &mut value); });
        frame(&ctx, press(at), |ui| { build(ui, &mut value); });
        frame(&ctx, vec![Event::PointerMoved(at + delta)], |ui| {
            changed = build(ui, &mut value).changed();
        });
        (value, changed)
    }

    #[test]
    fn a_knob_reports_that_it_turned() {
        let id = egui::Id::new("knob-reports");
        let (value, changed) = drag_reporting(
            |ui, value| knob(ui, id, value, false, "TEST", Color32::WHITE),
            0.5,
            egui::vec2(0.0, -40.0),
        );
        assert!(value > 0.55, "the knob did not turn: {value}");
        assert!(changed, "the knob turned but did not say so, so the panel discards it");
    }

    #[test]
    fn a_fader_reports_that_it_moved() {
        let id = egui::Id::new("fader-reports");
        let (value, changed) = drag_reporting(
            |ui, value| fader(ui, id, value, egui::vec2(40.0, 120.0), 0.0, 1.0, false),
            0.5,
            egui::vec2(0.0, -50.0),
        );
        assert!(value > 0.7, "the fader did not move: {value}");
        assert!(changed, "the fader moved but did not say so, so the panel discards it");
    }

    #[test]
    fn a_crossfader_reports_that_it_moved() {
        let (value, changed) = drag_reporting(
            |ui, value| crossfader(ui, egui::Id::new("xf-reports"), value, egui::vec2(200.0, 30.0)),
            0.5,
            egui::vec2(60.0, 0.0),
        );
        assert!(value > 0.7, "the crossfader did not move: {value}");
        assert!(changed, "the crossfader moved but did not say so");
    }

    #[test]
    fn a_control_that_is_only_hovered_reports_nothing() {
        // The other half of the contract: a change reported every frame would
        // have the panel writing to the engine forever.
        let ctx = Context::default();
        let mut value = 0.5f32;
        let mut changed = true;
        let build = |ui: &mut Ui, value: &mut f32| {
            knob(ui, egui::Id::new("quiet"), value, false, "TEST", Color32::WHITE)
        };
        frame(&ctx, vec![], |ui| { build(ui, &mut value); });
        frame(&ctx, vec![Event::PointerMoved(Pos2::new(20.0, 20.0))], |ui| {
            changed = build(ui, &mut value).changed();
        });
        assert!(!changed, "a knob nobody touched reported a change");
    }

    /// The whole failure, end to end: a panel that only writes back on
    /// `changed()` must still see the dial move.
    #[test]
    fn a_dial_read_back_from_state_each_frame_still_moves() {
        let ctx = Context::default();
        // Stands in for the engine: the widget never owns this.
        let mut state = 0.5f32;
        let at = Pos2::new(20.0, 20.0);

        let build = |ui: &mut Ui, state: &mut f32| {
            let mut local = *state;
            if knob(ui, egui::Id::new("statebacked"), &mut local, false, "TEST", Color32::WHITE)
                .changed()
            {
                *state = local;
            }
        };

        frame(&ctx, vec![], |ui| build(ui, &mut state));
        frame(&ctx, press(at), |ui| build(ui, &mut state));
        frame(&ctx, vec![Event::PointerMoved(at + egui::vec2(0.0, -40.0))], |ui| {
            build(ui, &mut state)
        });

        assert!(state > 0.55, "the dial snapped back to the engine's value: {state}");
    }

    #[test]
    fn a_fader_works_where_the_real_one_lives() {
        let ctx = Context::default();
        let mut value = 0.5f32;
        let seat = Rect::from_min_size(Pos2::new(10.0, 10.0), vec2(56.0, 200.0));
        let mut chip_hits = 0;

        let build = |ui: &mut Ui, value: &mut f32, chip_hits: &mut i32| {
            let mut column = crate::ui::child(
                ui,
                seat,
                egui::Layout::top_down(egui::Align::Center),
                "pitch",
            );
            column.spacing_mut().item_spacing.y = 5.0;
            if crate::ui::chip(&mut column, "SYNC", vec2(50.0, 20.0), false, true).clicked() {
                *chip_hits += 1;
            }
            fader(
                &mut column,
                egui::Id::new("real-fader"),
                value,
                vec2(44.0, 106.0),
                -8.0,
                8.0,
                true,
            );
        };

        // Press below the chip, on the fader.
        let at = Pos2::new(38.0, 120.0);
        frame(&ctx, vec![], |ui| build(ui, &mut value, &mut chip_hits));
        frame(&ctx, press(at), |ui| build(ui, &mut value, &mut chip_hits));
        frame(&ctx, vec![Event::PointerMoved(at + vec2(0.0, -40.0))], |ui| {
            build(ui, &mut value, &mut chip_hits)
        });

        assert!(
            (value - 0.5).abs() > 0.5,
            "the fader did not move where it actually lives: {value}"
        );
    }

    #[test]
    fn a_knob_survives_the_layout_around_it_changing() {
        // The bug this whole exercise is about. The FX and stems bands can be
        // toggled from the toolbar, which changes how many child panels are
        // built before a deck's knobs. An id taken from the parent's running
        // child counter moves when that happens, and a drag in progress lands
        // on a different control -- which is what "rubber-bandy" was.
        //
        // The knob is drawn at a fixed rectangle both times, so only its
        // identity is under test, not its position.
        let ctx = Context::default();
        let mut value = 0.5f32;
        let at = Pos2::new(30.0, 30.0);
        let seat = Rect::from_min_size(Pos2::new(10.0, 10.0), vec2(44.0, 57.0));

        let build = |ui: &mut Ui, value: &mut f32, extra: usize| {
            // Stand-ins for the bands that come and go above the decks.
            for i in 0..extra {
                let away = Rect::from_min_size(Pos2::new(500.0, 10.0 + i as f32 * 60.0),
                                               vec2(44.0, 57.0));
                let mut other = crate::ui::child(ui, away, crate::ui::left_row(), "band");
                let mut ignored = 0.5;
                knob(&mut other, egui::Id::new(("band", i)), &mut ignored,
                     false, "X", Color32::WHITE);
            }
            let mut here = crate::ui::child(ui, seat, crate::ui::left_row(), "deck");
            knob(&mut here, egui::Id::new("the-knob"), value, false, "T", Color32::WHITE);
        };

        // Two bands showing while the press lands.
        frame(&ctx, vec![], |ui| build(ui, &mut value, 2));
        frame(&ctx, press(at), |ui| build(ui, &mut value, 2));
        // A band disappears mid-drag, renumbering everything after it.
        frame(&ctx, vec![Event::PointerMoved(at + vec2(0.0, -40.0))], |ui| {
            build(ui, &mut value, 0)
        });

        assert!(
            value > 0.55,
            "the drag was lost when the layout changed: {value}"
        );
    }

    #[test]
    fn a_drag_keeps_working_after_the_pointer_leaves_the_control() {
        // A 44px knob is smaller than the movement that turns it, so a drag
        // that stopped at the edge would be a knob you could barely move.
        let id = egui::Id::new("knob-escape");
        let after = drag(
            |ui, value| {
                knob(ui, id, value, false, "TEST", Color32::WHITE);
            },
            0.5,
            egui::vec2(0.0, -300.0),
        );
        assert!(after > 0.95, "the drag was dropped at the edge: {after}");
    }
}
