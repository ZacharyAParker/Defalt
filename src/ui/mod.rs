//! The console.
//!
//! Eight fixed bands, top to bottom: toolbar, overview strip, deck row,
//! transport, beatgrid, FX, stems, browser. Everything except the deck row
//! and the browser is a fixed height, which is what stops a dense panel
//! reflowing under your hands when the window is resized mid-mix.

pub mod about;
pub mod browser;
pub mod decks;
pub mod racks;
pub mod remote;
pub mod radio;
pub mod studio;
pub mod theme;
pub mod toolbar;
pub mod waveform;
pub mod widgets;
pub mod visualizer;
pub mod director_chat;
pub mod feedback;
pub mod transition_preview;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use egui::{vec2, Align, Color32, FontId, Layout, Rect, Response, RichText, Sense, Stroke, Ui};

use crate::Defalt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Column {
    Title,
    Artist,
    Duration,
    Bpm,
    Key,
    /// How little work the transition out of what is playing would be.
    Match,
}

/// Said once, on every control that is drawn but has nothing behind it yet.
/// Drawing them keeps the panel honest about its shape; dimming them keeps it
/// honest about what it can actually do.
pub const NOT_WIRED: &str = "Not wired up yet.";

/// How the waveforms are coloured.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum WaveMode {
    /// One colour per column, blended from where its energy sits.
    Blend,
    /// Bass, mids and highs stacked in their own colours, as the reference
    /// hardware draws them. The default: a kick and a hi-hat look like
    /// different things, which is what you read a waveform for.
    #[default]
    ThreeBand,
}

/// Everything the panel keeps between frames that is the panel's business
/// and nobody else's: caches, and the state of a few of its own controls.
#[derive(Default)]
pub struct ViewState {
    /// Bumped whenever `records` is replaced, so the crate knows its cached
    /// rows are stale.
    pub records_generation: u64,
    crate_rows: RefCell<CrateRows>,
    pub wave_mode: WaveMode,
    /// Planned mix windows per deck, worked out once a frame.
    pub windows: [Vec<crate::airtime::MixWindow>; crate::DECKS],
    pub fx: [racks::Echo; crate::DECKS],
    /// Mix settings edited but not yet applied.
    pub mix_dirty: bool,
    /// The settings window was closed with edits still in it.
    pub mix_confirm_close: bool,
    /// How much of the console the library takes, and whether it is folded
    /// away to its header.
    pub library: Library,
    /// The height the bands and the library share, as of the last frame:
    /// what the splitter measures a drag against.
    pub console_height: f32,
    /// Synced lyrics and sections per record, read from the station's
    /// database in the background.
    pub lyrics: crate::lyrics::Cache,
    /// The lyric line under each deck's title, switched off.
    pub hide_lyrics: bool,
}

/// The library's share of the console, remembered between launches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Library {
    /// Of the height below the toolbar, the part the library takes.
    pub share: f32,
    /// Folded down to its header row, so the decks have the window.
    pub collapsed: bool,
    loaded: bool,
}

impl Default for Library {
    fn default() -> Self {
        Library { share: Library::SHARE, collapsed: false, loaded: false }
    }
}

impl Library {
    /// A little under two fifths: the decks and waveforms get the larger
    /// part, and the crate still shows a screenful of records.
    pub const SHARE: f32 = 0.39;
    pub const LEAST: f32 = 0.15;
    pub const MOST: f32 = 0.75;

    fn path(root: &std::path::Path) -> std::path::PathBuf {
        root.join("cache").join("console-layout.json")
    }

    pub fn load(root: &std::path::Path) -> Self {
        let saved = std::fs::read(Self::path(root)).ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .unwrap_or_default();
        Library {
            share: saved["library_share"].as_f64().map_or(Self::SHARE, |s| s as f32).clamp(Self::LEAST, Self::MOST),
            collapsed: saved["library_collapsed"].as_bool().unwrap_or(false),
            loaded: true,
        }
    }

    pub fn save(&self, root: &std::path::Path) {
        let path = Self::path(root);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let data = serde_json::json!({"library_share": self.share, "library_collapsed": self.collapsed});
        let _ = std::fs::write(path, data.to_string());
    }

    pub fn set_share(&mut self, share: f32) {
        self.share = share.clamp(Self::LEAST, Self::MOST);
    }
}

/// Fold the library away or bring it back, and remember which.
pub fn toggle_library(app: &mut Defalt) {
    app.view_state.library.collapsed = !app.view_state.library.collapsed;
    app.view_state.library.save(&app.root);
}

pub fn draw(app: &mut Defalt, ui: &mut Ui) {
    if let Some(engine) = &app.engine {
        engine.telemetry.visualizer.set_enabled(app.view == crate::View::Radio
            && (app.studio.visualizer || (app.studio.enabled && !app.studio.reduced))
            && app.airtime.on);
    }
    let ctx = ui.ctx().clone();
    // The booth's artwork is large; it decodes on a worker from the first
    // frame so it is ready by the time anyone opens the radio view.
    app.studio.preload(&ctx);
    let reading = app.info_page.is_some();
    about::footer(app, ui);

    egui::Panel::top("toolbar")
        .exact_size(42.0)
        .frame(band(theme::PANEL, 0.0))
        .show_separator_line(false)
        .show(ui, |ui| toolbar::draw(app, ui));

    if app.view == crate::View::Radio {
        crate::keys::release_bends(app);
        egui::CentralPanel::no_frame()
            .frame(band(theme::BOOTH_PANEL, 0.0))
            .show(ui, |ui| radio::draw(app, ui));
        notice(app, &ctx);
        help_overlay(app, &ctx);
        about::overlay(app, &ctx);
        return;
    }

    // Once a frame, not once per place that draws them.
    app.view_state.windows = std::array::from_fn(|deck| app.airtime.transition_windows(deck));

    // Every band above the browser is a fixed height and the browser takes
    // whatever is left. The other way round -- decks flexible, browser fixed
    // -- stretches the decks into a void on any screen larger than a laptop,
    // and a stretched mixer is a sparse one. On a short window the deck row
    // and overview give up some height first, so the crate never vanishes.
    if !app.view_state.library.loaded {
        app.view_state.library = Library::load(&app.root);
    }
    let height = ui.available_height();
    app.view_state.console_height = height;
    let bands = Bands::fit(height, app.show_grid, app.show_fx, app.show_stems, app.view_state.library);

    egui::Panel::top("overview")
        .exact_size(bands.overview)
        .frame(band(theme::GROUND, 4.0))
        .show_separator_line(false)
        .show(ui, |ui| decks::overview_strip(app, ui));

    egui::Panel::top("decks")
        .exact_size(bands.decks)
        .frame(band(theme::GROUND, 4.0))
        .show_separator_line(false)
        .show(ui, |ui| decks::deck_row(app, ui));

    egui::Panel::top("transport")
        .exact_size(44.0)
        .frame(band(theme::GROUND, 4.0))
        .show_separator_line(false)
        .show(ui, |ui| decks::transport(app, ui));

    if app.show_grid {
        egui::Panel::top("beatgrid")
            .exact_size(46.0)
            .frame(band(theme::GROUND, 4.0))
            .show_separator_line(false)
            .show(ui, |ui| racks::beatgrid(app, ui));
    }

    if app.show_fx {
        egui::Panel::top("fx")
            .exact_size(58.0)
            .frame(band(theme::GROUND, 4.0))
            .show_separator_line(false)
            .show(ui, |ui| racks::effects(app, ui));
    }

    if app.show_stems {
        egui::Panel::top("stems")
            .exact_size(62.0)
            .frame(band(theme::GROUND, 4.0))
            .show_separator_line(false)
            .show(ui, |ui| racks::stems(app, ui));
    }

    egui::CentralPanel::no_frame()
        .frame(band(theme::PANEL, 0.0))
        .show(ui, |ui| browser::draw(app, ui));

    notice(app, &ctx);
    help_overlay(app, &ctx);
    if !reading && app.info_page.is_none() {
        crate::keys::handle(app, &ctx);
    }
    about::overlay(app, &ctx);
}

/// The deck row at its old fixed height: a 200px jog with its pitch column
/// beside it. It grows past this when the library is made smaller.
#[cfg(test)]
pub const DECK_ROW: f32 = 256.0;

/// The heights of the bands and of the library, for a window of a given
/// height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bands {
    pub overview: f32,
    pub decks: f32,
    pub library: f32,
}

impl Bands {
    /// The least an open library is left with before the decks give way.
    const CRATE_LEAST: f32 = 120.0;
    /// The shortest deck row that still fits the three EQ knobs in a column.
    const DECKS_LEAST: f32 = 216.0;
    const DECKS_MOST: f32 = 440.0;
    const OVERVIEW_LEAST: f32 = 64.0;
    const OVERVIEW_MOST: f32 = 132.0;
    /// A folded library: the splitter and one header row.
    pub const FOLDED: f32 = browser::SPLITTER + 44.0;

    pub fn fit(available: f32, grid: bool, fx: bool, stems: bool, library: Library) -> Self {
        let racks = 44.0
            + if grid { 46.0 } else { 0.0 }
            + if fx { 58.0 } else { 0.0 }
            + if stems { 62.0 } else { 0.0 };
        // The decks' least is kept before the library's share; the library's
        // least before anything past the decks' least.
        let most = (available - racks - Self::OVERVIEW_LEAST - Self::DECKS_LEAST).max(Self::FOLDED);
        let library = if library.collapsed {
            Self::FOLDED
        } else {
            (library.share * available).round().max(Self::CRATE_LEAST).min(most)
        };
        let spare = available - racks - library;
        // The overview takes a little over a fifth of what is left, the deck
        // row the rest -- which is where the beat view's lanes grow.
        let overview = (spare * 0.22).round()
            .clamp(Self::OVERVIEW_LEAST, Self::OVERVIEW_MOST)
            .min((spare - Self::DECKS_LEAST).max(Self::OVERVIEW_LEAST));
        // Past its most the deck row has nothing to do with more height, so
        // a folded library's room goes to the overview's waveforms instead.
        let decks = (spare - overview).clamp(Self::DECKS_LEAST, Self::DECKS_MOST.max(Self::DECKS_LEAST));
        let overview = (spare - decks).max(overview);
        Bands { overview, decks, library }
    }
}

/// How soon the next frame is worth drawing.
///
/// A mixer that is moving -- a record turning, a meter falling, a notice
/// fading, a hand on a control, the station at work -- draws at 60 frames a
/// second, and never faster: vsync on a 240 Hz panel is four times the work
/// for nothing a hand can use. The booth's animation needs half that. A
/// console at rest only has a clock to keep.
pub fn repaint_after(app: &Defalt, ctx: &egui::Context) -> Duration {
    let moving = app.decks.iter().any(|d| d.playing || d.scrubbing || d.loading || d.meter > 0.002)
        || app.notice.is_some()
        || ctx.dragged_id().is_some()
        || app.airtime.on
        || app.airtime.live();
    let booth = app.view == crate::View::Radio
        && (app.studio.visualizer || (app.studio.enabled && !app.studio.reduced));
    Duration::from_millis(if moving { 16 } else if booth { 33 } else { 250 })
}

fn band(fill: Color32, inner: f32) -> egui::Frame {
    egui::Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin::same(inner as i8))
}

/* ── Shared pieces ───────────────────────────────────────────────────── */

/// A raised plate. Every group of controls sits on one; the seams are what
/// make a dense panel findable, because you locate the EQ by its box rather
/// than by reading every label on the way past it.
pub fn plate(ui: &Ui, rect: Rect) {
    ui.painter().rect_filled(rect, theme::R_M, theme::PANEL);
    ui.painter()
        .rect_stroke(rect, theme::R_M, Stroke::new(theme::LINE, theme::EDGE), egui::StrokeKind::Inside);
}

/// A well: recessed rather than raised. Waveforms live in these.
pub fn well(ui: &Ui, rect: Rect) {
    ui.painter().rect_filled(rect, theme::R_M, theme::WELL);
    ui.painter()
        .rect_stroke(rect, theme::R_M, Stroke::new(theme::LINE, theme::EDGE), egui::StrokeKind::Inside);
}

/* ── Controls ────────────────────────────────────────────────────────── */

/// What a button is for, which decides how loud it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// The one thing to press here. Filled with its accent, dark ink.
    Primary,
    /// A real key on the panel: raised, with a lit state.
    Secondary,
    /// A quiet toggle or a tool. Flat until touched; on, it keeps a mark.
    Ghost,
}

/// How one button is drawn: its role, whether it is on, whether it can be
/// pressed at all, and the colour it lights in.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub role: Role,
    pub on: bool,
    pub live: bool,
    pub accent: Color32,
    /// Lit for as long as it is held down, as a momentary key is.
    pub momentary: bool,
    /// Wears its accent as an outline at rest, so a key with one job --
    /// CUE, a cue pad -- is found by colour before it is pressed.
    pub outline: bool,
}

impl Look {
    pub fn secondary(on: bool, live: bool) -> Self {
        Look { role: Role::Secondary, on, live, accent: theme::BLUE, momentary: false, outline: false }
    }
    pub fn ghost(on: bool, live: bool) -> Self {
        Look { role: Role::Ghost, on, live, accent: theme::BLUE, momentary: false, outline: false }
    }
    pub fn primary(live: bool) -> Self {
        Look { role: Role::Primary, on: false, live, accent: theme::AMBER, momentary: false, outline: false }
    }
    pub fn accent(self, accent: Color32) -> Self {
        Look { accent, ..self }
    }
    pub fn momentary(self) -> Self {
        Look { momentary: true, ..self }
    }
    pub fn outlined(self) -> Self {
        Look { outline: true, ..self }
    }
}

/// The button body, drawn from the palette.
///
/// A secondary key is a little piece of hardware: a vertical gradient, a
/// highlight along its top edge where the light catches it and a darker line
/// along the bottom. Lit, it glows in its accent. A ghost is flat until it
/// is touched, and when it is on it keeps a bar under its label so the state
/// survives the pointer leaving.
pub fn paint_control(ui: &Ui, rect: Rect, look: Look, hovered: bool, pressed: bool) -> Color32 {
    let painter = ui.painter();
    let radius = theme::R_M;
    let lit = look.live && (look.on || (look.momentary && pressed));
    if !look.live {
        painter.rect_filled(rect, radius, theme::PANEL);
        painter.rect_stroke(rect, radius, Stroke::new(theme::LINE, theme::EDGE), egui::StrokeKind::Inside);
        return theme::TEXT_MUTE;
    }
    match look.role {
        Role::Primary => {
            let fill = if pressed {
                theme::tint(look.accent, Color32::BLACK, 0.15)
            } else if hovered {
                theme::tint(look.accent, Color32::WHITE, 0.12)
            } else {
                look.accent
            };
            glow(ui, rect, look.accent, if hovered { 60 } else { 36 });
            painter.rect_filled(rect, radius, fill);
            highlight(ui, rect, 60);
            theme::VINYL
        }
        Role::Secondary => {
            if lit {
                glow(ui, rect, look.accent, 70);
                painter.rect_filled(rect, radius, theme::lit_fill(look.accent));
                highlight(ui, rect, 26);
                painter.rect_stroke(rect, radius, Stroke::new(theme::LINE, look.accent), egui::StrokeKind::Inside);
                return theme::TEXT_BRIGHT;
            }
            let (top, bottom) = if pressed {
                (theme::RAISED, theme::RAISED)
            } else if hovered {
                (theme::tint(theme::RAISED_HI, Color32::WHITE, 0.05), theme::RAISED_HI)
            } else {
                (theme::RAISED_HI, theme::RAISED)
            };
            gradient(ui, rect, radius, top, bottom);
            if !pressed {
                highlight(ui, rect, 20);
            }
            painter.line_segment(
                [rect.left_bottom() + vec2(radius, -0.5), rect.right_bottom() + vec2(-radius, -0.5)],
                Stroke::new(theme::LINE, Color32::from_black_alpha(110)),
            );
            let edge = if look.outline { look.accent } else if hovered { theme::EDGE_LIT } else { theme::EDGE };
            painter.rect_stroke(rect, radius, Stroke::new(theme::LINE, edge), egui::StrokeKind::Inside);
            theme::TEXT
        }
        Role::Ghost => {
            if lit {
                painter.rect_filled(rect, radius, theme::tint(theme::PANEL, look.accent, 0.14));
                painter.rect_stroke(rect, radius, Stroke::new(theme::LINE, look.accent.gamma_multiply(0.55)),
                                    egui::StrokeKind::Inside);
                let bar = Rect::from_center_size(rect.center_bottom() - vec2(0.0, 3.0),
                                                 vec2((rect.width() * 0.4).clamp(8.0, 28.0), 2.0));
                painter.rect_filled(bar, 1.0, look.accent);
                return theme::TEXT_BRIGHT;
            }
            if hovered || pressed {
                painter.rect_filled(rect, radius, if pressed { theme::RAISED } else { theme::tint(theme::PANEL, theme::RAISED, 0.8) });
            }
            let edge = if look.outline { look.accent } else if hovered || pressed { theme::EDGE } else { Color32::TRANSPARENT };
            painter.rect_stroke(rect, radius, Stroke::new(theme::LINE, edge), egui::StrokeKind::Inside);
            if hovered || pressed { theme::TEXT } else { theme::TEXT_DIM }
        }
    }
}

/// A vertical gradient filling a rounded rectangle, as one mesh.
pub fn gradient(ui: &Ui, rect: Rect, radius: f32, top: Color32, bottom: Color32) {
    // The rounded body in the bottom colour, a rounded cap in the top one,
    // and a straight-sided gradient between the two corners' worth of each,
    // so no square end ever pokes past the rounding.
    let r = radius.min(rect.height() / 2.0);
    let painter = ui.painter();
    painter.rect_filled(rect, r, bottom);
    let corner = r.round() as u8;
    painter.rect_filled(Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + r * 2.0)),
                        egui::CornerRadius { nw: corner, ne: corner, sw: 0, se: 0 }, top);
    let (y0, y1) = (rect.top() + r, rect.bottom() - r);
    if y1 > y0 {
        let at = |x: f32, y: f32, c: Color32| egui::epaint::Vertex { pos: egui::pos2(x, y), uv: egui::epaint::WHITE_UV, color: c };
        let mut mesh = egui::Mesh::default();
        mesh.vertices.extend([
            at(rect.left(), y0, top), at(rect.right(), y0, top),
            at(rect.right(), y1, bottom), at(rect.left(), y1, bottom),
        ]);
        mesh.indices.extend([0, 1, 2, 0, 2, 3]);
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// The one-pixel line along a key's top edge where the light catches it.
fn highlight(ui: &Ui, rect: Rect, alpha: u8) {
    ui.painter().line_segment(
        [rect.left_top() + vec2(theme::R_M, 1.5), rect.right_top() + vec2(-theme::R_M, 1.5)],
        Stroke::new(theme::LINE, Color32::from_white_alpha(alpha)),
    );
}

/// Light spilling from a lit key onto the panel around it: a soft shadow in
/// the key's colour, dropped a pixel as real light falls.
fn glow(ui: &Ui, rect: Rect, colour: Color32, alpha: u8) {
    let shadow = egui::epaint::Shadow {
        offset: [0, 1],
        blur: 10,
        spread: 0,
        color: Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha),
    };
    ui.painter().add(shadow.as_shape(rect, theme::R_M));
}

/// A button, in any of the three roles.
pub fn button(ui: &mut Ui, text: &str, size: egui::Vec2, look: Look) -> Response {
    button_named(ui, text, text, size, look)
}

/// A button whose spoken name is not its printed one.
fn button_named(ui: &mut Ui, text: &str, name: &str, size: egui::Vec2, look: Look) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, if look.live { Sense::click() } else { Sense::hover() });
    response.widget_info(|| if look.on {
        egui::WidgetInfo::selected(egui::WidgetType::Button, look.live, true, name)
    } else {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, look.live, name)
    });
    let hovered = look.live && (response.hovered() || response.has_focus());
    let pressed = look.live && response.is_pointer_button_down_on();
    if ui.is_rect_visible(rect) {
        let ink = paint_control(ui, rect, look, hovered, pressed);
        if !text.is_empty() {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text,
                              FontId::proportional(theme::SIZE_S), ink);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(rect.expand(2.0), theme::R_M + 2.0, Stroke::new(theme::LINE_MID, theme::BLUE),
                                     egui::StrokeKind::Outside);
        }
    }
    if look.live {
        response
    } else {
        response.on_hover_text("Unavailable in the current state.")
    }
}

/// The width a button needs for its label, at the panel's button size.
pub fn fit(ui: &Ui, text: &str, height: f32) -> egui::Vec2 {
    let width = ui.painter().layout_no_wrap(text.into(), FontId::proportional(theme::SIZE_S), theme::TEXT).size().x;
    vec2((width + theme::SP_4 + 4.0).round(), height)
}

/// The small rounded button this whole panel is built out of: a secondary
/// key, lit in the panel's blue when on.
pub fn chip(ui: &mut Ui, text: &str, size: egui::Vec2, on: bool, live: bool) -> Response {
    button(ui, text, size, Look::secondary(on, live))
}

#[derive(Clone, Copy)]
pub enum Glyph {
    Minus,
    Plus,
    Up,
    Down,
    First,
    Cross,
}

/// Draw one of the panel's marks, centred on `centre`.
pub fn draw_glyph(ui: &Ui, glyph: Glyph, centre: egui::Pos2, ink: Color32) {
    let stroke = Stroke::new(theme::LINE_MID, ink);
    let painter = ui.painter();
    let line = |a: egui::Vec2, b: egui::Vec2| { painter.line_segment([centre + a, centre + b], stroke); };
    match glyph {
        Glyph::Minus => line(vec2(-4.0, 0.0), vec2(4.0, 0.0)),
        Glyph::Plus => {
            line(vec2(-4.0, 0.0), vec2(4.0, 0.0));
            line(vec2(0.0, -4.0), vec2(0.0, 4.0));
        }
        Glyph::Up => {
            line(vec2(-4.0, 2.0), vec2(0.0, -2.0));
            line(vec2(0.0, -2.0), vec2(4.0, 2.0));
        }
        Glyph::Down => {
            line(vec2(-4.0, -2.0), vec2(0.0, 2.0));
            line(vec2(0.0, 2.0), vec2(4.0, -2.0));
        }
        Glyph::First => {
            line(vec2(-4.0, -4.0), vec2(4.0, -4.0));
            line(vec2(-4.0, 3.0), vec2(0.0, -1.0));
            line(vec2(0.0, -1.0), vec2(4.0, 3.0));
        }
        Glyph::Cross => {
            line(vec2(-3.5, -3.5), vec2(3.5, 3.5));
            line(vec2(3.5, -3.5), vec2(-3.5, 3.5));
        }
    }
}

/// A button carrying a drawn mark rather than text, for the arrows and
/// crosses the fonts do not draw well. A hollow box where an arrow should be
/// is the single cheapest tell that a panel was assembled rather than built.
pub fn glyph_button(ui: &mut Ui, glyph: Glyph, size: egui::Vec2, look: Look, name: &str) -> Response {
    let response = button_named(ui, "", name, size, look);
    let hovered = look.live && (response.hovered() || response.has_focus());
    let ink = match (look.live, look.role) {
        (false, _) => theme::TEXT_MUTE,
        (true, Role::Primary) => theme::VINYL,
        (true, Role::Ghost) if !hovered && !look.on => theme::TEXT_DIM,
        _ => theme::TEXT,
    };
    draw_glyph(ui, glyph, response.rect.center(), ink);
    response
}

pub fn glyph_chip(ui: &mut Ui, glyph: Glyph, size: egui::Vec2, live: bool) -> Response {
    let name = match glyph {
        Glyph::Minus => "Less",
        Glyph::Plus => "More",
        Glyph::Up => "Up",
        Glyph::Down => "Down",
        Glyph::First => "First",
        Glyph::Cross => "Remove",
    };
    glyph_button(ui, glyph, size, Look::secondary(false, live), name)
}

/// A deck's letter on a small badge in its colour: the colour says which
/// side, the letter says it again for anyone who cannot tell the two apart.
pub fn deck_badge(ui: &Ui, rect: Rect, deck: usize) {
    let colour = theme::DECK_COLOURS[deck];
    ui.painter().rect_filled(rect, theme::R_S, theme::tint(theme::WELL, colour, 0.22));
    ui.painter().rect_stroke(rect, theme::R_S, Stroke::new(theme::LINE, colour), egui::StrokeKind::Inside);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, theme::DECK_LETTERS[deck],
                      theme::display(theme::SIZE_S), colour);
}

/// A silkscreen caption, centred over a column.
pub fn column_cap(ui: &Ui, rect: Rect, text: &str) {
    ui.painter().text(
        rect.center_top() + vec2(0.0, 2.0),
        egui::Align2::CENTER_TOP,
        text,
        FontId::proportional(theme::SIZE_XS),
        theme::TEXT_MUTE,
    );
}

pub fn label(ui: &Ui, at: egui::Pos2, align: egui::Align2, text: &str, size: f32, colour: Color32) {
    ui.painter()
        .text(at, align, text, FontId::proportional(size), colour);
}

pub fn mono(ui: &Ui, at: egui::Pos2, align: egui::Align2, text: &str, size: f32, colour: Color32) {
    ui.painter()
        .text(at, align, text, FontId::monospace(size), colour);
}

/// Ellipsize against actual font metrics, reserving room for neighboring readouts.
pub fn clipped_label(ui: &Ui, rect: Rect, text: &str, size: f32, colour: Color32) {
    clipped_text(ui, rect, text, FontId::proportional(size), colour);
}

/// The same, in any face.
pub fn clipped_text(ui: &Ui, rect: Rect, text: &str, font: FontId, colour: Color32) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, colour);
    job.wrap.max_width = rect.width();
    job.wrap.max_rows = 1;
    let galley = ui.painter().layout_job(job);
    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(rect.min, galley, colour);
}

pub fn rich(text: &str, size: f32, colour: Color32) -> RichText {
    RichText::new(text).font(FontId::proportional(size)).color(colour)
}

/* ── The transcript ──────────────────────────────────────────────────── */

/// How a transcript is set: the on-air page reads it large, the booth small.
#[derive(Clone, Copy)]
pub struct TranscriptStyle {
    pub body: f32,
    pub accent: Color32,
}

/// The whole conversation as text, the way both views copy it.
pub fn transcript_text(lines: &[crate::station::TranscriptLine]) -> String {
    lines.iter()
        .map(|line| format!("[{}] {}: {}", mmss(line.start_at), line.host, line.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Title, the follow toggle, and Copy -- which is dead when there is nothing
/// to copy, because a button that copies nothing looks broken.
pub fn transcript_header(
    ui: &mut Ui,
    lines: &[crate::station::TranscriptLine],
    follow: &mut bool,
    ducked: bool,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(rich("Transcript", theme::SIZE_L, theme::TEXT));
        if ducked {
            ui.label(rich("Music lowered for speech", theme::SIZE_S, theme::CYAN));
        }
        ui.checkbox(follow, "Follow live");
        if ui.add_enabled(!lines.is_empty(), egui::Button::new("Copy")).clicked() {
            ui.ctx().copy_text(transcript_text(lines));
        }
    });
}

/// What the hosts have said, newest last.
///
/// Only the lines in view are laid out and drawn. Each line's height is
/// measured once, when it first arrives or the width changes, and kept; the
/// scroll area is told the total, so a long evening's transcript costs what
/// a screenful does.
pub fn transcript_view(
    ui: &mut Ui,
    salt: &str,
    lines: &[crate::station::TranscriptLine],
    follow: bool,
    style: TranscriptStyle,
    empty: &str,
) {
    egui::ScrollArea::vertical()
        .id_salt(salt)
        .auto_shrink([false, false])
        .stick_to_bottom(follow)
        .show_viewport(ui, |ui, viewport| {
            if lines.is_empty() {
                ui.label(rich(empty, theme::SIZE_M, theme::TEXT_MUTE));
                return;
            }
            let width = ui.available_width().max(40.0);
            let heights = transcript_heights(ui, salt, lines, width, style);
            let total: f32 = heights.iter().sum();
            let (area, _) = ui.allocate_exact_size(vec2(width, total), Sense::hover());
            let mut top = 0.0;
            for (line, height) in lines.iter().zip(heights.iter()) {
                if top + height >= viewport.top() && top <= viewport.bottom() {
                    let row = Rect::from_min_size(area.min + vec2(0.0, top), vec2(width, *height));
                    transcript_line(ui, row, line, style);
                }
                top += height;
            }
        });
}

/// Gap above each line.
const LINE_GAP: f32 = 10.0;

fn line_header(line: &crate::station::TranscriptLine) -> String {
    format!("{}  {}{}", mmss(line.start_at), line.host.to_uppercase(),
            if line.active { "  • SPEAKING" } else { "" })
}

fn line_body(ui: &Ui, line: &crate::station::TranscriptLine, width: f32, style: TranscriptStyle)
    -> std::sync::Arc<egui::Galley> {
    let colour = if line.active { theme::TEXT_BRIGHT } else { theme::TEXT };
    ui.painter().layout(line.text.clone(), FontId::proportional(style.body), colour, width)
}

fn transcript_heights(
    ui: &Ui,
    salt: &str,
    lines: &[crate::station::TranscriptLine],
    width: f32,
    style: TranscriptStyle,
) -> Vec<f32> {
    use std::hash::{Hash, Hasher};
    // Keyed by content rather than position: the station keeps a rolling
    // window, so every line shifts up one when a new one arrives.
    #[derive(Clone, Default)]
    struct Measured {
        width: f32,
        body: f32,
        rows: std::collections::HashMap<u64, f32>,
    }
    let id = egui::Id::new(("transcript-heights", salt));
    let mut measured: Measured = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    if measured.width != width || measured.body != style.body {
        measured = Measured { width, body: style.body, rows: Default::default() };
    }
    let header = ui.painter().layout_no_wrap("0".into(), FontId::monospace(theme::SIZE_S), theme::TEXT).size().y;
    let source = ui.painter().layout_no_wrap("S".into(), FontId::proportional(theme::SIZE_XS), theme::TEXT).size().y;
    let mut seen = Vec::with_capacity(lines.len());
    let heights = lines.iter().map(|line| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (&line.text, &line.source, line.start_at.to_bits()).hash(&mut hasher);
        let fingerprint = hasher.finish();
        seen.push(fingerprint);
        *measured.rows.entry(fingerprint).or_insert_with(|| {
            LINE_GAP + header + 2.0 + line_body(ui, line, width, style).size().y
                + if line.source.is_some() { source + 4.0 } else { 0.0 }
        })
    }).collect();
    measured.rows.retain(|fingerprint, _| seen.contains(fingerprint));
    ui.data_mut(|d| d.insert_temp(id, measured));
    heights
}

fn transcript_line(ui: &mut Ui, row: Rect, line: &crate::station::TranscriptLine, style: TranscriptStyle) {
    let mut at = row.min + vec2(0.0, LINE_GAP);
    let header = ui.painter().layout_no_wrap(
        line_header(line),
        FontId::monospace(theme::SIZE_S),
        if line.active { theme::CYAN } else { style.accent },
    );
    let header_height = header.size().y;
    ui.painter().galley(at, header, theme::TEXT);
    at.y += header_height + 2.0;
    let body = line_body(ui, line, row.width(), style);
    let body_rect = Rect::from_min_size(at, body.size());
    at.y += body.size().y + 4.0;
    ui.put(body_rect, egui::Label::new(body).selectable(true));
    if let Some(source) = &line.source {
        let text = rich(&format!("Source: {source}"), theme::SIZE_XS, theme::TEXT_DIM);
        let place = Rect::from_min_size(at, vec2(row.width(), (row.bottom() - at.y).max(1.0)));
        let mut slot = child(ui, place, left_row(), &format!("src{}", line.start_at.to_bits()));
        match &line.source_url {
            Some(url) => { slot.hyperlink_to(text, url); }
            None => { slot.label(text); }
        }
    }
}

/* ── Catalogue suggestions ───────────────────────────────────────────── */

/// One line saying why there are no suggestions, if there is a reason worth
/// giving: no credentials, an error, or a search still running.
pub fn suggestion_status(ui: &mut Ui, search: &crate::spotify::Search, typed: bool) {
    let (text, colour) = if !search.available() {
        if !typed { return; }
        ("No Spotify credentials, so no suggestions. Typed text still works.".to_string(), theme::TEXT_MUTE)
    } else if let Some(error) = &search.error {
        (error.clone(), theme::RED)
    } else if search.busy && search.showing.is_empty() {
        ("Searching Spotify…".to_string(), theme::TEXT_MUTE)
    } else {
        return;
    };
    ui.add_space(4.0);
    ui.label(rich(&text, theme::SIZE_XS, colour));
}

/// What the catalogue thinks you meant, as rows to pick from. Returns the
/// one chosen, if any.
///
/// Each row is the catalogue's exact spelling with its length and year, so
/// the right record is picked on sight rather than by trying them.
pub fn suggestion_list(
    ui: &mut Ui,
    showing: &[crate::spotify::Suggestion],
    width: f32,
    limit: usize,
) -> Option<usize> {
    let mut chosen = None;
    for (at, found) in showing.iter().take(limit).enumerate() {
        let (rect, response) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, found.query()));
        if response.hovered() || response.has_focus() {
            ui.painter().rect_filled(rect, theme::R_M, theme::RAISED);
        }
        let inner = rect.shrink2(vec2(6.0, 1.0));
        let right = 44.0;
        clipped_label(ui, Rect::from_min_size(inner.left_top(), vec2(inner.width() - right, 16.0)),
                      &found.title, theme::SIZE_S, theme::TEXT);
        clipped_label(ui, Rect::from_min_size(inner.left_top() + vec2(0.0, 15.0), vec2(inner.width() - right, 15.0)),
                      &found.artist, theme::SIZE_XS, theme::TEXT_DIM);
        mono(ui, inner.right_top(), egui::Align2::RIGHT_TOP, &found.length(), theme::SIZE_XS, theme::TEXT_MUTE);
        if let Some(year) = &found.year {
            mono(ui, inner.right_top() + vec2(0.0, 15.0), egui::Align2::RIGHT_TOP, year, theme::SIZE_XS, theme::TEXT_MUTE);
        }
        let hint = match &found.album {
            Some(album) => format!("{}\n{album} - {}", found.query(), found.length()),
            None => format!("{}\n{}", found.query(), found.length()),
        };
        if response.on_hover_text(hint).clicked() {
            chosen = Some(at);
        }
    }
    chosen
}

/// Left / centre / right, with the centre capped so it does not swallow a
/// wide window.
pub fn split_thirds(rect: Rect, middle: f32, gap: f32) -> (Rect, Rect, Rect) {
    let middle = middle.min((rect.width() - gap * 2.0 - 80.0).max(60.0));
    let side = ((rect.width() - middle - gap * 2.0) / 2.0).max(40.0);
    let left = Rect::from_min_size(rect.min, vec2(side, rect.height()));
    let centre = Rect::from_min_size(
        egui::pos2(left.right() + gap, rect.top()),
        vec2(middle, rect.height()),
    );
    let right = Rect::from_min_size(
        egui::pos2(centre.right() + gap, rect.top()),
        vec2(side, rect.height()),
    );
    (left, centre, right)
}

/// A child laid out over an exact rectangle.
///
/// The salt is not decoration. Without one, every widget inside gets an id
/// from the parent's running child counter, so hiding a band renumbers
/// everything after it and a drag in progress lands on a different control.
pub fn child(ui: &mut Ui, rect: Rect, layout: Layout, salt: &str) -> Ui {
    ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(layout)
            .id_salt(salt),
    )
}

pub fn left_row() -> Layout {
    Layout::left_to_right(Align::Center)
}

pub fn right_row() -> Layout {
    Layout::right_to_left(Align::Center)
}

/* ── Overlays ────────────────────────────────────────────────────────── */

/// A line at the foot of the window, for the things that have no other way of
/// showing they happened -- a cue set, a key with nothing behind it yet.
fn notice(app: &mut Defalt, ctx: &egui::Context) {
    let Some((message, since)) = app.notice.clone() else { return };
    let age = since.elapsed().as_secs_f32();
    if age > 2.6 {
        app.notice = None;
        return;
    }
    // Holds, then fades. A notice that vanishes on a timer looks like a bug;
    // one that fades looks like it finished.
    let alpha = ((2.6 - age) / 0.8).clamp(0.0, 1.0);

    // Laid out here, at a width worked out from the text and the window,
    // then placed by its own size. An egui Area left to size itself wraps
    // its contents at whatever size it was last frame, so after a short
    // notice a long one came out a word wide, broken mid-word.
    let screen = ctx.content_rect();
    let galley = notice_galley(ctx, &message, screen.width());
    let pad = vec2(theme::SP_3, theme::SP_2);
    let size = galley.size() + pad * 2.0;
    let bottom = screen.bottom() - about::FOOTER - theme::SP_2;
    let rect = Rect::from_min_size(egui::pos2(screen.center().x - size.x / 2.0, bottom - size.y), size);
    let cut = galley.elided;

    egui::Area::new(egui::Id::new("notice"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .constrain(false)
        // Only a cut-short notice takes the pointer, to show the rest.
        .interactable(cut)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().rect(rect, theme::R_L, theme::RAISED.gamma_multiply(alpha * 0.95),
                              Stroke::new(1.0, theme::EDGE.gamma_multiply(alpha)), egui::StrokeKind::Inside);
            ui.painter().galley(rect.min + pad, galley, theme::TEXT.gamma_multiply(alpha));
            if cut {
                hint(response, &message);
            }
        });
}

/// The widest a notice is set before it wraps.
pub const NOTICE_MOST: f32 = 520.0;
/// However narrow the window, a notice is never wrapped tighter than this.
pub const NOTICE_LEAST: f32 = 160.0;
/// Past this many lines a notice is cut short, the whole of it on hover.
pub const NOTICE_ROWS: usize = 3;

/// The width a notice's text wraps at: the most it may take in a window this
/// wide, keeping clear of the window's edges.
pub fn notice_width(screen: f32) -> f32 {
    wrap_width(NOTICE_MOST, screen)
}

/// The width floating text wraps at: `most`, or less in a narrow window, but
/// never so little that it comes out a word or two to the line.
pub fn wrap_width(most: f32, screen: f32) -> f32 {
    most.min(screen - 2.0 * (theme::SP_4 + theme::SP_3)).max(NOTICE_LEAST)
}

/// Text on one line at its own width when that fits, otherwise wrapped
/// between words at `width` and cut short with an ellipsis after `rows`.
///
/// For anything floating. An Area sizes itself to last frame's contents, so
/// a label left to wrap at the width it is given inherits whatever the text
/// before it needed; a galley laid out here depends on nothing but its text.
pub fn wrapped(ctx: &egui::Context, text: &str, font: FontId, colour: Color32, width: f32, rows: usize)
    -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, colour, width);
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = false;
    ctx.fonts_mut(|fonts| fonts.layout_job(job))
}

/// A notice's text, wrapped at `notice_width` and cut short after a few
/// lines. Inked when painted, so the fade reaches the text too.
pub fn notice_galley(ctx: &egui::Context, text: &str, screen: f32) -> std::sync::Arc<egui::Galley> {
    wrapped(ctx, text, FontId::proportional(theme::SIZE_S), Color32::PLACEHOLDER, notice_width(screen), NOTICE_ROWS)
}

/// A tooltip whose text can change while it is showing. egui sizes a tooltip
/// once, when it opens; a longer text after that wrapped at the shorter one's
/// width. Laid out from the text alone, it is always as wide as it needs.
pub fn hint(response: Response, text: &str) -> Response {
    if text.is_empty() {
        return response;
    }
    response.on_hover_ui(|ui| {
        let width = wrap_width(ui.spacing().tooltip_width, ui.ctx().content_rect().width());
        let font = egui::TextStyle::Body.resolve(ui.style());
        let galley = wrapped(ui.ctx(), text, font, ui.visuals().text_color(), width, usize::MAX);
        ui.add(egui::Label::new(galley));
    })
}

fn help_overlay(app: &mut Defalt, ctx: &egui::Context) {
    if app.info_page.is_some() { return; }
    if ctx.input(|i| i.key_pressed(egui::Key::F1)) && !ctx.text_edit_focused() {
        app.show_help = !app.show_help;
    }
    if !app.show_help {
        return;
    }

    let mut open = true;
    egui::Window::new("Keyboard")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.label(rich(
                "Taken from the reference application's own defaults.",
                theme::SIZE_XS,
                theme::TEXT_MUTE,
            ));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                for (section, rows) in crate::keys::HELP {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(section.to_uppercase())
                            .font(FontId::monospace(theme::SIZE_XS))
                            .color(theme::BLUE),
                    );
                    egui::Grid::new(section)
                        .num_columns(2)
                        .spacing(vec2(16.0, 3.0))
                        .show(ui, |ui| {
                            for (keys, what) in *rows {
                                ui.label(
                                    RichText::new(*keys)
                                        .font(FontId::monospace(theme::SIZE_XS))
                                        .color(theme::TEXT),
                                );
                                ui.label(rich(what, theme::SIZE_S, theme::TEXT_DIM));
                                ui.end_row();
                            }
                        });
                }
            });
        });
    if !open {
        app.show_help = false;
    }
}

/* ── Formatting ──────────────────────────────────────────────────────── */
pub fn mmss(seconds: f64) -> String {
    let whole = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

pub fn tenths(seconds: f64) -> String {
    let whole = seconds.max(0.0) as u64;
    let tenth = ((seconds.max(0.0) - whole as f64) * 10.0) as u64;
    format!("{}:{:02}.{}", whole / 60, whole % 60, tenth)
}

pub fn elide(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// The crate's rows as last worked out, and what they were worked out for.
#[derive(Default)]
struct CrateRows {
    key: Option<RowsKey>,
    rows: Rc<[usize]>,
    /// Lowercased title, artist and key per record, folded once per library
    /// rather than once per comparison.
    folded: Vec<[String; 3]>,
    folded_for: Option<(u64, usize)>,
}

/// Everything the crate's order depends on. When none of it has changed the
/// last answer is still the answer.
#[derive(Clone, PartialEq, Debug)]
struct RowsKey {
    generation: u64,
    records: usize,
    search: String,
    sort: (Column, bool),
    /// Only for the match column: the deck, record and tempo it is scored
    /// against.
    reference: Option<(usize, String, i64)>,
}

/// Indices into `app.records`, filtered and sorted.
pub fn filtered(app: &Defalt) -> Vec<usize> {
    rows(app).to_vec()
}

/// The same, shared rather than copied: the crate asks for this several
/// times a frame and it only changes when something it depends on does.
pub fn rows(app: &Defalt) -> Rc<[usize]> {
    let key = RowsKey {
        generation: app.view_state.records_generation,
        records: app.records.len(),
        search: app.search.trim().to_lowercase(),
        sort: app.sort,
        reference: if app.sort.0 == Column::Match { app.match_reference_key() } else { None },
    };
    let mut cache = app.view_state.crate_rows.borrow_mut();
    if cache.key.as_ref() == Some(&key) {
        return cache.rows.clone();
    }
    let library = (key.generation, key.records);
    if cache.folded_for != Some(library) {
        cache.folded = app.records.iter().map(|record| [
            record.title.to_lowercase(),
            record.artist.to_lowercase(),
            record.camelot.as_deref().unwrap_or("").to_lowercase(),
        ]).collect();
        cache.folded_for = Some(library);
    }
    let rows = sorted_rows(app, &cache.folded, &key.search);
    cache.rows = rows.into();
    cache.key = Some(key);
    cache.rows.clone()
}

fn sorted_rows(app: &Defalt, folded: &[[String; 3]], needle: &str) -> Vec<usize> {
    let mut rows: Vec<usize> = folded
        .iter()
        .enumerate()
        .filter(|(_, [title, artist, camelot])| {
            needle.is_empty() || artist.contains(needle) || title.contains(needle) || camelot == needle
        })
        .map(|(index, _)| index)
        .collect();

    let (column, ascending) = app.sort;
    if column == Column::Match {
        // Best first when ascending, because "sorted by match" means the
        // easiest mix at the top -- nobody wants the worst one there. Scored
        // once per record, not once per comparison. Unscored records go
        // last either way round.
        rows.sort_by_cached_key(|&index| {
            let score = app.fit_for(index).map(|fit| (fit.score * 1_000_000.0) as i64);
            match (score, ascending) {
                (Some(score), true) => (0, -score),
                (None, true) => (1, 0),
                (Some(score), false) => (1, score),
                (None, false) => (0, 0),
            }
        });
        return rows;
    }
    rows.sort_by(|&a, &b| {
        let x = &app.records[a];
        let y = &app.records[b];
        let ordering = match column {
            Column::Title => folded[a][0].cmp(&folded[b][0]),
            Column::Artist => folded[a][1].cmp(&folded[b][1]),
            Column::Duration => option_order(x.duration, y.duration, ascending),
            Column::Bpm => option_order(x.bpm, y.bpm, ascending),
            Column::Key => option_str(x.camelot.as_deref(), y.camelot.as_deref(), ascending),
            Column::Match => std::cmp::Ordering::Equal,
        };
        if ascending {
            ordering
        } else {
            ordering.reverse()
        }
    });
    rows
}

fn option_order(a: Option<f64>, b: Option<f64>, ascending: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
        (None, None) => Ordering::Equal,
        // A record with no detected tempo is unknown, not slow: flipped when
        // descending so the reverse above still puts it last.
        (None, _) => if ascending { Ordering::Greater } else { Ordering::Less },
        (_, None) => if ascending { Ordering::Less } else { Ordering::Greater },
    }
}

fn option_str(a: Option<&str>, b: Option<&str>, ascending: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(b),
        (None, None) => Ordering::Equal,
        (None, _) => if ascending { Ordering::Greater } else { Ordering::Less },
        (_, None) => if ascending { Ordering::Less } else { Ordering::Greater },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Record;

    fn record(key: &str, artist: &str, bpm: Option<f64>, camelot: Option<&str>) -> Record {
        Record {
            key: key.into(), title: format!("{key} title"), artist: artist.into(), album: None,
            duration: Some(200.0), bpm, camelot: camelot.map(str::to_string), lufs: None,
            file: std::path::PathBuf::new(), beat_offset: None, beat_period: Some(0.5),
            downbeat_offset: None,
        }
    }

    fn app() -> Defalt {
        let mut app = Defalt::from_root(std::env::temp_dir().join("defalt-no-fixture"), false);
        app.records = vec![
            record("c", "Charlie", Some(124.0), Some("8A")),
            record("a", "alpha", Some(128.0), Some("9A")),
            record("b", "Bravo", Some(90.0), Some("2B")),
        ];
        app.view_state.records_generation += 1;
        app.sort = (Column::Artist, true);
        app
    }

    #[test]
    fn the_crate_is_worked_out_once_and_shared_until_something_changes() {
        let mut app = app();
        let first = rows(&app);
        assert_eq!(&*first, &[1, 2, 0], "sorted by artist, case folded");
        assert!(Rc::ptr_eq(&first, &rows(&app)), "an unchanged crate was sorted again");

        app.search = "BRA".into();
        assert_eq!(&*rows(&app), &[2], "a new search was not applied");
        app.search = "9a".into();
        assert_eq!(&*rows(&app), &[1], "keys match exactly, ignoring case");
        app.search.clear();

        app.sort = (Column::Bpm, false);
        assert_eq!(&*rows(&app), &[1, 0, 2]);
    }

    #[test]
    fn a_reloaded_library_is_never_served_from_the_old_cache() {
        let mut app = app();
        assert_eq!(rows(&app).len(), 3);
        // Same length, different records: only the generation says so.
        app.records[0] = record("d", "Delta", None, None);
        app.view_state.records_generation += 1;
        assert!(filtered(&app).iter().any(|&i| app.records[i].key == "d"));
        app.search = "delta".into();
        assert_eq!(rows(&app).len(), 1, "the old record's folded name was searched");
        app.search.clear();
        app.records.push(record("e", "Echo", None, None));
        assert_eq!(rows(&app).len(), 4, "a longer library was served from the cache");
    }

    #[test]
    fn match_order_follows_its_reference_and_only_its_reference() {
        let mut app = app();
        app.sort = (Column::Match, true);
        app.decks[0].record = Some(record("ref", "Ref", Some(128.0), Some("9A")));
        let rows_a = rows(&app);
        assert_eq!(app.records[rows_a[0]].key, "a", "the closest match is not first");
        // Unrelated state does not reshuffle it.
        app.crossfade = 0.9;
        assert!(Rc::ptr_eq(&rows_a, &rows(&app)));
        // A new reference does.
        app.decks[0].record = Some(record("ref2", "Ref", Some(90.0), Some("2B")));
        assert_eq!(app.records[rows(&app)[0]].key, "b");
    }

    #[test]
    fn the_library_takes_under_two_fifths_and_the_decks_grow_into_the_rest() {
        // 1440x900, less the toolbar and footer.
        let roomy = Bands::fit(830.0, false, false, false, Library::default());
        let share = roomy.library / 830.0;
        assert!((0.38..=0.40).contains(&share), "the library took {share}");
        assert!(roomy.decks > DECK_ROW, "the decks did not grow: {roomy:?}");
        assert_eq!(roomy.overview + roomy.decks + 44.0 + roomy.library, 830.0);
    }

    #[test]
    fn a_short_window_takes_height_from_the_library_share_not_the_decks() {
        // 1024x640 with every rack open, less the toolbar and footer.
        let tight = Bands::fit(570.0, true, true, true, Library::default());
        assert!(tight.decks >= Bands::DECKS_LEAST && tight.overview >= 64.0);
        let crate_left = 570.0 - tight.overview - tight.decks - 44.0 - 46.0 - 58.0 - 62.0;
        assert!(crate_left >= Bands::FOLDED, "only {crate_left} left for the crate");
        assert_eq!(crate_left, tight.library);
    }

    #[test]
    fn a_folded_library_keeps_its_header_and_gives_the_decks_the_window() {
        let folded = Library { collapsed: true, ..Library::default() };
        let bands = Bands::fit(830.0, false, false, false, folded);
        assert_eq!(bands.library, Bands::FOLDED);
        assert_eq!(bands.overview + bands.decks + 44.0 + bands.library, 830.0);
        let open = Bands::fit(830.0, false, false, false, Library::default());
        assert!(bands.decks > open.decks && bands.overview >= open.overview);
    }

    #[test]
    fn the_library_layout_is_remembered_and_a_bad_file_is_ignored() {
        let root = std::env::temp_dir().join(format!("defalt-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(Library::load(&root).share, Library::SHARE);
        let mut library = Library::load(&root);
        library.set_share(0.9);
        assert_eq!(library.share, Library::MOST, "the share was not clamped");
        library.collapsed = true;
        library.save(&root);
        let back = Library::load(&root);
        assert_eq!((back.share, back.collapsed), (Library::MOST, true));
        std::fs::write(root.join("cache").join("console-layout.json"), b"not json").unwrap();
        assert_eq!(Library::load(&root).share, Library::SHARE);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each wrapped row's text, and whether the galley was cut short.
    fn notice_rows(text: &str, screen: f32) -> (Vec<String>, f32, bool) {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut out = (Vec::new(), 0.0, false);
        ctx.run_ui(egui::RawInput::default(), |_| {
            let galley = notice_galley(&ctx, text, screen);
            let rows = galley.rows.iter().map(|row| row.row.glyphs.iter().map(|g| g.chr).collect()).collect();
            out = (rows, galley.size().x, galley.elided);
        }).drop_without_applying_deltas();
        out
    }

    fn natural(text: &str) -> f32 {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut width = 0.0;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            width = ui.painter().layout_no_wrap(text.into(), FontId::proportional(theme::SIZE_S), theme::TEXT).size().x;
        }).drop_without_applying_deltas();
        width
    }

    #[test]
    fn a_notice_wraps_at_most_520_and_never_tighter_than_160() {
        assert_eq!(notice_width(1440.0), NOTICE_MOST);
        assert_eq!(notice_width(1024.0), NOTICE_MOST);
        assert!(notice_width(400.0) < NOTICE_MOST, "a narrow window's notice ran to its edges");
        assert_eq!(notice_width(100.0), NOTICE_LEAST);
        assert_eq!(notice_width(0.0), NOTICE_LEAST);
        // Tooltips go through the same floor, at their own most.
        assert_eq!(wrap_width(500.0, 1024.0), 500.0);
        assert_eq!(wrap_width(500.0, 120.0), NOTICE_LEAST);
    }

    #[test]
    fn a_short_notice_sits_on_one_line_at_its_own_width() {
        let text = "Mix settings sent. New transitions use the new choices.";
        for screen in [1440.0, 1024.0] {
            let (rows, width, cut) = notice_rows(text, screen);
            assert_eq!(rows.len(), 1, "wrapped at {screen}: {rows:?}");
            assert!((width - natural(text)).abs() < 1.0, "{width} is not the text's own width");
            assert!(!cut);
        }
        let (rows, width, _) = notice_rows("Skipped.", 1024.0);
        assert_eq!(rows.len(), 1);
        assert!(width < 100.0, "a short notice was padded out to {width}");
    }

    #[test]
    fn a_long_notice_wraps_between_words_and_is_cut_after_three_lines() {
        let long = "The station is still getting ready (warming the voices, fetching the next record and \
                    working out the transition into it), so the first song may take a moment longer than usual.";
        for screen in [1440.0, 1024.0, 300.0] {
            let (rows, width, _) = notice_rows(long, screen);
            assert!(rows.len() > 1 && rows.len() <= NOTICE_ROWS, "{rows:?}");
            assert!(width <= notice_width(screen) + 0.5, "{width} is wider than it may be");
            assert!(width >= NOTICE_LEAST.min(natural(long)) - 40.0, "wrapped a word wide: {rows:?}");
            for pair in rows.windows(2) {
                let (end, start) = (pair[0].chars().last(), pair[1].chars().next());
                assert!(!(end.is_some_and(char::is_alphanumeric) && start.is_some_and(char::is_alphanumeric)),
                        "broke mid-word: {pair:?}");
            }
        }
        let (rows, _, cut) = notice_rows(&long.repeat(4), 1024.0);
        assert_eq!(rows.len(), NOTICE_ROWS);
        assert!(cut && rows.last().unwrap().ends_with('…'), "a very long notice was not cut: {rows:?}");
    }

    #[test]
    fn a_long_notice_after_a_short_one_is_not_squeezed_to_its_width() {
        // What an Area sizing itself did: remembered the short one's width.
        let mut app = app();
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1024.0, 640.0))),
            ..Default::default()
        };
        app.say("Skipped.");
        for _ in 0..3 {
            ctx.run_ui(input(), |_| notice(&mut app, &ctx)).drop_without_applying_deltas();
        }
        let short = ctx.memory(|m| m.area_rect(egui::Id::new("notice"))).expect("no notice drawn");
        let text = "Mix settings sent. New transitions use the new choices.";
        app.say(text);
        for _ in 0..3 {
            ctx.run_ui(input(), |_| notice(&mut app, &ctx)).drop_without_applying_deltas();
        }
        let long = ctx.memory(|m| m.area_rect(egui::Id::new("notice"))).unwrap();
        assert!(long.width() > natural(text) && long.width() > short.width() * 2.0, "{short:?} then {long:?}");
        assert!((long.center().x - 512.0).abs() <= 1.0, "not centred: {long:?}");
        assert!(long.bottom() <= 640.0 - about::FOOTER, "over the footer: {long:?}");
        assert!(long.height() < 40.0, "a one-line notice is {} tall", long.height());
    }

    #[test]
    fn an_idle_console_draws_rarely_and_a_moving_one_at_sixty() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert_eq!(repaint_after(&app, &ctx), Duration::from_millis(250));
        app.decks[1].playing = true;
        assert_eq!(repaint_after(&app, &ctx), Duration::from_millis(16));
        app.decks[1].playing = false;
        app.view = crate::View::Radio;
        app.studio.enabled = true;
        app.studio.reduced = false;
        assert_eq!(repaint_after(&app, &ctx), Duration::from_millis(33));
    }
}
