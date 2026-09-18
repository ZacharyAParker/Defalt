//! Waveform drawing.
//!
//! Every column is a coloured quad in a single mesh, so a lane costs one draw
//! call no matter how many columns are in it. This is the part of the console
//! that would have been the ceiling in a browser: two lanes and two overviews
//! redrawing every frame is thousands of quads, and it is nothing to a GPU
//! handed one buffer.

use egui::{epaint::Mesh, pos2, Color32, Painter, Rect, Shape, Stroke};

use crate::library::Record;
use crate::peaks::Peaks;

use super::theme;

/// Transition ranges use the same source-time axis as the waveform. Both
/// boundaries stay exact; the label is clamped separately to remain readable.
pub fn transition_marks(
    painter: &Painter,
    rect: Rect,
    windows: &[crate::airtime::MixWindow],
    from: f64,
    to: f64,
) {
    if !from.is_finite() || !to.is_finite() || to <= from || rect.height() < 12.0 { return; }
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    for window in windows {
        if !window.start.is_finite() || !window.end.is_finite()
            || window.end <= window.start || window.end < from || window.start > to { continue; }
        let colour = if window.incoming { theme::CYAN } else { theme::BLUE };
        let x = |seconds: f64| rect.left() + ((seconds - from) / (to - from)) as f32 * rect.width();
        let (left, right) = (x(window.start), x(window.end));
        let span = Rect::from_min_max(pos2(left.max(rect.left()), rect.top()),
                                      pos2(right.min(rect.right()), rect.bottom()));
        painter.rect_filled(span, 0.0, colour.gamma_multiply(0.18));
        painter.line_segment([pos2(span.left(), rect.bottom() - 1.0), pos2(span.right(), rect.bottom() - 1.0)],
                             Stroke::new(2.0, colour));
        for at in [left, right] {
            if at >= rect.left() && at <= rect.right() {
                painter.line_segment([pos2(at, rect.top()), pos2(at, rect.bottom())], Stroke::new(1.5, colour));
            }
        }
        let label = if window.incoming { "MIX IN" } else { "MIX OUT" };
        let galley = painter.layout_no_wrap(label.into(), egui::FontId::monospace(10.0), colour);
        let width = galley.size().x + 6.0;
        let label_left = (left + 3.0).clamp(rect.left(), (rect.right() - width).max(rect.left()));
        let label_rect = Rect::from_min_size(pos2(label_left, rect.bottom() - 16.0), egui::vec2(width, 14.0));
        painter.rect_filled(label_rect, 2.0, theme::WELL);
        painter.galley(label_rect.min + egui::vec2(3.0, 1.0), galley, colour);
    }
}

/// The whole record at a glance, with the playhead.
pub fn overview(
    painter: &Painter,
    rect: Rect,
    peaks: Option<&Peaks>,
    position: f64,
    length: f64,
    deck: Color32,
) {
    painter.rect_filled(rect, 3.0, theme::WELL);

    // Empty is solid dark. A placeholder line reads as a flat signal.
    let Some(peaks) = peaks else { return };

    let columns = rect.width().max(1.0) as usize;
    let buckets = peaks.window(0.0, length, columns);
    draw(painter, rect, &buckets, peaks.scale, deck, 0.92);

    if length > 0.0 {
        let x = rect.left() + (position / length).clamp(0.0, 1.0) as f32 * rect.width();
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.5, theme::PLAYHEAD),
        );
    }
}

/// One deck's lane in the beat view: a window of the record, centred on the
/// playhead, with its measured beat grid behind it.
pub fn lane(
    painter: &Painter,
    rect: Rect,
    peaks: Option<&Peaks>,
    record: Option<&Record>,
    position: f64,
    window_seconds: f64,
    deck: Color32,
) {
    let Some(peaks) = peaks else { return };

    let from = position - window_seconds / 2.0;
    let to = position + window_seconds / 2.0;
    let per_second = rect.width() as f64 / window_seconds.max(1e-6);

    // Grid first, behind the waveform: it is a reference, not a foreground.
    if let Some(record) = record {
        grid(painter, rect, record, from, to, per_second, deck);
    }

    let columns = rect.width().max(1.0) as usize;
    // Leading silence means the window starts before the record does, so the
    // drawn span has to be clipped and then placed where it actually falls.
    let visible_from = from.max(0.0);
    let buckets = peaks.window(visible_from, to.max(visible_from), columns);
    if buckets.is_empty() {
        return;
    }
    let left = rect.left() + ((visible_from - from) * per_second) as f32;
    let width = (((to.max(visible_from)) - visible_from) * per_second) as f32;
    let span = Rect::from_min_size(
        pos2(left, rect.top()),
        egui::vec2(width.min(rect.width()), rect.height()),
    );
    draw(painter, span, &buckets, peaks.scale, deck, 0.88);
}

fn grid(
    painter: &Painter,
    rect: Rect,
    record: &Record,
    from: f64,
    to: f64,
    per_second: f64,
    deck: Color32,
) {
    let (Some(offset), Some(period)) = (record.beat_offset, record.beat_period) else {
        return;
    };
    if period < 0.05 {
        return;
    }
    let downbeat = record.downbeat_offset.unwrap_or(offset);

    let first = ((from - offset) / period).floor() as i64;
    let last = ((to - offset) / period).ceil() as i64;
    // A grid finer than about four pixels a beat is noise, not information.
    let step = if period * per_second < 4.0 { 4 } else { 1 };

    let mut mesh = Mesh::default();
    for beat in (first..=last).step_by(step as usize) {
        let at = offset + beat as f64 * period;
        let x = rect.left() + ((at - from) * per_second) as f32;
        if x < rect.left() - 2.0 || x > rect.right() + 2.0 {
            continue;
        }
        // The downbeat is the line you actually align; the rest is texture.
        let bar_position = ((at - downbeat) / (period * 4.0)).rem_euclid(1.0);
        let downbeat_here = bar_position < 0.02 || bar_position > 0.98;
        let (width, colour) = if downbeat_here {
            (1.6, deck.gamma_multiply(0.45))
        } else {
            (1.0, Color32::from_white_alpha(12))
        };
        mesh.add_colored_rect(
            Rect::from_min_size(pos2(x, rect.top()), egui::vec2(width, rect.height())),
            colour,
        );
    }
    if !mesh.is_empty() {
        painter.add(Shape::mesh(mesh));
    }
}

/// The columns themselves, as one mesh.
///
/// Two layers per column: the peak envelope, dim, and the RMS body inside it,
/// bright. Peaks alone saturate -- a loud master hits full scale in nearly
/// every column and the lane turns into a solid block -- while the RMS is
/// where the dynamics actually live. Drawing both is what gives a waveform
/// depth instead of a silhouette.
fn draw(
    painter: &Painter,
    rect: Rect,
    buckets: &[crate::peaks::Bucket],
    scale: f32,
    deck: Color32,
    fill: f32,
) {
    if buckets.is_empty() {
        return;
    }
    let mid = rect.center().y;
    let half = rect.height() / 2.0 * fill;
    let step = rect.width() / buckets.len() as f32;

    // RMS of a full-scale sine is 0.707 of its peak, and a mix sits well
    // below that; the body is lifted so it reads at a useful height without
    // ever overtaking the envelope it lives inside.
    let body_lift = 1.7;

    let mut mesh = Mesh::default();
    for (i, bucket) in buckets.iter().enumerate() {
        let x = rect.left() + i as f32 * step;
        let colour = theme::band_colour(bucket.low, bucket.mid, bucket.high, deck);

        let top = mid - (bucket.max * scale).clamp(0.0, 1.0) * half;
        let bottom = mid + (bucket.min * scale).abs().clamp(0.0, 1.0) * half;
        mesh.add_colored_rect(
            Rect::from_min_max(pos2(x, top), pos2(x + step.max(1.0), bottom.max(top + 1.0))),
            colour.gamma_multiply(0.38),
        );

        let rms = (bucket.low * bucket.low + bucket.mid * bucket.mid + bucket.high * bucket.high)
            .sqrt()
            * scale
            * body_lift;
        let reach = rms.clamp(0.0, 1.0) * half;
        let peak_reach = ((bottom - top) / 2.0).max(0.5);
        let reach = reach.min(peak_reach);
        mesh.add_colored_rect(
            Rect::from_min_max(
                pos2(x, mid - reach),
                pos2(x + step.max(1.0), (mid + reach).max(mid - reach + 1.0)),
            ),
            colour,
        );
    }
    painter.add(Shape::mesh(mesh));
}
