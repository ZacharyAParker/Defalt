//! A quiet spectrum strip that belongs to the radio desk.
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Stroke, Ui};
use std::time::{Duration, Instant};
use crate::engine::visualizer::{analyze, BANDS};
use super::theme;

pub struct State {
    levels: [f32; BANDS],
    peaks: [f32; BANDS],
    last: Instant,
}
impl Default for State {
    fn default() -> Self { Self { levels: [0.; BANDS], peaks: [0.; BANDS], last: Instant::now() } }
}

pub fn draw(app: &mut crate::Defalt, ui: &mut Ui, rect: Rect) {
    let now = Instant::now();
    let dt = now.duration_since(app.studio.spectrum.last).as_secs_f32().min(0.2);
    let active = app.airtime.on && app.engine.is_some();
    let calm = app.studio.reduced;
    let interval = if calm { 100 } else { 33 };
    if dt >= interval as f32 / 1000. {
        let target = if active {
            let engine = app.engine.as_ref().unwrap();
            analyze(&engine.telemetry.visualizer.snapshot(), engine.sample_rate)
        } else { [0.; BANDS] };
        let state = &mut app.studio.spectrum;
        for (i, target) in target.into_iter().enumerate() {
            let response = if calm { 0.3 } else if target > state.levels[i] { 0.045 } else { 0.24 };
            state.levels[i] += (target - state.levels[i]) * (1. - (-dt / response).exp());
            state.peaks[i] = state.levels[i].max(state.peaks[i] - dt * 0.6);
        }
        state.last = now;
    }
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, 4., theme::GROUND);
    let graph = rect.shrink2(vec2(18., 12.));
    let baseline = graph.bottom() - 8.;
    let step = graph.width() / BANDS as f32;
    let width = (step * 0.55).clamp(2., 10.);
    for i in 0..BANDS {
        let value = app.studio.spectrum.levels[i];
        let x = graph.left() + (i as f32 + 0.5) * step;
        let height = (value * (graph.height() - 10.)).max(2.);
        let t = i as f32 / (BANDS - 1) as f32;
        let color = Color32::from_rgb((239. - t * 150.) as u8, (189. + t * 13.) as u8, (113. + t * 111.) as u8);
        p.rect_filled(Rect::from_min_max(pos2(x - width * 0.5, baseline - height), pos2(x + width * 0.5, baseline)), 1.5, color.gamma_multiply(0.85));
        if !calm { p.rect_filled(Rect::from_min_max(pos2(x - width * 0.5, baseline + 3.), pos2(x + width * 0.5, baseline + 3. + height * 0.12)), 1., color.gamma_multiply(0.16)); }
        if !calm && value > 0.035 {
            let top = baseline - app.studio.spectrum.peaks[i] * (graph.height() - 10.);
            p.line_segment([pos2(x - width * 0.5, top), pos2(x + width * 0.5, top)], Stroke::new(1., Color32::from_rgb(245, 224, 190)));
        }
    }
    if !active {
        p.text(rect.center() - vec2(0., 8.), Align2::CENTER_CENTER, "Play here to see the sound", FontId::proportional(11.), theme::TEXT_DIM);
    }
    if active { ui.ctx().request_repaint_after(Duration::from_millis(interval)); }
}
