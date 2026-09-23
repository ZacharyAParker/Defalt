//! The controls. None of these are stock widgets, because none of the things
//! on a mixer are stock widgets.
//!
//! Everything here is drawn from the palette in `theme` and driven by
//! pointer drags rather than clicks, which is how the physical versions work
//! and how muscle memory expects them to behave.
//!
//! Every control is also reachable without a pointer: Tab lands on it, a ring
//! says so, and the arrows move it (shift for the fine corrections). Screen
//! readers are told what it is and where it sits.

use egui::{
    epaint::PathStroke, vec2, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke,
    Ui, Vec2,
};

use super::theme;

/// Vertical drag distance that covers a control's whole range. Holding shift
/// quarters it, for the corrections you make with a record already playing.
const TRAVEL: f32 = 170.0;

/// One arrow press, or one wheel notch, as a share of a control's range.
/// Shift makes an arrow a quarter of that.
const STEP: f32 = 0.02;

/// Points of trackpad travel that count as one wheel notch.
const POINTS_PER_NOTCH: f32 = 40.0;

/// Where a fader runs, and where a double-click puts it back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Travel {
    pub low: f32,
    pub high: f32,
    /// What a double-click returns it to. Unity for a level, the centre for
    /// anything that swings both ways -- never the top of the travel.
    pub reset: f32,
    /// Draws a centre notch.
    pub bipolar: bool,
}

impl Travel {
    /// Centre-detented, and reset to the centre.
    pub fn bipolar(low: f32, high: f32) -> Self {
        Travel { low, high, reset: (low + high) / 2.0, bipolar: true }
    }

    /// From silence up to `high`, reset to `reset` (unity, usually).
    pub fn level(high: f32, reset: f32) -> Self {
        Travel { low: 0.0, high, reset, bipolar: false }
    }
}

/// Wheel notches this frame, counted from the raw wheel events.
///
/// Not the smoothed delta: egui spreads one notch over several frames, so a
/// control that moved a step on every frame with any delta in it moved
/// further at 144 Hz than at 60. Events are counted once, whatever the frame
/// rate, and a trackpad moves it in proportion to how far it travelled.
pub fn wheel_notches(ui: &Ui) -> f32 {
    ui.input(|i| {
        i.events.iter().map(|event| match event {
            egui::Event::MouseWheel { unit, delta, .. } => match unit {
                egui::MouseWheelUnit::Line => delta.y,
                egui::MouseWheelUnit::Point => delta.y / POINTS_PER_NOTCH,
                egui::MouseWheelUnit::Page => delta.y * 8.0,
            },
            _ => 0.0,
        }).sum()
    })
}

/// Arrow presses on a focused control this frame, signed, with shift making
/// each one a quarter step. Up and right increase.
///
/// Claims the arrows while it has focus, so they move the control instead of
/// walking focus on to the next one.
pub fn arrow_steps(ui: &Ui, response: &Response) -> f32 {
    if !response.has_focus() {
        return 0.0;
    }
    ui.memory_mut(|m| m.set_focus_lock_filter(response.id, egui::EventFilter {
        horizontal_arrows: true,
        vertical_arrows: true,
        ..Default::default()
    }));
    ui.input(|i| {
        let mut steps = 0.0;
        for event in &i.events {
            if let egui::Event::Key { key, pressed: true, modifiers, .. } = event {
                let size = if modifiers.shift { 0.25 } else { 1.0 };
                match key {
                    egui::Key::ArrowUp | egui::Key::ArrowRight => steps += size,
                    egui::Key::ArrowDown | egui::Key::ArrowLeft => steps -= size,
                    _ => {}
                }
            }
        }
        steps
    })
}

/// The ring a keyboard user follows.
fn focus_ring(ui: &Ui, rect: Rect, response: &Response, rounding: f32) {
    if response.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), rounding, Stroke::new(theme::LINE_MID, theme::BLUE),
                                 egui::StrokeKind::Outside);
    }
}

/// Turns a control's position into what it does, in the units the audio
/// uses: dB for a level or a band, Hz for a filter, percent for a pitch.
pub type Readout<'a> = &'a dyn Fn(f32) -> String;

/// Whether a control is being read closely enough to deserve its number:
/// under the pointer, in the hand, or moving under the arrow keys.
fn reading(response: &Response) -> bool {
    response.hovered() || response.dragged() || response.has_focus()
}

/// The value of a control, in a small bubble beside it, drawn above the
/// panel so a neighbour never clips it.
pub fn bubble(ui: &Ui, anchor: Pos2, align: Align2, text: &str) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, ui.id().with("bubble")));
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::monospace(theme::SIZE_XS), theme::TEXT_BRIGHT);
    let size = galley.size() + vec2(theme::SP_2 * 2.0 - 4.0, 4.0);
    let rect = align.anchor_size(anchor, size);
    let shadow = egui::epaint::Shadow { offset: [0, 2], blur: 8, spread: 0, color: Color32::from_black_alpha(160) };
    painter.add(shadow.as_shape(rect, theme::R_S));
    painter.rect_filled(rect, theme::R_S, theme::RAISED_HI);
    painter.rect_stroke(rect, theme::R_S, Stroke::new(theme::LINE, theme::EDGE_LIT), egui::StrokeKind::Inside);
    painter.galley(rect.center() - galley.size() / 2.0, galley, theme::TEXT_BRIGHT);
}

/// Tell assistive technology what this control is and where it sits.
fn describe(response: &Response, label: &str, value: f32) {
    response.widget_info(|| egui::WidgetInfo::slider(true, value as f64, label));
}

/// A rotary knob with a detent.
///
/// `bipolar` knobs rest at the centre and run -1..1; the rest run 0..1 and
/// rest at 0.5. The arc is drawn from the detent to the mark, so what the eye
/// reads is the departure from flat rather than the absolute position.
#[cfg_attr(not(test), allow(dead_code))]
pub fn knob(
    ui: &mut Ui,
    id: egui::Id,
    value: &mut f32,
    bipolar: bool,
    label: &str,
    accent: Color32,
) -> Response {
    knob_with(ui, id, value, bipolar, label, accent, KNOB, None)
}

/// A knob's face, before its caption.
pub const KNOB: f32 = 44.0;
/// The caption under a knob.
pub const KNOB_CAPTION: f32 = 15.0;

/// A knob at a given size, telling what it does while it is being read.
#[allow(clippy::too_many_arguments)]
pub fn knob_with(
    ui: &mut Ui,
    id: egui::Id,
    value: &mut f32,
    bipolar: bool,
    label: &str,
    accent: Color32,
    diameter: f32,
    readout: Option<Readout>,
) -> Response {
    let (_, rect) = ui.allocate_space(vec2(diameter, diameter + KNOB_CAPTION));
    let mut response = ui.interact(rect, id, Sense::click_and_drag());

    let detent = if bipolar { 0.0 } else { 0.5 };
    let (low, high) = if bipolar { (-1.0, 1.0) } else { (0.0, 1.0) };
    let range = high - low;
    let before = *value;

    if response.dragged() {
        let span = if ui.input(|i| i.modifiers.shift) { TRAVEL * 4.0 } else { TRAVEL };
        *value = (*value - response.drag_delta().y / span * range).clamp(low, high);
    }
    if response.double_clicked() {
        *value = detent;
    }
    if response.contains_pointer() || response.dragged() {
        let notches = wheel_notches(ui);
        if notches != 0.0 {
            *value = (*value + notches * range * STEP).clamp(low, high);
        }
    }
    let steps = arrow_steps(ui, &response);
    if steps != 0.0 {
        *value = (*value + steps * range * STEP).clamp(low, high);
    }
    if *value != before {
        response.mark_changed();
    }
    describe(&response, label, *value);

    if ui.is_rect_visible(rect) {
        let face = Rect::from_center_size(
            rect.center_top() + vec2(0.0, diameter / 2.0),
            Vec2::splat(diameter),
        );
        let centre = face.center();
        let radius = diameter / 2.0 - 3.0;
        let painter = ui.painter();

        // 300 degrees of travel: the gap at the bottom is what tells a hand
        // where the ends are without looking.
        let sweep = 300f32.to_radians();
        let fraction = (*value - low) / range;
        let detent_fraction = (detent - low) / range;
        let angle_of = |f: f32| -sweep / 2.0 + f * sweep;

        // The whole travel as a dim track, so the lit arc reads as a share
        // of something rather than as a floating mark.
        arc(ui, centre, radius + 3.5, angle_of(0.0), angle_of(1.0),
            Stroke::new(theme::LINE_BOLD, theme::tint(theme::PANEL, accent, 0.16)));

        // Body. A soft top-left highlight and a darker lower rim are what
        // make a circle read as a physical knob rather than a coloured disc.
        painter.circle_filled(centre + vec2(0.0, 1.0), radius, Color32::from_black_alpha(140));
        painter.circle_filled(centre, radius, theme::KNOB_BODY);
        painter.circle_filled(centre - vec2(radius * 0.25, radius * 0.3), radius * 0.62,
                              theme::KNOB_SHEEN);
        painter.circle_stroke(centre, radius, Stroke::new(theme::LINE, theme::EDGE_LIT));
        if response.has_focus() {
            painter.circle_stroke(centre, radius + 6.5, Stroke::new(theme::LINE_MID, theme::BLUE));
        }

        arc(ui, centre, radius + 3.5, angle_of(detent_fraction), angle_of(fraction),
            Stroke::new(2.6, accent));

        let mark = angle_of(fraction);
        let direction = vec2(mark.sin(), -mark.cos());
        painter.line_segment(
            [centre + direction * (radius * 0.42), centre + direction * (radius * 0.86)],
            Stroke::new(theme::LINE_BOLD, theme::TEXT_BRIGHT),
        );

        let moved = (fraction - detent_fraction).abs() > 0.005;
        painter.text(
            rect.center_bottom(),
            Align2::CENTER_BOTTOM,
            label,
            FontId::monospace(theme::SIZE_XS),
            if moved { accent } else { theme::TEXT_MUTE },
        );
        if let Some(readout) = readout {
            if reading(&response) {
                bubble(ui, face.left_center() - vec2(4.0, 0.0), Align2::RIGHT_CENTER, &readout(*value));
            }
        }
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

/// A vertical fader, bare: no scale, no readout.
#[cfg_attr(not(test), allow(dead_code))]
pub fn fader(
    ui: &mut Ui,
    id: egui::Id,
    value: &mut f32,
    size: Vec2,
    travel: Travel,
    label: &str,
) -> Response {
    fader_with(ui, id, value, size, travel, label, &Scale::default())
}

/// Which side of a fader its scale is printed on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Side {
    #[default]
    Left,
    Right,
}

/// What is printed beside a fader: tick marks at values, labels where there
/// is room for them, words at the two ends, and the value while it is read.
#[derive(Default)]
pub struct Scale<'a> {
    /// `(value, label)`; an empty label is a tick alone.
    pub marks: &'a [(f32, &'a str)],
    /// The mark a hand rests on, drawn longer and brighter.
    pub home: Option<f32>,
    pub side: Side,
    /// Words above and below the travel, as `(top, bottom)`.
    pub ends: Option<(&'a str, &'a str)>,
    pub readout: Option<Readout<'a>>,
}

/// Room kept at each end of a fader for its cap to overhang.
const FADER_END: f32 = 10.0;
/// Room for a word at an end of the travel.
const END_WORD: f32 = 15.0;
/// Room for scale labels beside the slot.
const SCALE_TEXT: f32 = 24.0;

/// The fader's travel inside its rectangle: the span the cap's centre runs
/// along, bottom to top. Public so a meter can be laid against it.
pub fn fader_travel(rect: Rect, scale: &Scale) -> (f32, f32) {
    let words = if scale.ends.is_some() { END_WORD } else { 0.0 };
    (rect.bottom() - FADER_END - words, rect.top() + FADER_END + words)
}

/// A vertical fader with its scale.
#[allow(clippy::too_many_arguments)]
pub fn fader_with(
    ui: &mut Ui,
    id: egui::Id,
    value: &mut f32,
    size: Vec2,
    travel: Travel,
    label: &str,
    scale: &Scale,
) -> Response {
    let (_, rect) = ui.allocate_space(size);
    let mut response = ui.interact(rect, id, Sense::click_and_drag());
    let Travel { low, high, reset, bipolar } = travel;

    let (bottom, top) = fader_travel(rect, scale);
    let length = (bottom - top).max(1.0);
    let before = *value;
    if response.dragged() {
        let span = if ui.input(|i| i.modifiers.shift) { length * 4.0 } else { length };
        *value = (*value - response.drag_delta().y / span * (high - low)).clamp(low, high);
    }
    if response.double_clicked() {
        *value = reset.clamp(low, high);
    }
    let steps = arrow_steps(ui, &response);
    if steps != 0.0 {
        *value = (*value + steps * (high - low) * STEP).clamp(low, high);
    }
    if *value != before {
        response.mark_changed();
    }
    describe(&response, label, *value);

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let y_of = |v: f32| bottom - (v - low) / (high - low) * length;
        // Labels only where there is room for them beside the cap; the ticks
        // are always drawn.
        let labelled = scale.marks.iter().any(|(_, text)| !text.is_empty())
            && rect.width() >= 28.0 + SCALE_TEXT;
        let x = if !labelled {
            rect.center().x
        } else if scale.side == Side::Left {
            rect.left() + SCALE_TEXT + (rect.width() - SCALE_TEXT) / 2.0
        } else {
            rect.left() + (rect.width() - SCALE_TEXT) / 2.0
        };
        let slot = Rect::from_center_size(egui::pos2(x, (top + bottom) / 2.0), vec2(5.0, length + 6.0));
        painter.rect_filled(slot, 2.5, theme::SLOT);
        painter.rect_stroke(slot, 2.5, Stroke::new(theme::LINE, theme::EDGE), egui::StrokeKind::Inside);

        let toward = if scale.side == Side::Left { -1.0 } else { 1.0 };
        for (at, text) in scale.marks {
            let y = y_of(*at);
            let home = scale.home == Some(*at);
            let (reach, colour) = if home { (12.0, theme::TEXT_DIM) } else { (8.0, theme::EDGE_LIT) };
            painter.line_segment([egui::pos2(x + toward * 4.0, y), egui::pos2(x + toward * reach, y)],
                                 Stroke::new(if home { theme::LINE_MID } else { theme::LINE }, colour));
            if labelled && !text.is_empty() {
                let (anchor, align) = if scale.side == Side::Left {
                    (egui::pos2(rect.left() + SCALE_TEXT - 2.0, y), Align2::RIGHT_CENTER)
                } else {
                    (egui::pos2(rect.right() - SCALE_TEXT + 2.0, y), Align2::LEFT_CENTER)
                };
                painter.text(anchor, align, *text, FontId::monospace(theme::SIZE_XS),
                             if home { theme::TEXT_DIM } else { theme::TEXT_MUTE });
            }
        }
        if bipolar {
            painter.line_segment(
                [egui::pos2(x - 9.0, y_of(reset)), egui::pos2(x + 9.0, y_of(reset))],
                Stroke::new(theme::LINE_MID, theme::EDGE_LIT),
            );
        }
        if let Some((up, down)) = scale.ends {
            let font = FontId::monospace(theme::SIZE_XS);
            painter.text(egui::pos2(x, rect.top() + 1.0), Align2::CENTER_TOP, up, font.clone(), theme::TEXT_MUTE);
            painter.text(egui::pos2(x, rect.bottom() - 1.0), Align2::CENTER_BOTTOM, down, font, theme::TEXT_MUTE);
        }

        let cap = Rect::from_center_size(egui::pos2(x, y_of(*value)), vec2(26.0, 14.0));
        painter.rect_filled(cap.translate(vec2(0.0, 1.5)), theme::R_S, Color32::from_black_alpha(150));
        super::gradient(ui, cap, theme::R_S, theme::tint(theme::CAP, Color32::WHITE, 0.25), theme::tint(theme::CAP, Color32::BLACK, 0.2));
        painter.rect_stroke(cap, theme::R_S, Stroke::new(theme::LINE, Color32::from_black_alpha(170)),
                            egui::StrokeKind::Inside);
        painter.line_segment(
            [cap.left_center() + vec2(3.0, 0.0), cap.right_center() - vec2(3.0, 0.0)],
            Stroke::new(theme::LINE_MID, theme::CAP_LINE),
        );
        focus_ring(ui, cap, &response, theme::R_S);
        if let Some(readout) = scale.readout {
            if reading(&response) {
                let (anchor, align) = if scale.side == Side::Left {
                    (cap.right_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER)
                } else {
                    (cap.left_center() - vec2(6.0, 0.0), Align2::RIGHT_CENTER)
                };
                bubble(ui, anchor, align, &readout(*value));
            }
        }
    }

    response
}

/// A horizontal level, for the master: a filled run from silence to the cap,
/// with unity marked, so the resting place is findable without reading.
pub fn level(
    ui: &mut Ui,
    id: egui::Id,
    value: &mut f32,
    size: Vec2,
    travel: Travel,
    label: &str,
) -> Response {
    let (_, rect) = ui.allocate_space(size);
    let mut response = ui.interact(rect, id, Sense::click_and_drag());
    let Travel { low, high, reset, .. } = travel;
    let length = (rect.width() - 16.0).max(1.0);
    let before = *value;

    if response.dragged() {
        let span = if ui.input(|i| i.modifiers.shift) { length * 4.0 } else { length };
        *value = (*value + response.drag_delta().x / span * (high - low)).clamp(low, high);
    }
    if response.double_clicked() {
        *value = reset.clamp(low, high);
    }
    let steps = arrow_steps(ui, &response);
    if steps != 0.0 {
        *value = (*value + steps * (high - low) * STEP).clamp(low, high);
    }
    if *value != before {
        response.mark_changed();
    }
    describe(&response, label, *value);

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let x_of = |v: f32| rect.left() + 8.0 + (v - low) / (high - low) * length;
        let slot = Rect::from_min_max(egui::pos2(rect.left() + 3.0, rect.center().y - 3.0),
                                      egui::pos2(rect.right() - 3.0, rect.center().y + 3.0));
        painter.rect_filled(slot, 3.0, theme::SLOT);
        painter.rect_stroke(slot, 3.0, Stroke::new(1.0, theme::EDGE), egui::StrokeKind::Inside);
        let x = x_of(*value);
        painter.rect_filled(
            Rect::from_min_max(slot.min + vec2(1.0, 1.0), egui::pos2(x, slot.bottom() - 1.0)),
            2.0,
            theme::BLUE_DEEP,
        );
        // Unity gets a tick, because it is where a double-click goes.
        let unity = x_of(reset);
        painter.line_segment(
            [egui::pos2(unity, rect.top() + 2.0), egui::pos2(unity, slot.top() - 1.0)],
            Stroke::new(1.2, theme::TEXT_MUTE),
        );
        let cap = Rect::from_center_size(egui::pos2(x, rect.center().y), vec2(12.0, 18.0));
        painter.rect_filled(cap, 3.0, theme::CAP);
        painter.rect_stroke(cap, 3.0, Stroke::new(1.0, Color32::from_black_alpha(180)),
                            egui::StrokeKind::Inside);
        focus_ring(ui, cap, &response, 3.0);
    }
    response
}

/// A thin horizontal level laid over an exact rectangle, for the stem strips
/// and the effects rack. Runs 0..1; double-click puts it back to `reset`.
#[allow(clippy::too_many_arguments)]
pub fn slim(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    value: &mut f32,
    reset: f32,
    live: bool,
    lit: bool,
    label: &str,
) -> Response {
    let mut response = ui.interact(rect, id, if live { Sense::click_and_drag() } else { Sense::hover() });
    let before = *value;
    if live {
        if response.dragged() {
            let span = if ui.input(|i| i.modifiers.shift) { 4.0 } else { 1.0 } * rect.width().max(1.0);
            *value = (*value + response.drag_delta().x / span).clamp(0.0, 1.0);
        }
        if response.double_clicked() {
            *value = reset;
        }
        let steps = arrow_steps(ui, &response);
        if steps != 0.0 {
            *value = (*value + steps * STEP * 2.5).clamp(0.0, 1.0);
        }
    }
    if *value != before {
        response.mark_changed();
    }
    describe(&response, label, *value);

    let track = Rect::from_center_size(rect.center(), vec2(rect.width(), 5.0));
    let painter = ui.painter();
    painter.rect_filled(track, 2.5, theme::SLOT);
    if live && lit {
        painter.rect_filled(
            Rect::from_min_size(track.min, vec2(track.width() * *value, track.height())),
            2.5,
            theme::BLUE_DEEP,
        );
    }
    let cap = Rect::from_center_size(
        egui::pos2(track.left() + track.width() * *value, track.center().y),
        vec2(9.0, 13.0),
    );
    painter.rect_filled(cap, 2.0, if live { theme::CAP } else { theme::RAISED_HI });
    focus_ring(ui, cap, &response, 2.0);
    response
}

/// Where each lamp of a meter comes on, in dBFS, bottom to top.
///
/// Not evenly spaced: the quiet end is coarse and the top is fine, because
/// the last six decibels are the ones a mix is steered by. The top lamp is
/// the red one, lit only above -0.5 dB.
pub const METER_STEPS: [f32; 17] = [
    -54.0, -48.0, -42.0, -36.0, -30.0, -26.0, -22.0, -18.0, -15.0, -12.0, -9.0, -6.0, -4.5, -3.0, -2.0, -1.0,
    -0.5,
];
/// How long the highest lamp stays lit after the level has fallen.
pub const PEAK_HOLD: f64 = 1.5;
/// How long the clip lamp stays lit after an over.
pub const CLIP_LATCH: f64 = 2.0;

/// A lamp's colour when lit: the channel's own colour, amber in the last six
/// decibels, red at the top.
pub fn meter_colour(step: f32, base: Color32) -> Color32 {
    if step >= -0.5 {
        theme::RED
    } else if step >= -6.0 {
        theme::AMBER
    } else {
        base
    }
}

/// How many lamps a linear peak lights.
pub fn lamps_lit(peak: f32) -> usize {
    let db = 20.0 * peak.max(1e-6).log10();
    METER_STEPS.iter().filter(|step| db > **step).count()
}

/// What a meter remembers between frames: the peak it is holding and since
/// when, and when it last went over.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Hold {
    pub lamps: usize,
    pub since: f64,
    pub clipped: Option<f64>,
}

impl Hold {
    /// Take this frame's level at time `now`: a higher lamp takes the hold,
    /// a lower one waits for the hold to run out, and an over latches.
    pub fn update(mut self, lamps: usize, over: bool, now: f64) -> Self {
        if lamps >= self.lamps || now - self.since > PEAK_HOLD {
            self.lamps = lamps;
            self.since = now;
        }
        if over {
            self.clipped = Some(now);
        }
        if self.clipped.is_some_and(|at| now - at > CLIP_LATCH) {
            self.clipped = None;
        }
        self
    }
}

/// A channel meter laid over an exact rectangle: a column of lamps, unlit
/// ones still faintly there so the scale is readable at rest, the highest
/// recent peak held for a moment, and a clip lamp at the top that stays lit
/// for two seconds after an over.
///
/// `peak` is linear. `over` is the moment the signal went past full scale,
/// which the caller knows better than a decayed peak does.
pub fn level_meter(ui: &Ui, rect: Rect, id: egui::Id, peak: f32, over: bool, colour: Color32, vertical: bool) {
    let now = ui.input(|i| i.time);
    let lit = lamps_lit(peak);
    let hold = ui.data(|d| d.get_temp::<Hold>(id)).unwrap_or_default().update(lit, over, now);
    ui.data_mut(|d| d.insert_temp(id, hold));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::WELL);
    let inner = rect.shrink(1.0);
    // The clip lamp is set apart from the column by a gap, so it reads as
    // a separate warning rather than as one more step.
    let lamp_size = if vertical { inner.width().min(5.0) } else { inner.height().min(5.0) };
    let (column, lamp) = if vertical {
        (Rect::from_min_max(inner.min + vec2(0.0, lamp_size + 2.0), inner.max),
         Rect::from_min_size(inner.min, vec2(inner.width(), lamp_size)))
    } else {
        (Rect::from_min_max(inner.min, inner.max - vec2(lamp_size + 2.0, 0.0)),
         Rect::from_min_size(egui::pos2(inner.right() - lamp_size, inner.top()), vec2(lamp_size, inner.height())))
    };
    let steps = METER_STEPS.len();
    for (i, step) in METER_STEPS.iter().enumerate() {
        let cell = if vertical {
            let h = column.height() / steps as f32;
            Rect::from_min_size(egui::pos2(column.left(), column.bottom() - (i as f32 + 1.0) * h),
                                vec2(column.width(), (h - 1.0).max(1.0)))
        } else {
            let w = column.width() / steps as f32;
            Rect::from_min_size(egui::pos2(column.left() + i as f32 * w, column.top()),
                                vec2((w - 1.0).max(1.0), column.height()))
        };
        let on = meter_colour(*step, colour);
        let held = hold.lamps > 0 && i + 1 == hold.lamps;
        let fill = if i < lit || held { on } else { on.gamma_multiply(0.14) };
        painter.rect_filled(cell, 0.5, fill);
    }
    let clip = if hold.clipped.is_some() { theme::RED } else { theme::RED.gamma_multiply(0.16) };
    painter.rect_filled(lamp, 0.5, clip);
}

/// A plain level meter, laid out in a `Ui`, in the panel's blue. `peak` is
/// linear, not decibels; the scale is.
pub fn meter(ui: &mut Ui, peak: f32, size: Vec2, vertical: bool) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    level_meter(ui, rect, response.id, peak, peak > 1.0, theme::BLUE, vertical);
}

pub struct PlatterOut {
    pub response: Response,
    /// Radians the hand moved this frame, seam-corrected.
    pub turned: f32,
    /// Radians the arrow keys moved it this frame, while it had focus.
    pub nudged: f32,
}

/// How a platter is dressed: its deck's colour, and how urgently the end of
/// the record is coming (0 calm, 1 a pulse at its brightest).
#[derive(Clone, Copy)]
pub struct Dress {
    pub colour: Color32,
    pub ending: f32,
}

/// The platter. Turns because the record is moving, not because a timer is
/// running -- a stopped deck has to look stopped.
///
/// Drawn as a turntable: a bevelled rim, grooves, a label in the deck's
/// colour, a marker that turns with the record, and two reflections that do
/// not -- light falls on a record from the room, so the shine stays put while
/// the grooves go round under it, and that is what makes turning visible.
pub fn platter(
    ui: &mut Ui,
    id: egui::Id,
    diameter: f32,
    spin: f32,
    progress: f32,
    loaded: bool,
    dress: Dress,
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
    // An arrow is a sixteenth of a turn: about the size of correction a hand
    // resting on the platter makes.
    let steps = arrow_steps(ui, &response);
    let nudged = if loaded { steps * std::f32::consts::TAU / 16.0 } else { 0.0 };
    describe(&response, "Jog wheel", progress);

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = diameter / 2.0 - 2.0;
        let label = radius * 0.5;
        let colour = dress.colour;

        // Rim, with a bevel: light along the top edge, shade along the
        // bottom.
        painter.circle_filled(centre + vec2(0.0, 2.0), radius, Color32::from_black_alpha(160));
        painter.circle_filled(centre, radius, theme::PLATTER_EDGE);
        arc(ui, centre, radius - 0.5, -1.9, 1.9, Stroke::new(theme::LINE, Color32::from_white_alpha(34)));
        arc(ui, centre, radius - 0.5, 2.3, 3.98, Stroke::new(theme::LINE, Color32::from_black_alpha(200)));

        // The progress ring's track sits on the rim; the body inside it.
        let ring = radius - 5.0;
        painter.circle_filled(centre, radius - 8.0, theme::PLATTER_BODY);
        painter.circle_stroke(centre, radius - 8.0, Stroke::new(theme::LINE, Color32::from_white_alpha(10)));

        // Grooves: fine rings between the strobe and the label.
        let (outer, inner) = (radius - 19.0, label + 5.0);
        let grooves = 7;
        for i in 0..grooves {
            let r = inner + (outer - inner) * (i as f32 + 0.5) / grooves as f32;
            painter.circle_stroke(centre, r, Stroke::new(theme::LINE, Color32::from_white_alpha(if i % 3 == 0 { 16 } else { 9 })));
        }
        // Two fixed reflections, opposite one another.
        for middle in [-0.85f32, std::f32::consts::PI - 0.85] {
            sheen(painter, centre, inner, outer, middle, 0.55, 30);
        }

        // Strobe dots, turning with the record.
        let dots = 44;
        for i in 0..dots {
            let angle = spin + (i as f32 / dots as f32) * std::f32::consts::TAU;
            let at = centre + vec2(angle.cos(), angle.sin()) * (radius - 13.0);
            painter.circle_filled(at, 1.6,
                if i % 4 == 0 { theme::STROBE_LIT } else { theme::STROBE });
        }

        // The label: the deck's colour, darkened so the numbers on it read.
        painter.circle_filled(centre, label, if loaded { theme::tint(theme::PLATTER_BODY, colour, 0.22) } else { theme::PLATTER_SHEEN });
        painter.circle_stroke(centre, label, Stroke::new(theme::LINE_MID,
            if loaded { colour.gamma_multiply(0.8) } else { theme::EDGE }));

        // Progress, in the deck's colour over a dim track. In the last half
        // minute the ring leans towards red and back, so the end of the
        // record is seen without being read.
        painter.circle_stroke(centre, ring, Stroke::new(4.0, theme::tint(theme::PLATTER_EDGE, colour, 0.18)));
        if loaded {
            let lit = theme::tint(colour, theme::RED, dress.ending.clamp(0.0, 1.0));
            arc(ui, centre, ring, 0.0, progress.clamp(0.0, 1.0) * std::f32::consts::TAU, Stroke::new(4.0, lit));

            // The marker turns with the record: a bright tick just outside
            // the label, so even a slow turn reads.
            let mark = vec2(spin.cos(), spin.sin());
            painter.line_segment(
                [centre + mark * (label + 3.0), centre + mark * (radius - 19.0)],
                Stroke::new(2.5, colour),
            );
        }
        if response.has_focus() {
            painter.circle_stroke(centre, radius + 1.5, Stroke::new(theme::LINE_MID, theme::BLUE));
        }
    }

    PlatterOut { response, turned, nudged }
}

/// A wedge of light across the grooves, brightest along `middle` and fading
/// to nothing at `middle ± width / 2`. Angles are egui's, from three o'clock.
fn sheen(painter: &egui::Painter, centre: Pos2, inner: f32, outer: f32, middle: f32, width: f32, alpha: u8) {
    let slices = 12;
    let mut mesh = egui::Mesh::default();
    for i in 0..=slices {
        let t = i as f32 / slices as f32;
        let angle = middle - width / 2.0 + width * t;
        let strength = (std::f32::consts::PI * t).sin();
        let colour = Color32::from_white_alpha((alpha as f32 * strength) as u8);
        let direction = vec2(angle.cos(), angle.sin());
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(centre + direction * inner, colour.gamma_multiply(0.4));
        mesh.colored_vertex(centre + direction * outer, colour);
        if i > 0 {
            mesh.add_triangle(base - 2, base - 1, base + 1);
            mesh.add_triangle(base - 2, base + 1, base);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
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
    let steps = arrow_steps(ui, &response);
    if steps != 0.0 {
        *value = (*value + steps * STEP).clamp(0.0, 1.0);
    }
    if *value != before {
        response.mark_changed();
    }
    describe(&response, "Crossfader", *value);

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
        focus_ring(ui, cap, &response, 3.0);
    }

    response
}

/* ── Do the controls actually move? ──────────────────────────────────────
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
                fader(ui, id, value, egui::vec2(40.0, 120.0), Travel::level(1.0, 1.0), "T");
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
            |ui, value| fader(ui, id, value, egui::vec2(40.0, 120.0), Travel::level(1.0, 1.0), "T"),
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
                Travel::bipolar(-8.0, 8.0),
                "Pitch",
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

    /// Double-click at the middle of a control and report where it ended.
    fn double_click(mut build: impl FnMut(&mut Ui, &mut f32), start: f32) -> f32 {
        let ctx = Context::default();
        let mut value = start;
        let at = Pos2::new(20.0, 40.0);
        let mut run = |time: f64, events: Vec<Event>, value: &mut f32| {
            let input = RawInput { time: Some(time), events, ..Default::default() };
            ctx.run_ui(input, |ui| build(ui, value)).drop_without_applying_deltas();
        };
        run(0.0, vec![], &mut value);
        for (time, pressed) in [(0.1, true), (0.15, false), (0.2, true), (0.25, false)] {
            run(time, vec![Event::PointerMoved(at), Event::PointerButton {
                pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE,
            }], &mut value);
        }
        value
    }

    #[test]
    fn a_double_clicked_volume_fader_goes_back_to_unity_not_the_top() {
        let after = double_click(|ui, value| {
            fader(ui, egui::Id::new("vol"), value, vec2(40.0, 120.0), Travel::level(2.0, 1.0), "Volume");
        }, 0.3);
        assert_eq!(after, 1.0);
    }

    #[test]
    fn a_double_clicked_pitch_or_filter_fader_goes_back_to_centre() {
        for (low, high, start) in [(-8.0, 8.0, 5.0), (-1.0, 1.0, -0.7)] {
            let after = double_click(|ui, value| {
                fader(ui, egui::Id::new("centred"), value, vec2(40.0, 120.0), Travel::bipolar(low, high), "T");
            }, start);
            assert_eq!(after, 0.0);
        }
    }

    #[test]
    fn a_double_clicked_master_goes_back_to_unity() {
        let after = double_click(|ui, value| {
            level(ui, egui::Id::new("master"), value, vec2(178.0, 60.0), Travel::level(1.5, 1.0), "Master");
        }, 0.4);
        assert_eq!(after, 1.0);
    }

    /// Turn a knob with wheel events, then run on for some idle frames.
    fn wheel(events_per_frame: &[Vec<Event>], idle: usize) -> f32 {
        let ctx = Context::default();
        let mut value = 0.5f32;
        let at = Pos2::new(20.0, 20.0);
        let build = |ui: &mut Ui, value: &mut f32| {
            knob(ui, egui::Id::new("wheel"), value, false, "T", Color32::WHITE);
        };
        frame(&ctx, vec![Event::PointerMoved(at)], |ui| build(ui, &mut value));
        for events in events_per_frame {
            frame(&ctx, events.clone(), |ui| build(ui, &mut value));
        }
        for _ in 0..idle {
            frame(&ctx, vec![], |ui| build(ui, &mut value));
        }
        value
    }

    fn notch(lines: f32) -> Event {
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, lines),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn a_wheel_notch_moves_a_knob_one_step_however_many_frames_follow() {
        // The smoothed delta spread one notch across frames and stepped on
        // each, so the same notch moved further at a higher frame rate.
        let few = wheel(&[vec![notch(1.0)]], 2);
        let many = wheel(&[vec![notch(1.0)]], 40);
        assert!((few - 0.52).abs() < 1e-5, "one notch moved it to {few}");
        assert_eq!(few, many, "the frame count changed how far a notch went");
        // Three notches in one frame or across three frames land the same.
        let together = wheel(&[vec![notch(1.0), notch(1.0), notch(1.0)]], 5);
        let apart = wheel(&[vec![notch(1.0)], vec![notch(1.0)], vec![notch(1.0)]], 5);
        assert!((together - apart).abs() < 1e-6);
    }

    #[test]
    fn a_focused_control_moves_with_the_arrows_and_reports_itself() {
        let ctx = Context::default();
        let id = egui::Id::new("keyed");
        let mut value = 0.5f32;
        let key = |key: egui::Key, shift: bool| Event::Key {
            key, physical_key: None, pressed: true, repeat: false,
            modifiers: if shift { Modifiers::SHIFT } else { Modifiers::NONE },
        };
        let mut info = None;
        let mut run = |events: Vec<Event>, value: &mut f32| {
            let output = ctx.run_ui(RawInput { events, ..Default::default() }, |ui| {
                knob(ui, id, value, false, "HIGH", Color32::WHITE);
            });
            info = output.platform_output.events.iter().find_map(|e| match e {
                egui::output::OutputEvent::ValueChanged(info) => Some(info.clone()),
                _ => None,
            }).or(info.clone());
            output.drop_without_applying_deltas();
        };
        run(vec![], &mut value);
        ctx.memory_mut(|m| m.request_focus(id));
        run(vec![], &mut value);
        run(vec![key(egui::Key::ArrowUp, false)], &mut value);
        assert!((value - 0.52).abs() < 1e-5, "up arrow gave {value}");
        run(vec![key(egui::Key::ArrowDown, true)], &mut value);
        assert!((value - 0.515).abs() < 1e-5, "shift made a fine step, got {value}");
        assert!(ctx.memory(|m| m.has_focus(id)), "the arrows walked focus away");
        let info = info.expect("a knob reports its value to assistive technology");
        assert_eq!(info.typ, egui::WidgetType::Slider);
    }

    #[test]
    fn meters_light_by_the_decibel_and_turn_amber_then_red_at_the_top() {
        assert_eq!(lamps_lit(0.0), 0);
        assert_eq!(lamps_lit(1.0), METER_STEPS.len(), "full scale lights every lamp");
        // -3 dB lights the -4.5 lamp but not the -3 one, and it is amber.
        let three = lamps_lit(10f32.powf(-3.0 / 20.0));
        assert_eq!(METER_STEPS[three - 1], -4.5);
        assert_eq!(meter_colour(METER_STEPS[three - 1], Color32::WHITE), theme::AMBER);
        assert_eq!(meter_colour(-12.0, Color32::WHITE), Color32::WHITE);
        assert_eq!(meter_colour(-0.5, Color32::WHITE), theme::RED);
        assert_eq!(meter_colour(-0.6, Color32::WHITE), theme::AMBER);
    }

    #[test]
    fn a_meter_holds_its_peak_then_lets_go_and_latches_an_over() {
        let hold = Hold::default().update(12, false, 0.0);
        let hold = hold.update(3, false, 1.0);
        assert_eq!(hold.lamps, 12, "the peak let go before the hold ran out");
        let hold = hold.update(3, false, 1.6);
        assert_eq!(hold.lamps, 3, "the peak was held past its time");
        let hold = hold.update(17, true, 2.0);
        assert!(hold.clipped.is_some());
        assert!(hold.update(0, false, 3.9).clipped.is_some(), "the clip lamp went out early");
        assert!(hold.update(0, false, 4.1).clipped.is_none(), "the clip lamp never went out");
    }

    #[test]
    fn a_fader_scale_leaves_the_travel_exactly_where_the_bare_fader_had_it() {
        // Words at the ends shorten the travel; a scale without them must
        // not, or the unity mark would sit off unity.
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(54.0, 120.0));
        assert_eq!(fader_travel(rect, &Scale::default()), (110.0, 10.0));
        let ends = Scale { ends: Some(("HP", "LP")), ..Default::default() };
        let (bottom, top) = fader_travel(rect, &ends);
        assert!(bottom < 110.0 && top > 10.0);
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
