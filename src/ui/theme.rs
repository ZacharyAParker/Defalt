//! The palette, the type and the measurements.
//!
//! Graphite rather than navy: near-black neutral surfaces, so the only
//! colour on the panel is colour that means something. Each deck has its own
//! -- A cyan, B violet, always with its letter beside it -- and those two are
//! how you tell which side a thing belongs to without reading. Green is
//! playing, amber is cue and the station's own go button, red is too loud.
//!
//! One deliberate exception: the playhead is warm. It is the single mark you
//! must find instantly against a field of waveform, and it is the only
//! orange-red on the panel, so nothing else is mistaken for it.

use std::sync::{Arc, OnceLock};

use egui::{
    Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals,
};

/* ── Surfaces ────────────────────────────────────────────────────────── */

pub const GROUND: Color32 = Color32::from_rgb(0x0c, 0x0f, 0x14);
pub const PANEL: Color32 = Color32::from_rgb(0x17, 0x1c, 0x24);
pub const RAISED: Color32 = Color32::from_rgb(0x24, 0x2d, 0x3a);
pub const RAISED_HI: Color32 = Color32::from_rgb(0x30, 0x3b, 0x4b);
/// Every other row of a table, a shade off the panel.
pub const STRIPE: Color32 = Color32::from_rgb(0x1a, 0x20, 0x29);
pub const WELL: Color32 = Color32::from_rgb(0x07, 0x09, 0x0c);
pub const EDGE: Color32 = Color32::from_rgb(0x2b, 0x34, 0x42);
pub const EDGE_LIT: Color32 = Color32::from_rgb(0x46, 0x53, 0x68);

/// The radio view's own room: the same graphite, warmed towards the booth's
/// lamplight so the artwork does not sit in a cold frame.
pub const BOOTH_GROUND: Color32 = Color32::from_rgb(0x14, 0x13, 0x19);
pub const BOOTH_PANEL: Color32 = Color32::from_rgb(0x1c, 0x1a, 0x22);
pub const BOOTH_EDGE: Color32 = Color32::from_rgb(0x33, 0x2f, 0x3a);

/* ── Ink ─────────────────────────────────────────────────────────────── */

pub const TEXT: Color32 = Color32::from_rgb(0xe7, 0xeb, 0xf1);
pub const TEXT_BRIGHT: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xb8, 0xc2, 0xd1);
/// Captions and resting labels. Clearly a step down from `TEXT_DIM`, but
/// still 4.5:1 on every surface it is drawn on, raised buttons included,
/// because captions are still read.
pub const TEXT_MUTE: Color32 = Color32::from_rgb(0x9a, 0xa8, 0xbc);

/* ── Signal colours ──────────────────────────────────────────────────── */

/// The panel's own accent: focus, selection, the station at work. Never a
/// deck's colour, so a focused control is not mistaken for a side.
pub const BLUE: Color32 = Color32::from_rgb(0x6a, 0xa6, 0xff);
/// Selection: a selected row, the source in use. Dark enough that muted
/// captions on it still read.
pub const BLUE_DEEP: Color32 = Color32::from_rgb(0x1c, 0x37, 0x60);
/// Information that is good news: a finished job, speech ducking the music,
/// the incoming side of a planned mix.
pub const CYAN: Color32 = Color32::from_rgb(0x4f, 0xd6, 0xc0);
/// Playing, and a match close enough to mix.
pub const GREEN: Color32 = Color32::from_rgb(0x3d, 0xdc, 0x84);
/// Kept warm on purpose, and used for nothing else.
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xff, 0x5a, 0x3c);
pub const RED: Color32 = Color32::from_rgb(0xff, 0x44, 0x68);
/// Something worth reading twice that is not yet an error: a limit about to
/// be crossed, a message that failed to send.
pub const WARN: Color32 = Color32::from_rgb(0xf5, 0xb4, 0x82);
/// The booth's own light, and the one primary action on each side of the
/// panel: CUE on a deck, going on air in the radio view.
pub const AMBER: Color32 = Color32::from_rgb(0xef, 0xbd, 0x71);
pub const AMBER_PALE: Color32 = Color32::from_rgb(0xf5, 0xe0, 0xbe);
pub const VINYL: Color32 = Color32::from_rgb(0x13, 0x12, 0x18);
pub const VINYL_GROOVE: Color32 = Color32::from_rgb(0x31, 0x2c, 0x37);
pub const WINDOW_LIGHT: Color32 = Color32::from_rgb(0xff, 0xc2, 0x6d);
pub const RAIN: Color32 = Color32::from_rgb(0xad, 0xc0, 0xdc);
pub const SNORE: Color32 = Color32::from_rgb(0xcc, 0xbf, 0xc8);

/// Each deck's colour. Never used alone: wherever one appears, the deck's
/// letter is beside it, so the two sides are told apart without telling
/// cyan from violet.
pub const DECK_A: Color32 = Color32::from_rgb(0x6b, 0xc5, 0xee);
pub const DECK_B: Color32 = Color32::from_rgb(0xc7, 0xa0, 0xff);
pub const DECK_COLOURS: [Color32; 2] = [DECK_A, DECK_B];
pub const DECK_LETTERS: [&str; 2] = ["A", "B"];

/// Cue points, C1 to C4. Distinct from each other, from the playhead and
/// from the waveform, so a flag reads as a cue rather than as a loud moment.
pub const CUE_COLOURS: [Color32; 4] = [
    Color32::from_rgb(0xff, 0x6b, 0x9a),
    Color32::from_rgb(0x4c, 0xe0, 0x7a),
    Color32::from_rgb(0xf2, 0xd2, 0x3c),
    Color32::from_rgb(0x5b, 0x8c, 0xff),
];

/// The three-band waveform, as the reference hardware draws it: bass a deep
/// blue at full height, mids amber inside it, highs white on top.
pub const WAVE_LOW: Color32 = Color32::from_rgb(0x1f, 0x4f, 0xe6);
pub const WAVE_MID: Color32 = Color32::from_rgb(0xf2, 0xa2, 0x32);
pub const WAVE_HIGH: Color32 = Color32::from_rgb(0xf4, 0xf6, 0xff);

/* ── Hardware ────────────────────────────────────────────────────────── */

pub const SLOT: Color32 = Color32::from_rgb(0x04, 0x05, 0x07);
pub const CAP: Color32 = Color32::from_rgb(0xb4, 0xbc, 0xc8);
pub const CAP_LINE: Color32 = Color32::from_rgb(0x2a, 0x30, 0x3a);

pub const KNOB_BODY: Color32 = Color32::from_rgb(0x1a, 0x20, 0x29);
pub const KNOB_SHEEN: Color32 = Color32::from_rgb(0x27, 0x2f, 0x3b);

pub const PLATTER_EDGE: Color32 = Color32::from_rgb(0x05, 0x06, 0x09);
pub const PLATTER_BODY: Color32 = Color32::from_rgb(0x12, 0x15, 0x1b);
pub const PLATTER_SHEEN: Color32 = Color32::from_rgb(0x1a, 0x1f, 0x27);
pub const STROBE: Color32 = Color32::from_rgb(0x22, 0x28, 0x32);
pub const STROBE_LIT: Color32 = Color32::from_rgb(0x55, 0x61, 0x75);

/* ── Measurements ────────────────────────────────────────────────────── */

/// The type scale. Nothing on the panel is set smaller than `SIZE_XS`, which
/// is the smallest a caption can be and still be read at arm's length.
pub const SIZE_XS: f32 = 12.0;
pub const SIZE_S: f32 = 13.0;
pub const SIZE_M: f32 = 14.0;
pub const SIZE_L: f32 = 18.0;
pub const SIZE_XL: f32 = 22.0;
pub const SIZE_XXL: f32 = 28.0;

/// Corners: small parts (caps, flags, lamps), controls and plates, and the
/// things that float (popups, notices, cards).
pub const R_S: f32 = 3.0;
pub const R_M: f32 = 6.0;
pub const R_L: f32 = 10.0;

/// Strokes: hairlines, marks that must be seen, and marks that must be found.
pub const LINE: f32 = 1.0;
pub const LINE_MID: f32 = 1.5;
pub const LINE_BOLD: f32 = 2.0;

/// The spacing scale.
pub const SP_1: f32 = 4.0;
pub const SP_2: f32 = 8.0;
pub const SP_3: f32 = 12.0;
pub const SP_4: f32 = 16.0;
pub const SP_5: f32 = 24.0;

/// Control heights. Every button on the panel is one of these two, save the
/// radio's single go button.
pub const CONTROL_S: f32 = 24.0;
pub const CONTROL_M: f32 = 32.0;
pub const CONTROL_L: f32 = 40.0;

/* ── Type ────────────────────────────────────────────────────────────── */

/// The same families as the browser player, so the two read as one product:
/// Archivo for words, its semi-condensed semibold for titles and the brand,
/// IBM Plex Mono wherever a number is read as a measurement -- which on a
/// mixer is nearly every number there is. Plex is monospaced, so every
/// readout is tabular and a changing time never jitters sideways.
const ARCHIVO: &[u8] = include_bytes!("../../assets/fonts/Archivo-Regular.ttf");
const ARCHIVO_TITLE: &[u8] = include_bytes!("../../assets/fonts/ArchivoSemiCondensed-SemiBold.ttf");
const PLEX: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf");
const PLEX_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Medium.ttf");

const DISPLAY_FAMILY: &str = "display";
const READOUT_FAMILY: &str = "readout";

fn family(name: &'static str, cell: &'static OnceLock<FontFamily>) -> FontFamily {
    cell.get_or_init(|| FontFamily::Name(Arc::from(name))).clone()
}

/// Titles, deck names and the brand.
pub fn display(size: f32) -> FontId {
    static CELL: OnceLock<FontFamily> = OnceLock::new();
    FontId::new(size, family(DISPLAY_FAMILY, &CELL))
}

/// Big numbers: the platter's tempo, the time left.
pub fn readout(size: f32) -> FontId {
    static CELL: OnceLock<FontFamily> = OnceLock::new();
    FontId::new(size, family(READOUT_FAMILY, &CELL))
}

pub fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in [
        ("archivo", ARCHIVO),
        ("archivo-title", ARCHIVO_TITLE),
        ("plex", PLEX),
        ("plex-medium", PLEX_MEDIUM),
    ] {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    // egui's own fonts stay behind ours, for the symbols ours do not carry.
    let fallback = |family: FontFamily| fonts.families.get(&family).cloned().unwrap_or_default();
    let proportional = fallback(FontFamily::Proportional);
    let monospace = fallback(FontFamily::Monospace);
    let chain = |first: &str, rest: &[String]| {
        std::iter::once(first.to_string()).chain(rest.iter().cloned()).collect::<Vec<_>>()
    };
    fonts.families.insert(FontFamily::Proportional, chain("archivo", &proportional));
    fonts.families.insert(FontFamily::Monospace, chain("plex", &monospace));
    fonts.families.insert(FontFamily::Name(DISPLAY_FAMILY.into()), chain("archivo-title", &proportional));
    fonts.families.insert(FontFamily::Name(READOUT_FAMILY.into()), chain("plex-medium", &monospace));
    fonts
}

pub fn apply(ctx: &Context) {
    ctx.set_fonts(fonts());

    let mut visuals = Visuals::dark();

    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = GROUND;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = WELL;
    visuals.faint_bg_color = STRIPE;
    visuals.selection.bg_fill = BLUE_DEEP;
    visuals.selection.stroke = Stroke::new(LINE, BLUE);
    visuals.hyperlink_color = BLUE;
    visuals.text_cursor.stroke = Stroke::new(LINE_BOLD, BLUE);

    // egui's default chrome is rounded, floaty and mid-grey. Everything here
    // is squarer, darker and flatter: a panel of equipment, not a dialog.
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(R_M as u8);
        widget.bg_fill = RAISED;
        widget.weak_bg_fill = RAISED;
        widget.bg_stroke = Stroke::new(LINE, EDGE);
        widget.fg_stroke = Stroke::new(LINE, TEXT_DIM);
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.weak_bg_fill = PANEL;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(LINE, TEXT_DIM);
    visuals.widgets.hovered.weak_bg_fill = RAISED_HI;
    visuals.widgets.hovered.bg_stroke = Stroke::new(LINE, EDGE_LIT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(LINE, TEXT);
    visuals.widgets.active.bg_fill = RAISED_HI;
    visuals.widgets.active.weak_bg_fill = RAISED_HI;
    visuals.widgets.active.bg_stroke = Stroke::new(LINE, BLUE);
    visuals.widgets.active.fg_stroke = Stroke::new(LINE, TEXT_BRIGHT);

    visuals.window_stroke = Stroke::new(LINE, EDGE);
    visuals.window_corner_radius = CornerRadius::same(R_L as u8);
    visuals.menu_corner_radius = CornerRadius::same(R_M as u8);
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 22,
        spread: 0,
        color: Color32::from_black_alpha(200),
    };
    visuals.window_shadow = visuals.popup_shadow;

    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(SP_2 - 2.0, SP_1);
        style.spacing.button_padding = egui::vec2(SP_2, SP_1);
        style.spacing.window_margin = egui::Margin::same(SP_3 as i8);
        style.spacing.scroll.bar_width = 9.0;
        style.spacing.interact_size.y = CONTROL_S;

        style.text_styles = [
            (TextStyle::Heading, display(SIZE_L)),
            (TextStyle::Body, FontId::new(SIZE_M, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(SIZE_S, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(SIZE_S, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(SIZE_XS, FontFamily::Proportional)),
        ]
        .into();
    });
}

/// A deck's colour laid over a surface at `share`, as a solid colour: the
/// tint a lit control or a deck's name strip is filled with.
pub fn tint(surface: Color32, colour: Color32, share: f32) -> Color32 {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * share).round() as u8;
    Color32::from_rgb(mix(surface.r(), colour.r()), mix(surface.g(), colour.g()), mix(surface.b(), colour.b()))
}

/// How a lit control is filled: its accent at about a third over a raised
/// button, so white type on it still reads.
pub fn lit_fill(accent: Color32) -> Color32 {
    tint(RAISED, accent, 0.30)
}

/// Where a bucket's energy sits, as a colour.
///
/// Coloured by content, not by which deck it is on: bass deep indigo, mids
/// blue, highs pale cyan. The colour tells you which part of the record you
/// are looking at, which is what you need when you are deciding where to
/// bring the next one in.
pub fn band_colour(low: f32, mid: f32, high: f32) -> Color32 {
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

/// Relative luminance, as WCAG defines it.
#[cfg(test)]
fn luminance(colour: Color32) -> f32 {
    let channel = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.039_28 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel(colour.r()) + 0.7152 * channel(colour.g()) + 0.0722 * channel(colour.b())
}

#[cfg(test)]
pub(crate) fn contrast(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_least(ink: Color32, grounds: &[Color32], ratio: f32, what: &str) {
        for ground in grounds {
            let got = contrast(ink, *ground);
            assert!(got >= ratio, "{what} {ink:?} on {ground:?} is {got:.2}:1, wants {ratio}:1");
        }
    }

    #[test]
    fn captions_stay_readable_on_every_ground_they_sit_on() {
        // Every surface text is drawn on, raised buttons included: a caption
        // on a button is still a caption.
        let grounds = [GROUND, PANEL, STRIPE, RAISED, RAISED_HI, WELL, BOOTH_GROUND, BOOTH_PANEL, BLUE_DEEP];
        for (ink, name) in [(TEXT, "text"), (TEXT_DIM, "dim"), (TEXT_MUTE, "mute"), (WARN, "warn"),
                            (BLUE, "blue"), (CYAN, "cyan"), (GREEN, "green"), (AMBER, "amber"),
                            (DECK_A, "deck A"), (DECK_B, "deck B")] {
            at_least(ink, &grounds, 4.5, name);
        }
        // Errors are set on the flat grounds, never on a button.
        at_least(RED, &[GROUND, PANEL, WELL, BOOTH_GROUND, BOOTH_PANEL], 4.5, "red");
    }

    #[test]
    fn type_on_a_lit_or_primary_control_still_reads() {
        for accent in [BLUE, DECK_A, DECK_B, AMBER, GREEN, CYAN, RED] {
            at_least(TEXT_BRIGHT, &[lit_fill(accent)], 4.5, "lit");
        }
        for set in CUE_COLOURS {
            at_least(TEXT_BRIGHT, &[lit_fill(set)], 4.5, "cue pad");
        }
        // The primary buttons carry dark ink on their accent.
        at_least(VINYL, &[AMBER, GREEN], 4.5, "primary");
        at_least(GROUND, &[DECK_A, DECK_B, GREEN], 4.5, "badge");
    }

    #[test]
    fn the_playhead_is_the_only_warm_red_on_the_panel() {
        // Distinct from every cue flag, the CUE button's amber and the clip
        // red: the one mark that must never be mistaken for another.
        let distance = |a: Color32, b: Color32| {
            let d = |x: u8, y: u8| (x as f32 - y as f32).powi(2);
            (d(a.r(), b.r()) + d(a.g(), b.g()) + d(a.b(), b.b())).sqrt()
        };
        for other in CUE_COLOURS.into_iter().chain([AMBER, RED, WARN]) {
            assert!(distance(PLAYHEAD, other) > 45.0, "{other:?} is too close to the playhead");
        }
    }

    #[test]
    fn muted_text_is_visibly_a_step_below_dim_text() {
        assert!(luminance(TEXT_DIM) / luminance(TEXT_MUTE) > 1.35);
    }

    #[test]
    fn the_bundled_fonts_parse_and_cover_the_panel_s_symbols() {
        let ctx = Context::default();
        apply(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            for font in [display(SIZE_L), readout(SIZE_XXL), FontId::proportional(SIZE_M), FontId::monospace(SIZE_S)] {
                let galley = ui.painter().layout_no_wrap("DEFALT 128.0 ÷2 ×2 −∞ ·".into(), font, TEXT);
                assert!(galley.size().x > 20.0);
            }
        }).drop_without_applying_deltas();
    }
}
