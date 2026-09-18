//! The palette.
//!
//! Black and navy. The ground is black with a blue cast rather than neutral
//! grey, panels are navy, and everything that lights up is a brighter blue.
//! The reference's shapes and densities are kept exactly; only the colour is
//! ours.
//!
//! One deliberate exception: the playhead is warm. It is the single mark you
//! must find instantly against a field of blue waveform, and a blue line on
//! blue is a line you hunt for.

use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

pub const GROUND: Color32 = Color32::from_rgb(0x05, 0x07, 0x0d);
pub const PANEL: Color32 = Color32::from_rgb(0x13, 0x1b, 0x29);
pub const RAISED: Color32 = Color32::from_rgb(0x20, 0x2d, 0x40);
pub const RAISED_HI: Color32 = Color32::from_rgb(0x2c, 0x3e, 0x56);
pub const WELL: Color32 = Color32::from_rgb(0x02, 0x04, 0x0a);
pub const EDGE: Color32 = Color32::from_rgb(0x2c, 0x39, 0x4c);
pub const EDGE_LIT: Color32 = Color32::from_rgb(0x3e, 0x55, 0x82);

pub const TEXT: Color32 = Color32::from_rgb(0xe4, 0xea, 0xf6);
pub const TEXT_BRIGHT: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x93, 0xa3, 0xc0);
pub const TEXT_MUTE: Color32 = Color32::from_rgb(0x94, 0xa3, 0xbc);

/// The accent: what is selected, on, or being touched.
pub const BLUE: Color32 = Color32::from_rgb(0x4d, 0x9f, 0xff);
pub const BLUE_DEEP: Color32 = Color32::from_rgb(0x1e, 0x5f, 0xb8);
pub const CYAN: Color32 = Color32::from_rgb(0x3c, 0xd2, 0xe6);
/// Kept warm on purpose, and used for nothing else.
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xff, 0x5a, 0x3c);
pub const RED: Color32 = Color32::from_rgb(0xff, 0x45, 0x3a);

pub const SLOT: Color32 = Color32::from_rgb(0x02, 0x04, 0x09);
pub const CAP: Color32 = Color32::from_rgb(0x8f, 0x9d, 0xb8);
pub const CAP_LINE: Color32 = Color32::from_rgb(0x2a, 0x37, 0x52);

pub const KNOB_BODY: Color32 = Color32::from_rgb(0x12, 0x1b, 0x2e);
pub const KNOB_SHEEN: Color32 = Color32::from_rgb(0x1a, 0x27, 0x42);

pub const PLATTER_EDGE: Color32 = Color32::from_rgb(0x04, 0x07, 0x0f);
pub const PLATTER_BODY: Color32 = Color32::from_rgb(0x0f, 0x17, 0x28);
pub const PLATTER_SHEEN: Color32 = Color32::from_rgb(0x17, 0x22, 0x3a);
pub const PLATTER_MARK: Color32 = Color32::from_rgb(0x54, 0x68, 0x92);
pub const STROBE: Color32 = Color32::from_rgb(0x1c, 0x28, 0x40);
pub const STROBE_LIT: Color32 = Color32::from_rgb(0x4f, 0x67, 0x93);

/// A deck's own colour, for the few places a thing must be attributed to one
/// deck. Waveforms are coloured by frequency instead, as the reference does
/// it, so the lanes are told apart by content rather than by tint.
pub const DECK_COLOURS: [Color32; 2] = [BLUE, CYAN];

pub fn apply(ctx: &Context) {
    let mut visuals = Visuals::dark();

    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = GROUND;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = WELL;
    visuals.faint_bg_color = PANEL;
    visuals.selection.bg_fill = Color32::from_rgb(0x14, 0x33, 0x60);
    visuals.selection.stroke = Stroke::new(1.0, BLUE);
    visuals.hyperlink_color = BLUE;

    // egui's default chrome is rounded, floaty and mid-grey. Everything here
    // is squarer, darker and flatter: a panel of equipment, not a dialog.
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(5);
        widget.bg_fill = RAISED;
        widget.weak_bg_fill = PANEL;
        widget.bg_stroke = Stroke::new(1.0, EDGE);
        widget.fg_stroke = Stroke::new(1.0, TEXT_DIM);
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, EDGE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, EDGE_LIT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.active.bg_fill = RAISED_HI;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, BLUE);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_BRIGHT);

    visuals.window_stroke = Stroke::new(1.0, EDGE);
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 22,
        spread: 0,
        color: Color32::from_black_alpha(200),
    };
    visuals.window_shadow = visuals.popup_shadow;

    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.window_margin = egui::Margin::same(8);
        style.spacing.scroll.bar_width = 9.0;
        style.spacing.interact_size.y = 24.0;

        // Compact sans for labels, monospace wherever a number is read as a
        // measurement -- which on a mixer is nearly every number there is.
        style.text_styles = [
            (TextStyle::Heading, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(11.0, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(9.5, FontFamily::Monospace)),
        ]
        .into();
    });
}

/// Where a bucket's energy sits, as a colour.
///
/// Coloured by content, not by which deck it is on: bass deep indigo, mids
/// blue, highs pale cyan. The colour tells you which part of the record you
/// are looking at, which is what you need when you are deciding where to
/// bring the next one in.
pub fn band_colour(low: f32, mid: f32, high: f32, _deck: Color32) -> Color32 {
    let energy = low + mid + high;
    if energy <= f32::EPSILON {
        return EDGE;
    }
    let (low, mid, high) = (low / energy, mid / energy, high / energy);

    // Mixed additively, so a full-range moment comes out pale rather than
    // resolving to whichever band happened to win.
    let r = low * 70.0 + mid * 70.0 + high * 165.0;
    let g = low * 95.0 + mid * 160.0 + high * 235.0;
    let b = low * 215.0 + mid * 255.0 + high * 255.0;

    let boost = 1.15;
    Color32::from_rgb(
        (r * boost).clamp(0.0, 255.0) as u8,
        (g * boost).clamp(0.0, 255.0) as u8,
        (b * boost).clamp(0.0, 255.0) as u8,
    )
}
