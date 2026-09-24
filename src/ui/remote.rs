//! The toolbar's remote indicator: whether the tunnel is up, and who is
//! listening through it. Shown only once remote listening is set up (or
//! somebody is on the stream anyway), so a console without it looks as it
//! always did.

use egui::{vec2, Align2, FontId, Sense, Stroke, Ui};

use super::theme;
use crate::tunnel::TunnelState;
use crate::Defalt;

/// The words and the lamp colour for a tunnel state and a listener count.
pub fn describe(state: &TunnelState, listeners: usize) -> (String, egui::Color32) {
    let (word, colour) = match state {
        TunnelState::Online(_) => ("online", theme::CYAN),
        TunnelState::Connecting => ("connecting", theme::AMBER),
        TunnelState::Off => ("off", theme::TEXT_MUTE),
    };
    let mut text = format!("Remote: {word}");
    if listeners > 0 {
        text.push_str(&format!(" \u{00b7} {listeners}"));
    }
    (text, colour)
}

pub fn indicator(app: &Defalt, ui: &mut Ui) {
    let listeners = app.remote.listeners();
    let configured = app.remote.configured();
    if configured.is_err() && listeners == 0 {
        return;
    }
    let state = app.remote.tunnel_state();
    let (text, colour) = describe(&state, listeners);
    let width = 28.0 + ui.painter().layout_no_wrap(text.clone(), FontId::proportional(theme::SIZE_S), theme::TEXT_DIM).size().x;
    let (rect, response) = ui.allocate_exact_size(vec2(width, theme::CONTROL_S), Sense::hover());
    ui.painter().rect_stroke(rect, theme::R_M, Stroke::new(theme::LINE, theme::EDGE), egui::StrokeKind::Inside);
    ui.painter().circle_filled(egui::pos2(rect.left() + 11.0, rect.center().y), 3.5, colour);
    ui.painter().text(egui::pos2(rect.left() + 20.0, rect.center().y), Align2::LEFT_CENTER, &text,
                      FontId::proportional(theme::SIZE_S), theme::TEXT_DIM);
    let detail = match (&state, configured) {
        (_, Err(why)) if !why.is_empty() => format!("Remote listening is not set up: {why}"),
        (TunnelState::Online(n), _) => format!("The tunnel is up ({n} edge connections). {listeners} listening."),
        (TunnelState::Connecting, _) => "Reaching Cloudflare...".to_string(),
        (TunnelState::Off, _) => "The tunnel runs while the radio is on. See docs/REMOTE.md.".to_string(),
    };
    super::hint(response, &detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_indicator_says_the_state_and_counts_listeners() {
        assert_eq!(describe(&TunnelState::Online(4), 2).0, "Remote: online \u{00b7} 2");
        assert_eq!(describe(&TunnelState::Connecting, 0).0, "Remote: connecting");
        assert_eq!(describe(&TunnelState::Off, 0).1, theme::TEXT_MUTE);
    }
}
