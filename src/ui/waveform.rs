//! Waveform drawing.
//!
//! Every column is a coloured quad in a single mesh, so a lane costs one draw
//! call no matter how many columns are in it. This is the part of the console
//! that would have been the ceiling in a browser: two lanes and two overviews
//! redrawing every frame is thousands of quads, and it is nothing to a GPU
//! handed one buffer.
//!
//! The overview's mesh is built once per record and size and kept; only the
//! playhead, the played shading and the cue flags are drawn fresh. The lanes
//! are rebuilt every frame, but their columns are pinned to the record rather
//! than to the window, so the waveform slides past the playhead instead of
//! re-quantising under it -- which is what made it shimmer.

use std::sync::Arc;

use egui::{epaint::Mesh, pos2, vec2, Align2, Color32, FontId, Painter, Rect, Shape, Stroke};

use crate::library::Record;
use crate::peaks::{Bucket, Peaks};

use super::{theme, WaveMode};

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
        let galley = painter.layout_no_wrap(label.into(), FontId::monospace(theme::SIZE_XS), colour);
        let width = galley.size().x + 6.0;
        let label_left = (left + 3.0).clamp(rect.left(), (rect.right() - width).max(rect.left()));
        let label_rect = Rect::from_min_size(pos2(label_left, rect.bottom() - 18.0), vec2(width, 16.0));
        painter.rect_filled(label_rect, theme::R_S, theme::WELL);
        painter.galley(label_rect.center() - galley.size() / 2.0, galley, colour);
    }
}

/// What an overview mesh was built for. Anything else and it is rebuilt.
#[derive(Clone, PartialEq)]
struct OverviewKey {
    peaks: usize,
    rect: [i32; 4],
    length: u64,
    mode: WaveMode,
}

/// The whole record at a glance: what has played shaded, the cue points
/// flagged, the playhead drawn over the top.
pub fn overview(
    painter: &Painter,
    rect: Rect,
    peaks: Option<&Arc<Peaks>>,
    position: f64,
    length: f64,
    cues: &[Option<f64>; 4],
    mode: WaveMode,
) {
    painter.rect_filled(rect, theme::R_S, theme::WELL);

    // Empty is solid dark. A placeholder line reads as a flat signal.
    let Some(peaks) = peaks else { return };

    let key = OverviewKey {
        peaks: Arc::as_ptr(peaks) as usize,
        rect: [rect.left().round() as i32, rect.top().round() as i32,
               rect.width().round() as i32, rect.height().round() as i32],
        length: length.to_bits(),
        mode,
    };
    let id = egui::Id::new(("overview-mesh", key.peaks));
    let cached = painter.ctx().data(|d| d.get_temp::<(OverviewKey, Arc<Mesh>)>(id))
        .filter(|(built_for, _)| *built_for == key)
        .map(|(_, mesh)| mesh);
    let mesh = cached.unwrap_or_else(|| {
        let columns = rect.width().max(1.0) as usize;
        let buckets = peaks.window(0.0, length, columns);
        let step = rect.width() / buckets.len().max(1) as f32;
        let placed = buckets.iter().enumerate().map(|(i, b)| (i as f32 * step, step, *b));
        let mesh = Arc::new(columns_mesh(rect, placed, peaks.scale, 0.92, mode));
        painter.ctx().data_mut(|d| d.insert_temp(id, (key, mesh.clone())));
        mesh
    });
    painter.add(Shape::Mesh(mesh));

    if length > 0.0 {
        let x = rect.left() + (position / length).clamp(0.0, 1.0) as f32 * rect.width();
        // Played is shaded, not hidden: you still want to see where the drop
        // you just played through was.
        painter.rect_filled(Rect::from_min_max(rect.min, pos2(x, rect.bottom())), 0.0,
                            Color32::from_black_alpha(125));
        cue_flags(painter, rect, cues, 0.0, length);
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(theme::LINE_BOLD, theme::PLAYHEAD),
        );
    }
}

/// One deck's lane in the beat view: a window of the record, centred on the
/// playhead, with its measured beat grid behind it.
#[allow(clippy::too_many_arguments)]
pub fn lane(
    painter: &Painter,
    rect: Rect,
    peaks: Option<&Peaks>,
    record: Option<&Record>,
    position: f64,
    window_seconds: f64,
    deck: Color32,
    cues: &[Option<f64>; 4],
    mode: WaveMode,
) {
    let Some(peaks) = peaks else { return };

    let from = position - window_seconds / 2.0;
    let to = position + window_seconds / 2.0;
    let per_second = rect.width() as f64 / window_seconds.max(1e-6);

    // Grid first, behind the waveform: it is a reference, not a foreground.
    if let Some(record) = record {
        grid(painter, rect, record, from, to, per_second, deck);
    }

    let columns = pinned_columns(peaks, from, to, rect.width());
    if !columns.is_empty() {
        let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
        let placed = columns.into_iter().map(|c| (c.left, c.width, c.bucket));
        painter.add(Shape::mesh(columns_mesh(rect, placed, peaks.scale, 0.88, mode)));
    }

    // The part of the window already played is shaded, as on the overview.
    let now = rect.center().x;
    painter.rect_filled(Rect::from_min_max(rect.min, pos2(now, rect.bottom())), 0.0,
                        Color32::from_black_alpha(105));
    cue_flags(painter, rect, cues, from, to);
}

/// A column of a lane, in pixels from the lane's left edge.
#[derive(Clone, Copy)]
pub struct Column {
    /// Index of the first bucket folded into it. Fixed to the record, so
    /// the same column holds the same audio however the window moves.
    #[cfg_attr(not(test), allow(dead_code))]
    pub first: usize,
    pub left: f32,
    pub width: f32,
    pub bucket: Bucket,
}

/// The buckets in `from..to`, folded into columns about a pixel wide.
///
/// Columns start at whole multiples of their bucket count, counted from the
/// start of the record, and the part of a column the window starts inside
/// becomes a pixel offset. Grouping from wherever the window happens to
/// start instead -- as `Peaks::window` does -- regroups every column on every
/// frame as the playhead moves, and a waveform regrouped sixty times a
/// second boils.
pub fn pinned_columns(peaks: &Peaks, from: f64, to: f64, width: f32) -> Vec<Column> {
    let each = peaks.seconds_each;
    if peaks.buckets.is_empty() || each <= 0.0 || to <= from || width < 1.0 {
        return Vec::new();
    }
    let per_second = width as f64 / (to - from);
    let group = ((to - from) / width as f64 / each).round().max(1.0) as usize;
    let group_seconds = group as f64 * each;
    let first = (from.max(0.0) / group_seconds).floor() as usize;
    let last = ((to / group_seconds).ceil().max(0.0) as usize)
        .min(peaks.buckets.len().div_ceil(group));
    let mut out = Vec::with_capacity(last.saturating_sub(first));
    for g in first..last {
        let start = g * group;
        let end = (start + group).min(peaks.buckets.len());
        if start >= end {
            break;
        }
        out.push(Column {
            first: start,
            left: ((start as f64 * each - from) * per_second) as f32,
            width: (group_seconds * per_second) as f32,
            bucket: fold(&peaks.buckets[start..end]),
        });
    }
    out
}

/// Extremes kept, band energies averaged: a waveform that averages its way
/// through a transient is a waveform that hides the transient.
fn fold(group: &[Bucket]) -> Bucket {
    let mut out = Bucket::default();
    if group.is_empty() {
        return out;
    }
    for bucket in group {
        out.min = out.min.min(bucket.min);
        out.max = out.max.max(bucket.max);
        out.low += bucket.low;
        out.mid += bucket.mid;
        out.high += bucket.high;
    }
    let count = group.len() as f32;
    out.low /= count;
    out.mid /= count;
    out.high /= count;
    out
}

/// C1 to C4, as small numbered flags at the top of a strip of record
/// running from `from` to `to` seconds.
fn cue_flags(painter: &Painter, rect: Rect, cues: &[Option<f64>; 4], from: f64, to: f64) {
    if to <= from {
        return;
    }
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    for (slot, cue) in cues.iter().enumerate() {
        let Some(at) = cue else { continue };
        if *at < from || *at > to {
            continue;
        }
        let colour = theme::CUE_COLOURS[slot];
        let x = rect.left() + ((at - from) / (to - from)) as f32 * rect.width();
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(theme::LINE_MID, colour));
        let flag = Rect::from_min_size(pos2(x, rect.bottom() - 15.0), vec2(14.0, 14.0));
        painter.rect_filled(flag, theme::R_S, colour);
        painter.text(flag.center(), Align2::CENTER_CENTER, format!("{}", slot + 1),
                     FontId::monospace(theme::SIZE_XS), theme::GROUND);
    }
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
    let beat_width = period * per_second;
    // A grid finer than about four pixels a beat is noise, not information.
    let step = if beat_width < 4.0 { 4 } else { 1 };

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
            Rect::from_min_size(pos2(x, rect.top()), vec2(width, rect.height())),
            colour,
        );
    }

    // Phrases: a line every sixteen beats from the downbeat, a stronger one
    // every thirty-two, which is where records change what they are doing.
    // Zoomed out far enough to see several, they carry their bar number.
    let labelled = beat_width < 12.0;
    let first_phrase = ((from - downbeat) / (period * 16.0)).floor() as i64;
    let last_phrase = ((to - downbeat) / (period * 16.0)).ceil() as i64;
    let mut labels = Vec::new();
    for phrase in first_phrase..=last_phrase {
        let at = downbeat + phrase as f64 * period * 16.0;
        let x = rect.left() + ((at - from) * per_second) as f32;
        if x < rect.left() - 2.0 || x > rect.right() + 2.0 {
            continue;
        }
        let long = phrase.rem_euclid(2) == 0;
        mesh.add_colored_rect(
            Rect::from_min_size(pos2(x, rect.top()), vec2(if long { 2.0 } else { 1.4 }, rect.height())),
            Color32::from_white_alpha(if long { 46 } else { 26 }),
        );
        let bar = phrase * 4 + 1;
        if labelled && bar >= 1 && (long || beat_width >= 6.0) {
            labels.push((x, bar));
        }
    }
    if !mesh.is_empty() {
        painter.add(Shape::mesh(mesh));
    }
    for (x, bar) in labels {
        painter.text(pos2(x + 3.0, rect.top() + 2.0), Align2::LEFT_TOP, format!("{bar}"),
                     FontId::monospace(theme::SIZE_XS), theme::TEXT_MUTE);
    }
}

/// The columns themselves, as one mesh. Each is `(left, width, bucket)`, in
/// pixels from `rect.left()`.
///
/// In the blended mode, two layers per column: the peak envelope, dim, and
/// the RMS body inside it, bright. Peaks alone saturate -- a loud master hits
/// full scale in nearly every column and the lane turns into a solid block --
/// while the RMS is where the dynamics actually live. Drawing both is what
/// gives a waveform depth instead of a silhouette.
///
/// In the three-band mode, bass fills the envelope in deep blue, and the mids
/// and highs are stacked inside it by how much of the energy is theirs, so a
/// kick and a hi-hat look like different things.
fn columns_mesh(
    rect: Rect,
    columns: impl Iterator<Item = (f32, f32, Bucket)>,
    scale: f32,
    fill: f32,
    mode: WaveMode,
) -> Mesh {
    let mid = rect.center().y;
    let half = rect.height() / 2.0 * fill;

    // RMS of a full-scale sine is 0.707 of its peak, and a mix sits well
    // below that; the body is lifted so it reads at a useful height without
    // ever overtaking the envelope it lives inside.
    let body_lift = 1.7;

    let mut mesh = Mesh::default();
    for (left, width, bucket) in columns {
        let x = rect.left() + left;
        let right = x + width.max(1.0);
        let top = mid - (bucket.max * scale).clamp(0.0, 1.0) * half;
        let bottom = mid + (bucket.min * scale).abs().clamp(0.0, 1.0) * half;
        let bottom = bottom.max(top + 1.0);
        match mode {
            WaveMode::Blend => {
                let colour = theme::band_colour(bucket.low, bucket.mid, bucket.high);
                mesh.add_colored_rect(Rect::from_min_max(pos2(x, top), pos2(right, bottom)),
                                      colour.gamma_multiply(0.38));
                let rms = (bucket.low * bucket.low + bucket.mid * bucket.mid + bucket.high * bucket.high)
                    .sqrt() * scale * body_lift;
                let reach = (rms.clamp(0.0, 1.0) * half).min(((bottom - top) / 2.0).max(0.5));
                mesh.add_colored_rect(
                    Rect::from_min_max(pos2(x, mid - reach), pos2(right, (mid + reach).max(mid - reach + 1.0))),
                    colour,
                );
            }
            WaveMode::ThreeBand => {
                let energy = bucket.low + bucket.mid + bucket.high;
                mesh.add_colored_rect(Rect::from_min_max(pos2(x, top), pos2(right, bottom)), theme::WAVE_LOW);
                if energy > f32::EPSILON {
                    let upper = mid - top;
                    let lower = bottom - mid;
                    let mids = ((bucket.mid + bucket.high) / energy).sqrt().min(1.0);
                    let highs = (bucket.high / energy * 1.6).sqrt().min(1.0) * 0.85;
                    for (share, colour) in [(mids, theme::WAVE_MID), (highs, theme::WAVE_HIGH)] {
                        if share * (upper + lower) < 0.5 {
                            continue;
                        }
                        mesh.add_colored_rect(
                            Rect::from_min_max(pos2(x, mid - upper * share), pos2(right, mid + lower * share)),
                            colour,
                        );
                    }
                }
            }
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peaks() -> Peaks {
        let buckets = (0..20_000).map(|i| {
            let v = ((i * 7919) % 101) as f32 / 101.0;
            Bucket { min: -v, max: v, low: v, mid: v * 0.5, high: v * 0.25 }
        }).collect();
        Peaks { buckets, seconds_each: 256.0 / 48_000.0, scale: 1.0 }
    }

    #[test]
    fn lane_columns_stay_pinned_to_the_record_as_the_playhead_moves() {
        // The boil: the same audio has to land in the same column however
        // far the window has slid, only the pixel offset may change.
        let peaks = peaks();
        let window = 16.0;
        let width = 800.0;
        let before = pinned_columns(&peaks, 30.0, 30.0 + window, width);
        let nudge = 0.0037; // well under one column
        let after = pinned_columns(&peaks, 30.0 + nudge, 30.0 + nudge + window, width);
        let per_second = width as f64 / window;
        let shift = (nudge * per_second) as f32;
        let mut shared = 0;
        for column in &before {
            if let Some(same) = after.iter().find(|c| c.first == column.first) {
                assert_eq!(same.bucket.max, column.bucket.max, "column {} was regrouped", column.first);
                assert!((same.left - (column.left - shift)).abs() < 1e-3);
                shared += 1;
            }
        }
        assert!(shared + 2 >= before.len(), "only {shared} of {} columns survived", before.len());
    }

    #[test]
    fn three_band_mode_stacks_bass_mids_and_highs_in_their_own_colours() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 100.0));
        let bucket = Bucket { min: -0.8, max: 0.8, low: 0.6, mid: 0.3, high: 0.2 };
        let mesh = columns_mesh(rect, std::iter::once((0.0, 1.0, bucket)), 1.0, 1.0, WaveMode::ThreeBand);
        let colours: Vec<Color32> = mesh.vertices.chunks(4).map(|quad| quad[0].color).collect();
        assert_eq!(colours, [theme::WAVE_LOW, theme::WAVE_MID, theme::WAVE_HIGH]);
        // Each layer sits inside the one before it.
        let height = |quad: usize| mesh.vertices[quad * 4 + 2].pos.y - mesh.vertices[quad * 4].pos.y;
        assert!(height(0) > height(1) && height(1) > height(2), "{} {} {}", height(0), height(1), height(2));
        // Silence is still drawn, as a hairline, and never as a stray mid.
        let quiet = Bucket::default();
        let mesh = columns_mesh(rect, std::iter::once((0.0, 1.0, quiet)), 1.0, 1.0, WaveMode::ThreeBand);
        assert_eq!(mesh.vertices.len(), 4);
    }

    #[test]
    fn columns_cover_the_window_at_about_a_pixel_each() {
        let peaks = peaks();
        let columns = pinned_columns(&peaks, 10.0, 26.0, 800.0);
        let covered: f32 = columns.iter().map(|c| c.width).sum();
        assert!((covered - 800.0).abs() < 10.0, "covered {covered}");
        assert!(columns.iter().all(|c| c.width >= 0.9 && c.width < 2.5));
    }

    #[test]
    fn a_window_before_the_record_starts_is_placed_where_the_record_begins() {
        let peaks = peaks();
        let columns = pinned_columns(&peaks, -4.0, 4.0, 400.0);
        assert_eq!(columns[0].first, 0);
        assert!((columns[0].left - 200.0).abs() < 1.0, "left {}", columns[0].left);
    }
}
