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
pub mod radio;
pub mod studio;
pub mod theme;
pub mod toolbar;
pub mod waveform;
pub mod widgets;
pub mod visualizer;
pub mod director_chat;

use egui::{vec2, Align, Color32, FontId, Layout, Rect, Response, RichText, Sense, Stroke, Ui};

use crate::Defalt;

#[derive(Clone, Copy, PartialEq, Eq)]
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

pub fn draw(app: &mut Defalt, ui: &mut Ui) {
    if let Some(engine) = &app.engine {
        engine.telemetry.visualizer.set_enabled(app.view == crate::View::Radio && app.studio.visualizer && app.airtime.on);
    }
    let ctx = ui.ctx().clone();
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
            .frame(band(theme::PANEL, 0.0))
            .show(ui, |ui| radio::draw(app, ui));
        notice(app, &ctx);
        help_overlay(app, &ctx);
        about::overlay(app, &ctx);
        return;
    }

    // Every band above the browser is a fixed height and the browser takes
    // whatever is left. The other way round -- decks flexible, browser fixed
    // -- stretches the decks into a void on any screen larger than a laptop,
    // and a stretched mixer is a sparse one.
    egui::Panel::top("overview")
        .exact_size(96.0)
        .frame(band(theme::GROUND, 4.0))
        .show_separator_line(false)
        .show(ui, |ui| decks::overview_strip(app, ui));

    egui::Panel::top("decks")
        .exact_size(DECK_ROW)
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

/// Tall enough for a 200px jog with its pitch column beside it, no taller.
pub const DECK_ROW: f32 = 256.0;

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
    ui.painter().rect_filled(rect, 5.0, theme::PANEL);
    ui.painter()
        .rect_stroke(rect, 5.0, Stroke::new(1.0, theme::EDGE), egui::StrokeKind::Inside);
}

/// A well: recessed rather than raised. Waveforms live in these.
pub fn well(ui: &Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 4.0, theme::WELL);
    ui.painter()
        .rect_stroke(rect, 4.0, Stroke::new(1.0, theme::EDGE), egui::StrokeKind::Inside);
}

/// The small rounded button this whole panel is built out of.
pub fn chip(ui: &mut Ui, text: &str, size: egui::Vec2, on: bool, live: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, if live { Sense::click() } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, live, text));
    let hovered = response.hovered() && live;

    let fill = if !live {
        theme::PANEL
    } else if on {
        theme::BLUE_DEEP
    } else if hovered || response.has_focus() {
        theme::RAISED_HI
    } else {
        theme::RAISED
    };
    let edge = if on {
        theme::BLUE
    } else if hovered {
        theme::EDGE_LIT
    } else {
        theme::EDGE
    };
    let ink = if !live {
        theme::TEXT_MUTE
    } else if on {
        theme::TEXT_BRIGHT
    } else {
        theme::TEXT
    };

    ui.painter().rect_filled(rect, 5.0, fill);
    ui.painter()
        .rect_stroke(rect, 5.0, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::proportional(11.5),
        ink,
    );

    if live {
        response
    } else {
        response.on_hover_text("Unavailable in the current state.")
    }
}

#[derive(Clone, Copy)]
pub enum Glyph {
    Minus,
    Plus,
}

/// A chip carrying a drawn mark rather than text, for the arrows and crosses
/// the default font does not have. A hollow box where an arrow should be is
/// the single cheapest tell that a panel was assembled rather than built.
pub fn glyph_chip(ui: &mut Ui, glyph: Glyph, size: egui::Vec2, live: bool) -> Response {
    let response = chip(ui, "", size, false, live);
    let rect = response.rect;
    let centre = rect.center();
    let ink = if live { theme::TEXT } else { theme::TEXT_MUTE };
    let stroke = Stroke::new(1.3, ink);
    let painter = ui.painter();

    match glyph {
        Glyph::Minus => painter.line_segment([centre + vec2(-3.5, 0.0), centre + vec2(3.5, 0.0)], stroke),
        Glyph::Plus => {
            painter.line_segment([centre + vec2(-3.5, 0.0), centre + vec2(3.5, 0.0)], stroke);
            painter.line_segment([centre + vec2(0.0, -3.5), centre + vec2(0.0, 3.5)], stroke)
        }
    };
    response
}

/// A label with a chevron: something you would pick from, once there is
/// something to pick.
pub fn dropdown(ui: &mut Ui, text: &str, width: f32, live: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 20.0), if live { Sense::click() } else { Sense::hover() });
    let ink = if live { theme::TEXT } else { theme::TEXT_MUTE };

    ui.painter().rect_filled(rect, 4.0, theme::RAISED);
    ui.painter()
        .rect_stroke(rect, 4.0, Stroke::new(1.0, theme::EDGE), egui::StrokeKind::Inside);
    ui.painter().text(
        rect.left_center() + vec2(7.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        FontId::proportional(10.5),
        ink,
    );
    chevron(
        ui,
        rect.right_center() - vec2(9.0, 0.0),
        if live { theme::TEXT_DIM } else { theme::TEXT_MUTE },
    );

    if live {
        response
    } else {
        response.on_hover_text(NOT_WIRED)
    }
}

pub fn chevron(ui: &Ui, at: egui::Pos2, colour: Color32) {
    ui.painter().add(egui::Shape::line(
        vec![at + vec2(-3.5, -1.5), at + vec2(0.0, 2.0), at + vec2(3.5, -1.5)],
        egui::epaint::PathStroke::new(1.3, colour),
    ));
}

/// A silkscreen caption, centred over a column.
pub fn column_cap(ui: &Ui, rect: Rect, text: &str) {
    ui.painter().text(
        rect.center_top() + vec2(0.0, 3.0),
        egui::Align2::CENTER_TOP,
        text,
        FontId::proportional(10.0),
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
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), FontId::proportional(size), colour);
    job.wrap.max_width = rect.width();
    job.wrap.max_rows = 1;
    let galley = ui.painter().layout_job(job);
    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(rect.min, galley, colour);
}

pub fn rich(text: &str, size: f32, colour: Color32) -> RichText {
    RichText::new(text).font(FontId::proportional(size)).color(colour)
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

    egui::Area::new(egui::Id::new("notice"))
        .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -18.0))
        .interactable(false)
        .show(ctx, |ui| {
            let text = RichText::new(&message)
                .font(FontId::proportional(11.5))
                .color(theme::TEXT.gamma_multiply(alpha));
            egui::Frame::NONE
                .fill(theme::RAISED.gamma_multiply(alpha * 0.95))
                .stroke(Stroke::new(1.0, theme::EDGE.gamma_multiply(alpha)))
                .corner_radius(5.0)
                .inner_margin(egui::Margin::symmetric(12, 6))
                .show(ui, |ui| ui.label(text));
        });
}

fn help_overlay(app: &mut Defalt, ctx: &egui::Context) {
    if app.info_page.is_some() { return; }
    if ctx.input(|i| i.key_pressed(egui::Key::F1)) && !ctx.memory(|m| m.focused().is_some()) {
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
                10.5,
                theme::TEXT_MUTE,
            ));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                for (section, rows) in crate::keys::HELP {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(section.to_uppercase())
                            .font(FontId::monospace(8.5))
                            .color(theme::BLUE),
                    );
                    egui::Grid::new(section)
                        .num_columns(2)
                        .spacing(vec2(16.0, 3.0))
                        .show(ui, |ui| {
                            for (keys, what) in *rows {
                                ui.label(
                                    RichText::new(*keys)
                                        .font(FontId::monospace(10.5))
                                        .color(theme::TEXT),
                                );
                                ui.label(rich(what, 11.0, theme::TEXT_DIM));
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

/// Indices into `app.records`, filtered and sorted.
pub fn filtered(app: &Defalt) -> Vec<usize> {
    let needle = app.search.trim().to_lowercase();
    let mut rows: Vec<usize> = app
        .records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            needle.is_empty()
                || record.artist.to_lowercase().contains(&needle)
                || record.title.to_lowercase().contains(&needle)
                || record
                    .camelot
                    .as_deref()
                    .is_some_and(|k| k.to_lowercase() == needle)
        })
        .map(|(index, _)| index)
        .collect();

    let (column, ascending) = app.sort;
    rows.sort_by(|&a, &b| {
        let x = &app.records[a];
        let y = &app.records[b];
        let ordering = match column {
            Column::Title => x.title.to_lowercase().cmp(&y.title.to_lowercase()),
            Column::Artist => x.artist.to_lowercase().cmp(&y.artist.to_lowercase()),
            Column::Duration => option_order(x.duration, y.duration, ascending),
            Column::Bpm => option_order(x.bpm, y.bpm, ascending),
            Column::Key => option_str(x.camelot.as_deref(), y.camelot.as_deref(), ascending),
            // Best first when ascending, because "sorted by match" means the
            // easiest mix at the top -- nobody wants the worst one there.
            Column::Match => {
                let fit = |index: usize| app.fit_for(index).map(|f| f.score);
                match (fit(a), fit(b)) {
                    (Some(a), Some(b)) => b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal),
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, _) => std::cmp::Ordering::Greater,
                    (_, None) => std::cmp::Ordering::Less,
                }
            }
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
