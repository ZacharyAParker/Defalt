//! A spectrum strip that belongs to the radio desk.
//!
//! Real audio only: the bands come from the engine's own analysis of what is
//! playing here. The strip is the booth's light made visible -- capsule bars
//! from warm amber in the bass through the booth's rose and violet to the
//! cool cyan of the highs, a soft reflection under them, caps that fall
//! under gravity, a glow behind the low end that swells with the bass, and a
//! border that answers the kick. With nothing playing it breathes, slowly.
//! Reduced motion keeps the bars and drops everything decorative.
use egui::{epaint::PathStroke, pos2, vec2, Align2, Color32, FontId, Mesh, Pos2, Rect, Shape, Stroke, Ui};
use std::time::{Duration, Instant};
use crate::engine::visualizer::{analyze, BANDS};
use super::theme;

/// How fast a falling peak cap speeds up, in strip-heights per second².
const GRAVITY: f32 = 2.6;
/// The bass has to jump this far over its own running average to count as
/// a beat, as a ratio and a floor.
const BEAT_RATIO: f32 = 1.35;
const BEAT_FLOOR: f32 = 0.04;

pub struct State {
    levels: [f32; BANDS],
    peaks: [f32; BANDS],
    /// How fast each peak cap is falling right now.
    falling: [f32; BANDS],
    /// The low end, smoothed: what the glow behind the bars follows.
    glow: f32,
    /// A beat's flash on the border, 1 on the beat and fading.
    pulse: f32,
    /// The bass's own running average, which a beat stands out from.
    bass: f32,
    last: Instant,
    preview: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            levels: [0.; BANDS], peaks: [0.; BANDS], falling: [0.; BANDS],
            glow: 0., pulse: 0., bass: 0., last: Instant::now(), preview: false,
        }
    }
}
impl State {
    /// A synthetic spectrum for screenshots: a plausible mix frozen on a
    /// kick, so the strip can be reviewed without playing anything.
    pub fn pose(&mut self) {
        self.preview = true;
        self.levels = std::array::from_fn(|i| {
            let x = i as f32 / BANDS as f32;
            ((0.32 + (i as f32 * 0.35).sin().abs() * 0.58) * (1. - x * 0.55) + (1. - x).powi(6) * 0.25).min(0.97)
        });
        self.peaks = std::array::from_fn(|i| (self.levels[i] + 0.05 + (i as f32 * 0.9).sin().abs() * 0.08).min(1.));
        self.glow = 0.75;
        self.pulse = 0.7;
        self.bass = 0.4;
    }

    /// How much is going on at the bottom of the spectrum, 0..1: the kick
    /// and the bass, which is what a room feels before it hears anything else.
    pub fn low_energy(&self) -> f32 {
        let low = &self.levels[..BANDS / 8];
        (low.iter().sum::<f32>() / low.len() as f32).clamp(0., 1.)
    }

    /// Move `dt` seconds toward `target`. Quick to rise and slower to fall
    /// normally; slow both ways, with no caps, glow or beat, in reduced
    /// motion.
    fn step(&mut self, target: &[f32; BANDS], dt: f32, calm: bool) {
        for (i, target) in target.iter().enumerate() {
            let response = if calm { 0.3 } else if *target > self.levels[i] { 0.045 } else { 0.24 };
            self.levels[i] += (target - self.levels[i]) * (1. - (-dt / response).exp());
            if calm || self.levels[i] >= self.peaks[i] {
                self.peaks[i] = self.levels[i];
                self.falling[i] = 0.;
            } else {
                self.falling[i] += GRAVITY * dt;
                self.peaks[i] = (self.peaks[i] - self.falling[i] * dt).max(self.levels[i]);
            }
        }
        let low = self.low_energy();
        if calm {
            self.glow = 0.;
            self.pulse = 0.;
        } else {
            self.glow += (low - self.glow) * (1. - (-dt / 0.12).exp());
            self.pulse *= (-dt / 0.2).exp();
            if low > self.bass * BEAT_RATIO + BEAT_FLOOR && self.pulse < 0.35 {
                self.pulse = 1.;
            }
        }
        self.bass += (low - self.bass) * (1. - (-dt / 1.2).exp());
    }
}

/// Whether there is live sound to analyse.
fn active(app: &crate::Defalt) -> bool {
    app.studio.spectrum.preview || (app.airtime.on && app.engine.is_some())
}

/// Step the levels toward what is playing now. Called whenever the strip or
/// the booth is showing, so the booth can move with the music even with the
/// strip itself turned off.
pub fn update(app: &mut crate::Defalt) {
    let now = Instant::now();
    let dt = now.duration_since(app.studio.spectrum.last).as_secs_f32().min(0.2);
    let preview = app.studio.spectrum.preview;
    let calm = app.studio.reduced;
    let interval = if calm { 100 } else { 33 };
    if preview || dt < interval as f32 / 1000. {
        return;
    }
    let target = match app.engine.as_ref().filter(|_| active(app)) {
        Some(engine) => analyze(&engine.telemetry.visualizer.snapshot(), engine.sample_rate),
        None => [0.; BANDS],
    };
    app.studio.spectrum.step(&target, dt, calm);
    app.studio.spectrum.last = now;
}

/// How many bars a strip this wide shows: more as it widens, each about
/// nine points apart, never fewer than the eye can read as a spectrum.
pub fn bar_count(width: f32) -> usize {
    ((width / 9.) as usize).clamp(24, 96)
}

/// A band's level at `at` (0 the lowest band, 1 the highest). The engine's
/// bands are already spaced by octave, so reading between them in a straight
/// line keeps the display logarithmic in frequency however many bars it has.
pub fn sample(levels: &[f32; BANDS], at: f32) -> f32 {
    let position = at.clamp(0., 1.) * (BANDS - 1) as f32;
    let below = position.floor() as usize;
    let above = (below + 1).min(BANDS - 1);
    let t = position - below as f32;
    levels[below] * (1. - t) + levels[above] * t
}

/// The booth's light across the spectrum: amber lows, rose and violet mids,
/// cyan highs.
pub fn palette(at: f32) -> Color32 {
    const ROSE: Color32 = Color32::from_rgb(0xf0, 0x8f, 0xa8);
    let stops = [(0.0, theme::AMBER), (0.36, ROSE), (0.68, theme::DECK_B), (1.0, theme::DECK_A)];
    let at = at.clamp(0., 1.);
    for pair in stops.windows(2) {
        let ((a, from), (b, to)) = (pair[0], pair[1]);
        if at <= b {
            return theme::tint(from, to, (at - a) / (b - a));
        }
    }
    theme::DECK_A
}

fn fade(colour: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), (alpha.clamp(0., 1.) * 255.) as u8)
}

/// A soft elliptical light: brightest in the middle, gone at the rim, as one
/// fan of triangles.
fn glow(mesh: &mut Mesh, centre: Pos2, radii: egui::Vec2, colour: Color32) {
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(centre, colour);
    let rim = 32;
    for i in 0..=rim {
        let angle = i as f32 / rim as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(centre + vec2(angle.cos() * radii.x, angle.sin() * radii.y), Color32::TRANSPARENT);
        if i > 0 {
            mesh.add_triangle(base, base + i, base + i + 1);
        }
    }
}

/// A small solid disc, for a bar's rounded tip, in the same mesh as the bar.
fn disc(mesh: &mut Mesh, centre: Pos2, radius: f32, colour: Color32) {
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(centre, colour);
    let rim = 10;
    for i in 0..=rim {
        let angle = i as f32 / rim as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(centre + vec2(angle.cos(), angle.sin()) * radius, colour);
        if i > 0 {
            mesh.add_triangle(base, base + i, base + i + 1);
        }
    }
}

/// A vertical run of colour from `top` to `bottom`, as two triangles.
fn column(mesh: &mut Mesh, rect: Rect, top: Color32, bottom: Color32) {
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(base, base + 1, base + 2);
    mesh.add_triangle(base, base + 2, base + 3);
}

pub fn draw(app: &mut crate::Defalt, ui: &mut Ui, rect: Rect) {
    let active = active(app);
    let calm = app.studio.reduced;
    let spectrum = &app.studio.spectrum;
    let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));

    // The well, with a border that catches the beat.
    p.rect_filled(rect, theme::R_L, theme::tint(theme::BOOTH_GROUND, Color32::BLACK, 0.35));
    let beat = if calm || !active { 0. } else { spectrum.pulse };
    p.rect_stroke(rect, theme::R_L, Stroke::new(theme::LINE, theme::tint(theme::BOOTH_EDGE, theme::AMBER, beat * 0.55)),
                  egui::StrokeKind::Inside);

    let graph = rect.shrink2(vec2(theme::SP_4, theme::SP_2));
    // Bars rise from a line two thirds of the way down; their reflection
    // takes the rest.
    let baseline = graph.top() + graph.height() * 0.7;
    let reach = baseline - graph.top() - 4.;

    if !active {
        idle(ui, &p, graph, baseline, calm);
        p.text(pos2(rect.center().x, graph.top() + theme::SP_2), Align2::CENTER_TOP, "Play here to see the sound",
               FontId::proportional(theme::SIZE_S), theme::TEXT_DIM);
        return;
    }

    let mut mesh = Mesh::default();
    // The bass lights the room behind the bars: a wide soft glow over the
    // low end, and a smaller brighter heart in it.
    if !calm && spectrum.glow > 0.02 {
        let g = spectrum.glow.min(1.);
        let centre = pos2(graph.left() + graph.width() * 0.2, baseline);
        glow(&mut mesh, centre, vec2(graph.width() * (0.3 + 0.25 * g), graph.height() * (0.55 + 0.4 * g)), fade(theme::AMBER, 0.22 * g));
        glow(&mut mesh, centre, vec2(graph.width() * 0.14, graph.height() * 0.4), fade(theme::WINDOW_LIGHT, 0.16 * g));
    }

    let bars = bar_count(graph.width());
    let step = graph.width() / bars as f32;
    let width = (step * 0.62).max(1.5);
    let radius = width / 2.;
    for i in 0..bars {
        let at = (i as f32 + 0.5) / bars as f32;
        let value = sample(&spectrum.levels, at);
        let x = graph.left() + (i as f32 + 0.5) * step;
        let colour = palette(at);
        let height = (value * reach).max(width);
        let top = baseline - height;
        // Body: darker at the floor, bright at the tip, and a rounded tip.
        column(&mut mesh, Rect::from_min_max(pos2(x - radius, top + radius), pos2(x + radius, baseline)),
               theme::tint(colour, Color32::WHITE, 0.2), fade(colour, 0.55));
        if radius >= 1.5 {
            disc(&mut mesh, pos2(x, top + radius), radius, theme::tint(colour, Color32::WHITE, 0.2));
        }
        if !calm {
            // The reflection: the same bar upside down, fading to nothing.
            let depth = (height * 0.45).min(graph.bottom() - baseline - 2.);
            if depth > 1. {
                column(&mut mesh, Rect::from_min_max(pos2(x - radius, baseline + 2.), pos2(x + radius, baseline + 2. + depth)),
                       fade(colour, 0.22), Color32::TRANSPARENT);
            }
            let peak = sample(&spectrum.peaks, at);
            if peak > 0.03 && peak > value + 0.01 {
                let y = baseline - peak * reach;
                column(&mut mesh, Rect::from_min_max(pos2(x - radius, y - 1.), pos2(x + radius, y + 1.)),
                       theme::AMBER_PALE, theme::AMBER_PALE);
            }
        }
    }
    p.add(Shape::mesh(mesh));
    p.line_segment([pos2(graph.left(), baseline + 1.), pos2(graph.right(), baseline + 1.)],
                   Stroke::new(theme::LINE, Color32::from_white_alpha(14)));
    ui.ctx().request_repaint_after(Duration::from_millis(if calm { 100 } else { 33 }));
}

/// Nothing playing: a low wave in the booth's colours, breathing slowly, and
/// its reflection. Still, in reduced motion.
fn idle(ui: &Ui, p: &egui::Painter, graph: Rect, baseline: f32, calm: bool) {
    let t = if calm { 0. } else { ui.input(|i| i.time) as f32 };
    let breath = 3. + 2.2 * (t * 0.55).sin();
    let points = ((graph.width() / 5.) as usize).max(16);
    let wave = |sign: f32, scale: f32| -> Vec<Pos2> {
        (0..=points).map(|i| {
            let u = i as f32 / points as f32;
            let x = graph.left() + u * graph.width();
            // Quiet at the ends so the wave rests on the line rather than
            // stopping mid-swing at the edges.
            let envelope = (std::f32::consts::PI * u).sin();
            let y = (u * 17. + t * 0.8).sin() * 0.6 + (u * 31. - t * 1.3).sin() * 0.4;
            pos2(x, baseline - sign * y * breath * scale * envelope)
        }).collect()
    };
    let colour = |alpha: f32| PathStroke::new_uv(1.5, move |bounds: Rect, at: Pos2| {
        fade(palette((at.x - bounds.left()) / bounds.width().max(1.)), alpha)
    });
    p.add(Shape::line(wave(-1., 0.6), colour(0.18)));
    p.add(Shape::line(wave(1., 1.), colour(0.75)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_multiply_with_width_and_read_the_bands_in_octave_order() {
        assert_eq!(bar_count(100.), 24);
        assert_eq!(bar_count(600.), 66);
        assert_eq!(bar_count(4000.), 96);
        let levels: [f32; BANDS] = std::array::from_fn(|i| i as f32 / (BANDS - 1) as f32);
        assert_eq!(sample(&levels, 0.), 0.);
        assert_eq!(sample(&levels, 1.), 1.);
        assert!((sample(&levels, 0.5) - 0.5).abs() < 1e-6, "between bands reads the line between them");
        let bars = bar_count(900.);
        let read: Vec<f32> = (0..bars).map(|i| sample(&levels, (i as f32 + 0.5) / bars as f32)).collect();
        assert!(read.windows(2).all(|w| w[1] > w[0]), "a rising spectrum must rise across the bars");
    }

    #[test]
    fn the_palette_runs_warm_to_cool() {
        assert_eq!(palette(0.), theme::AMBER);
        assert_eq!(palette(1.), theme::DECK_A);
        assert!(palette(0.).r() > palette(1.).r() && palette(0.).b() < palette(1.).b());
    }

    #[test]
    fn peak_caps_fall_faster_the_longer_they_fall_and_never_below_the_bar() {
        let mut state = State::default();
        let loud = [0.9; BANDS];
        for _ in 0..30 { state.step(&loud, 0.033, false); }
        let top = state.peaks[0];
        let silence = [0.; BANDS];
        let mut drops = Vec::new();
        let mut before = top;
        for _ in 0..8 {
            state.step(&silence, 0.033, false);
            drops.push(before - state.peaks[0]);
            before = state.peaks[0];
            assert!(state.peaks[0] >= state.levels[0]);
        }
        assert!(drops[6] > drops[1] * 2., "the caps did not accelerate: {drops:?}");
    }

    #[test]
    fn a_kick_over_a_quiet_bed_is_a_beat_and_the_glow_follows_the_bass() {
        let mut state = State::default();
        let quiet = [0.1; BANDS];
        for _ in 0..90 { state.step(&quiet, 0.033, false); }
        assert!(state.pulse < 0.1);
        let mut kick = [0.1; BANDS];
        kick[..BANDS / 8].fill(0.9);
        for _ in 0..3 { state.step(&kick, 0.033, false); }
        assert!(state.pulse > 0.5, "the kick did not register: {}", state.pulse);
        assert!(state.glow > 0.2);
        for _ in 0..40 { state.step(&quiet, 0.033, false); }
        assert!(state.pulse < 0.05, "the beat never faded");
    }

    #[test]
    fn reduced_motion_answers_slowly_with_no_caps_glow_or_beat() {
        let mut calm = State::default();
        let mut lively = State::default();
        let mut kick = [0.1; BANDS];
        kick[..BANDS / 8].fill(0.9);
        calm.step(&kick, 0.033, true);
        lively.step(&kick, 0.033, false);
        assert!(calm.levels[0] < lively.levels[0], "reduced motion rose as fast");
        assert_eq!((calm.pulse, calm.glow), (0., 0.));
        calm.step(&[0.; BANDS], 0.033, true);
        assert_eq!(calm.peaks, calm.levels, "a cap was left hanging in reduced motion");
    }
}
