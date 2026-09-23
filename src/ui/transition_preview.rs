//! The next mix, drawn before it happens.
//!
//! Both records on the station clock, the overlap between them, and every
//! move the station has planned across it: the level each deck is faded
//! along, the EQ bands, the filters, an echo tail. Read-only -- it is a
//! picture of the plan, not a way to change it.
//!
//! Nothing here knows the names of the station's transition styles. It draws
//! whatever curves an item carries and prints whatever the style is called,
//! so a technique added to the station shows up here without a change.

use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Stroke, Ui};

use super::theme;
use crate::airtime::{band_knob, filter_fader, Curve, Scheduled};

/// Height the strip wants.
pub const HEIGHT: f32 = 92.0;

/// The mix to preview: the record going out, the one coming in, and the
/// span of station time where both are sounding.
pub struct Pair<'a> {
    pub outgoing: &'a Scheduled,
    pub incoming: &'a Scheduled,
    pub overlap: (f64, f64),
}

/// The first planned mix that has not finished yet.
pub fn next_pair(schedule: &[Scheduled], now: f64) -> Option<Pair<'_>> {
    let mut music: Vec<&Scheduled> = schedule.iter().filter(|item| item.is_music()).collect();
    music.sort_by(|a, b| a.start_at.total_cmp(&b.start_at));
    music.windows(2).find_map(|pair| {
        let (outgoing, incoming) = (pair[0], pair[1]);
        incoming.transition.as_ref()?;
        let start = incoming.start_at;
        let end = outgoing.ends_at().min(incoming.ends_at()).max(start);
        (end > now).then_some(Pair { outgoing, incoming, overlap: (start, end) })
    })
}

/// A breakpoint list read the way the station reads its own: linear
/// between points, flat beyond the ends.
fn envelope_at(points: &[[f32; 2]], t: f32) -> Option<f32> {
    let first = points.first()?;
    let last = points.last()?;
    if t <= first[0] {
        return Some(first[1]);
    }
    if t >= last[0] {
        return Some(last[1]);
    }
    points.windows(2).find(|pair| pair[0][0] <= t && t <= pair[1][0]).map(|pair| {
        let ([t0, v0], [t1, v1]) = (pair[0], pair[1]);
        if t1 <= t0 { v1 } else { v0 + (v1 - v0) * (t - t0) / (t1 - t0) }
    })
}

/// Every lane of movement an item carries, as a colour and a reading of its
/// position (0..1) at a moment in the item. Empty curves are left out.
#[allow(clippy::type_complexity)]
fn moves(item: &Scheduled) -> Vec<(&'static str, Color32, Box<dyn Fn(f32) -> Option<f32> + '_>)> {
    let mut out: Vec<(&'static str, Color32, Box<dyn Fn(f32) -> Option<f32> + '_>)> = Vec::new();
    let band = |curve: &Curve| -> Box<dyn Fn(f32) -> Option<f32> + '_> {
        let curve = curve.clone();
        Box::new(move |t| curve.at(t).map(band_knob))
    };
    let automation = &item.automation;
    if !automation.low.is_empty() { out.push(("low", theme::WAVE_LOW, band(&automation.low))); }
    if !automation.mid.is_empty() { out.push(("mid", theme::WAVE_MID, band(&automation.mid))); }
    if !automation.high.is_empty() { out.push(("high", theme::WAVE_HIGH, band(&automation.high))); }
    // A filter sweep reads as how far the filter has closed.
    if !automation.lpf.is_empty() {
        let curve = automation.lpf.clone();
        out.push(("low-pass", theme::PLAYHEAD, Box::new(move |t| curve.at(t).map(|hz| -filter_fader(hz, true)))));
    }
    if !automation.hpf.is_empty() {
        let curve = automation.hpf.clone();
        out.push(("high-pass", theme::CUE_COLOURS[3], Box::new(move |t| curve.at(t).map(|hz| filter_fader(hz, false)))));
    }
    out
}

/// The words for what the station has planned on one item, for the legend.
fn move_names(item: &Scheduled) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = moves(item).into_iter().map(|(name, _, _)| name).collect();
    if item.echo.is_some() {
        names.push("echo");
    }
    if (item.rate_curve.len() > 1) || (item.playback_rate - 1.0).abs() > 0.001 {
        names.push("tempo");
    }
    names
}

pub fn draw(ui: &mut Ui, rect: Rect, schedule: &[Scheduled], now: f64) {
    let Some(pair) = next_pair(schedule, now) else { return };
    super::plate(ui, rect);
    let inner = rect.shrink2(vec2(10.0, 6.0));
    let painter = ui.painter().with_clip_rect(rect);

    // Heading: what the style is called, as the station names it.
    let transition = pair.incoming.transition.clone().unwrap_or_default();
    let (start, end) = pair.overlap;
    let when = if now >= start {
        "mixing now".to_string()
    } else {
        format!("in {}", super::mmss(start - now))
    };
    let style = if transition.preset.is_empty() { "mix".to_string() } else { transition.preset.replace(['_', '-'], " ") };
    let heading = format!("Next mix: {style} · {:.0}s overlap · {when}", end - start);
    painter.text(inner.left_top(), Align2::LEFT_TOP, heading, FontId::proportional(theme::SIZE_S), theme::TEXT);
    let mut planned: Vec<&str> = move_names(pair.outgoing);
    for name in move_names(pair.incoming) {
        if !planned.contains(&name) { planned.push(name); }
    }
    if !planned.is_empty() {
        painter.text(inner.right_top(), Align2::RIGHT_TOP, planned.join(" · "),
                     FontId::monospace(theme::SIZE_XS), theme::TEXT_MUTE);
    }

    // The time axis: the overlap, with room either side to see it coming.
    let pad = ((end - start) * 0.5).max(6.0);
    let from = start - pad;
    let to = end.max(start + transition.overlap) + pad;
    let lanes = Rect::from_min_max(inner.min + vec2(0.0, 18.0), inner.max);
    let x = |t: f64| lanes.left() + ((t - from) / (to - from)).clamp(0.0, 1.0) as f32 * lanes.width();
    let lane_height = (lanes.height() - 4.0) / 2.0;

    painter.rect_filled(Rect::from_min_max(pos2(x(start), lanes.top()), pos2(x(end), lanes.bottom())),
                        2.0, theme::BLUE.gamma_multiply(0.14));
    for (row, (item, colour)) in [(pair.outgoing, theme::BLUE), (pair.incoming, theme::CYAN)].into_iter().enumerate() {
        let top = lanes.top() + row as f32 * (lane_height + 4.0);
        let lane = Rect::from_min_max(pos2(x(item.start_at.max(from)), top),
                                      pos2(x(item.ends_at().min(to)), top + lane_height));
        if lane.width() < 1.0 {
            continue;
        }
        item_lane(&painter, lane, item, colour, from, to, lanes);
    }

    // Where the station clock is now, if it is in view.
    if now >= from && now <= to {
        let at = x(now);
        painter.line_segment([pos2(at, lanes.top()), pos2(at, lanes.bottom())], Stroke::new(1.5, theme::PLAYHEAD));
    }

    let hover = ui.interact(rect, ui.id().with("transition-preview"), egui::Sense::hover());
    if !transition.reason.is_empty() {
        hover.on_hover_text(&transition.reason);
    }
}

/// One record's bar: its fade drawn as a filled level, the moves on it as
/// lines, and its title.
fn item_lane(painter: &egui::Painter, lane: Rect, item: &Scheduled, colour: Color32, from: f64, to: f64, axis: Rect) {
    painter.rect_filled(lane, 3.0, theme::WELL);
    let steps = (lane.width() / 3.0).clamp(8.0, 240.0) as usize;
    let seconds = |i: usize| -> f64 {
        let t = from + (to - from) * ((lane.left() - axis.left()) / axis.width()) as f64
            + (to - from) * (lane.width() / axis.width()) as f64 * i as f64 / steps as f64;
        t - item.start_at
    };
    let x = |i: usize| lane.left() + lane.width() * i as f32 / steps as f32;

    // The level: how far up its fader the record is, as a filled shape.
    let envelope = if item.deck_envelope.is_empty() { &item.envelope } else { &item.deck_envelope };
    let mut mesh = egui::Mesh::default();
    for i in 0..steps {
        let level = envelope_at(envelope, seconds(i) as f32).unwrap_or(1.0).clamp(0.0, 1.0);
        let top = lane.bottom() - level * lane.height();
        mesh.add_colored_rect(Rect::from_min_max(pos2(x(i), top), pos2(x(i + 1), lane.bottom())),
                              colour.gamma_multiply(0.28));
    }
    painter.add(egui::Shape::mesh(mesh));

    // Echo tails are a span, not a curve.
    if let Some([start, end, ..]) = item.echo {
        let t0 = item.start_at + start as f64;
        let t1 = item.start_at + end as f64;
        let place = |t: f64| axis.left() + ((t - from) / (to - from)).clamp(0.0, 1.0) as f32 * axis.width();
        let span = Rect::from_min_max(pos2(place(t0), lane.top()), pos2(place(t1), lane.top() + 3.0));
        painter.rect_filled(span, 1.0, theme::AMBER);
    }

    for (_, line_colour, read) in moves(item) {
        let points: Vec<egui::Pos2> = (0..=steps)
            .filter_map(|i| read(seconds(i) as f32).map(|v| pos2(x(i), lane.bottom() - v.clamp(0.0, 1.0) * lane.height())))
            .collect();
        if points.len() > 1 {
            painter.add(egui::Shape::line(points, egui::epaint::PathStroke::new(1.4, line_colour)));
        }
    }

    let title = if item.artist.is_empty() { item.title.clone() } else { format!("{} - {}", item.artist, item.title) };
    let text = Rect::from_min_size(lane.min + vec2(5.0, 2.0), vec2((lane.width() - 10.0).max(0.0), lane.height()));
    let galley = painter.layout(title, FontId::proportional(theme::SIZE_XS), theme::TEXT, text.width().max(1.0));
    painter.with_clip_rect(text.intersect(painter.clip_rect())).galley(text.min, galley, theme::TEXT);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(now: f64) -> Vec<Scheduled> {
        let body = serde_json::json!({"now": now, "items": [
            {"id": "a", "kind": "music", "url": "/a", "start_at": 0, "duration": 180,
             "meta": {"title": "Out"}},
            {"id": "talk", "kind": "voice", "url": "/v", "start_at": 170, "duration": 6},
            {"id": "b", "kind": "music", "url": "/b", "start_at": 172, "duration": 200,
             "meta": {"title": "In", "transition": {"preset": "loop_roll_into_drop", "overlap": 8.0}}},
            {"id": "c", "kind": "music", "url": "/c", "start_at": 364, "duration": 200,
             "meta": {"title": "Later", "transition": {"preset": "blend", "overlap": 8.0}}}
        ]});
        crate::airtime::snapshot_from(&body, 0).items
    }

    #[test]
    fn the_next_mix_is_the_first_one_not_yet_finished() {
        let items = schedule(10.0);
        let pair = next_pair(&items, 10.0).expect("a planned mix");
        assert_eq!((pair.outgoing.id.as_str(), pair.incoming.id.as_str()), ("a", "b"));
        assert_eq!(pair.overlap, (172.0, 180.0));
        // Voice between them is not a record and does not break the pair.
        let later = next_pair(&items, 181.0).expect("the one after");
        assert_eq!(later.incoming.id, "c");
        assert!(next_pair(&items, 10_000.0).is_none());
    }

    #[test]
    fn any_style_the_station_names_is_carried_through() {
        // A technique this panel has never heard of still gets its name shown.
        let items = schedule(10.0);
        let pair = next_pair(&items, 10.0).unwrap();
        assert_eq!(pair.incoming.transition.as_ref().unwrap().preset, "loop_roll_into_drop");
    }

    #[test]
    fn a_mix_with_every_kind_of_move_draws() {
        let body = serde_json::json!({"now": 170, "items": [
            {"id": "a", "kind": "music", "url": "/a", "start_at": 0, "duration": 180,
             "envelope": [[0, 1], [172, 1], [180, 0]],
             "meta": {"title": "Out", "echo": {"start": 176, "end": 180, "seconds": 0.5, "mix": 0.3},
                      "automation": {"low": [[172, 0], [176, -30]], "hpf": [[174, 20], [180, 900]]}}},
            {"id": "b", "kind": "music", "url": "/b", "start_at": 172, "duration": 200,
             "meta": {"title": "In", "deck_envelope": [[0, 0], [8, 1]],
                      "transition": {"preset": "brake", "overlap": 8.0, "reason": "a brake into the drop"},
                      "automation": {"low": [[0, -30], [4, 0]], "mid": [[0, -6], [8, 0]],
                                     "high": [[0, -3], [8, 0]], "lpf": [[0, 800], [8, 20000]]}}}
        ]});
        let items = crate::airtime::snapshot_from(&body, 0).items;
        assert_eq!(move_names(&items[1]), ["low", "mid", "high", "low-pass"]);
        assert_eq!(move_names(&items[0]), ["low", "high-pass", "echo"]);
        let ctx = egui::Context::default();
        for now in [100.0, 170.0, 175.0] {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                draw(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, HEIGHT)), &items, now);
            }).drop_without_applying_deltas();
        }
    }

    #[test]
    fn envelopes_read_like_the_station_reads_them() {
        let points = [[0.0, 0.0], [3.0, 0.5], [6.0, 1.0]];
        assert_eq!(envelope_at(&points, -1.0), Some(0.0));
        assert_eq!(envelope_at(&points, 1.5), Some(0.25));
        assert_eq!(envelope_at(&points, 9.0), Some(1.0));
        assert_eq!(envelope_at(&[], 1.0), None);
    }
}
