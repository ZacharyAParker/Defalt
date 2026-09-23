//! The browser: a source rail on the left, the crate on the right.
//!
//! Selecting a row here is what the load buttons in the overview strip act
//! on, so picking a record and choosing a deck are two separate decisions --
//! which is how you audition something without committing it to a deck.

use egui::{vec2, Align, Align2, FontId, Layout, Rect, RichText, Sense, Stroke, Ui};
use egui_extras::{Column as TableColumn, TableBuilder};

use super::{theme, Column, Library, Look};
use crate::Defalt;

const RAIL: f32 = 224.0;
/// The grip between the decks and the library.
pub const SPLITTER: f32 = 8.0;

pub fn draw(app: &mut Defalt, ui: &mut Ui) {
    let whole = ui.max_rect();
    let grip = Rect::from_min_size(whole.min, vec2(whole.width(), SPLITTER));
    splitter(app, ui, grip);
    let full = Rect::from_min_max(egui::pos2(whole.left(), grip.bottom()), whole.max);
    if app.view_state.library.collapsed {
        folded(app, ui, full);
        return;
    }
    let rail = Rect::from_min_size(full.min, vec2(RAIL.min(full.width() * 0.4), full.height()));
    let main = Rect::from_min_max(egui::pos2(rail.right(), full.top()), full.max);

    sidebar(app, ui, rail);
    ui.painter().line_segment(
        [rail.right_top(), rail.right_bottom()],
        Stroke::new(theme::LINE, theme::EDGE),
    );
    crate_pane(app, ui, main);
}

/* ── Splitter ────────────────────────────────────────────────────────── */

/// The grip between the decks and the library: drag it to give one more
/// room, double-click it to fold the library away or bring it back. The
/// arrows move it too, once it has focus.
fn splitter(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    let response = ui.interact(rect, egui::Id::new("library-splitter"), Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true,
        "Library size. Drag, or double-click to fold it away."));
    let hot = response.hovered() || response.dragged() || response.has_focus();
    if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    let space = app.view_state.console_height.max(1.0);
    let bottom = ui.max_rect().bottom();
    let library = &mut app.view_state.library;
    let mut moved = false;
    if response.double_clicked() {
        library.collapsed = !library.collapsed;
        moved = true;
    } else if response.dragged() {
        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
            // Dragged well down while open, it folds; dragged up while
            // folded, it opens where the pointer is.
            let share = (bottom - pointer.y) / space;
            if share < Library::LEAST * 0.6 {
                library.collapsed = true;
            } else {
                library.collapsed = false;
                library.set_share(share);
            }
        }
    }
    let steps = super::widgets::arrow_steps(ui, &response);
    if steps != 0.0 {
        library.collapsed = false;
        library.set_share(library.share + steps * 0.02);
        moved = true;
    }
    if moved || response.drag_stopped() {
        library.save(&app.root);
    }

    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme::GROUND);
    painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(theme::LINE, theme::EDGE));
    let ink = if hot { theme::TEXT_DIM } else { theme::EDGE_LIT };
    for dy in [-1.5, 1.5] {
        let y = rect.center().y + dy;
        painter.line_segment([egui::pos2(rect.center().x - 16.0, y), egui::pos2(rect.center().x + 16.0, y)],
                             Stroke::new(theme::LINE, ink));
    }
    if response.has_focus() {
        painter.rect_stroke(rect, theme::R_S, Stroke::new(theme::LINE_MID, theme::BLUE), egui::StrokeKind::Inside);
    }
    response.on_hover_text("Drag to resize the library. Double-click or Ctrl+L to fold it away.");
}

/// The library folded to one row: what it holds, and the way back.
fn folded(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, theme::PANEL);
    let mut bar = super::child(ui, rect.shrink2(vec2(theme::SP_3, 0.0)), super::left_row(), "bro-folded");
    bar.spacing_mut().item_spacing.x = theme::SP_2;
    if super::glyph_button(&mut bar, super::Glyph::Up, vec2(theme::CONTROL_S, theme::CONTROL_S),
                           Look::secondary(false, true), "Show the library")
        .on_hover_text("Show the library (Ctrl+L)").clicked() {
        super::toggle_library(app);
    }
    bar.label(RichText::new("Music").font(theme::display(theme::SIZE_L)).color(theme::TEXT_BRIGHT));
    bar.label(RichText::new(format!("{} tracks", app.records.len())).font(FontId::monospace(theme::SIZE_XS)).color(theme::TEXT_MUTE));
    if let Some(record) = app.selected.and_then(|i| app.records.get(i)) {
        bar.label(RichText::new(format!("Selected: {}", super::elide(&record.title, 40))).size(theme::SIZE_S).color(theme::TEXT_DIM));
    }
    let mut right = super::child(ui, rect.shrink2(vec2(theme::SP_3, 0.0)), super::right_row(), "bro-folded-r");
    right.label(RichText::new("Ctrl+L to show").font(FontId::monospace(theme::SIZE_XS)).color(theme::TEXT_MUTE));
}

/* ── Sidebar ─────────────────────────────────────────────────────────── */
fn sidebar(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, theme::GROUND);
    let inner = rect.shrink2(vec2(theme::SP_2, theme::SP_2));

    let mut column = super::child(ui, inner, Layout::top_down(Align::Min), "bro0");
    egui::ScrollArea::vertical().id_salt("library_sources").show(&mut column, |column| {
    column.spacing_mut().item_spacing.y = theme::SP_2 - 2.0;

    column.label(
        RichText::new("Library")
            .font(theme::display(theme::SIZE_M))
            .color(theme::TEXT_DIM),
    );

    // The one real source: everything the importer has put in the database.
    let (row, response) = column.allocate_exact_size(
        vec2(column.available_width(), 32.0),
        Sense::click(),
    );
    let selected = true;
    ui.painter().rect_filled(
        row,
        theme::R_M,
        if selected { theme::BLUE_DEEP } else if response.hovered() { theme::RAISED } else { theme::GROUND },
    );
    ui.painter().circle_filled(row.left_center() + vec2(11.0, 0.0), 5.0, theme::CYAN);
    ui.painter().text(
        row.left_center() + vec2(24.0, 0.0),
        Align2::LEFT_CENTER,
        "Music",
        FontId::proportional(theme::SIZE_S),
        if selected { theme::TEXT_BRIGHT } else { theme::TEXT },
    );
    ui.painter().text(
        row.right_center() - vec2(8.0, 0.0),
        Align2::RIGHT_CENTER,
        &format!("{}", app.records.len()),
        FontId::monospace(theme::SIZE_XS),
        if selected { theme::TEXT } else { theme::TEXT_MUTE },
    );

    column.add_space(theme::SP_1);
    let rescan_width = column.available_width().min(96.0);
    if super::chip(column, "Refresh", vec2(rescan_width, theme::CONTROL_S), false, true).on_hover_text("Reload imported tracks from the library").clicked() {
        app.reload_library();
    }

    if let Some(error) = &app.library_error {
        column.add_space(6.0);
        column.label(RichText::new(error).font(FontId::proportional(theme::SIZE_XS)).color(theme::RED));
    }

    column.add_space(theme::SP_4);
    requests(app, column);
    });
}

/// Ask for a record you do not have.
///
/// It lands in your music folder, tagged, and is imported the way anything
/// already in that folder is -- so the next line of this panel is the crate,
/// with the record in it. Nothing deletes it afterwards.
fn requests(app: &mut Defalt, ui: &mut Ui) {
    ui.label(
        RichText::new("Find a track")
            .font(theme::display(theme::SIZE_M))
            .color(theme::TEXT_DIM),
    );
    ui.add_space(theme::SP_1);

    if !app.can_pull() {
        ui.label(
            RichText::new("Needs the station's Python environment.")
                .font(FontId::proportional(theme::SIZE_XS))
                .color(theme::TEXT_MUTE),
        );
        return;
    }

    let width = ui.available_width();
    let field = ui.add(
        egui::TextEdit::singleline(&mut app.pull_query)
            .hint_text("Artist - Title or YouTube link")
            .desired_width(width)
            .font(FontId::proportional(theme::SIZE_S)),
    );

    // Typing invalidates a duration taken from an earlier suggestion: the
    // resolver must not be told the length of a record you have edited away
    // from.
    if field.changed() {
        app.pull_duration_ms = None;
        app.catalogue.typed(&app.pull_query.clone());
    }

    // Enter is how you finish typing a request; reaching for a button after
    // typing a name is a step nobody wants.
    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        app.begin_pull();
    }

    ui.add_space(theme::SP_1);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::SP_1;
        let ready = !app.pull_query.trim().is_empty();
        if super::chip(ui, "Get it", vec2(96.0f32.min(width), theme::CONTROL_S), false, ready).clicked() {
            app.begin_pull();
        }
        // The length is the useful half of taking a suggestion, so it is
        // worth showing that it was taken.
        if let Some(ms) = app.pull_duration_ms {
            let whole = ms / 1000;
            ui.label(
                RichText::new(format!("{}:{:02}", whole / 60, whole % 60))
                    .font(FontId::monospace(theme::SIZE_XS))
                    .color(theme::CYAN),
            )
            .on_hover_text("From the catalogue. The resolver uses it to reject the wrong upload.");
        }
    });

    suggestions(app, ui, width);

    if app.pulls.is_empty() {
        return;
    }
    ui.add_space(10.0);
    for job in &app.pulls {
        let (colour, mark) = match &job.stage {
            crate::pull::Stage::Failed { .. } => (theme::RED, "x"),
            crate::pull::Stage::Done { .. } => (theme::CYAN, "+"),
            _ => (theme::BLUE, ">"),
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.label(
                RichText::new(mark)
                    .font(FontId::monospace(theme::SIZE_XS))
                    .color(colour),
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    RichText::new(super::elide(&job.query, 24))
                        .font(FontId::proportional(theme::SIZE_XS))
                        .color(theme::TEXT),
                );
                ui.label(
                    RichText::new(super::elide(&job.stage.label(), 30))
                        .font(FontId::proportional(theme::SIZE_XS))
                        .color(colour),
                );
            });
        });
        ui.add_space(4.0);
    }
}

/* ── Crate ───────────────────────────────────────────────────────────── */
fn crate_pane(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), 44.0));
    let body = Rect::from_min_max(egui::pos2(rect.left(), head.bottom()), rect.max);

    let mut bar = super::child(ui, head.shrink2(vec2(theme::SP_3, 5.0)), super::left_row(), "bro1");
    bar.spacing_mut().item_spacing.x = theme::SP_2;
    if super::glyph_button(&mut bar, super::Glyph::Down, vec2(theme::CONTROL_S, theme::CONTROL_S),
                           Look::ghost(false, true), "Fold the library away")
        .on_hover_text("Fold the library away (Ctrl+L)").clicked() {
        super::toggle_library(app);
    }
    bar.label(
        RichText::new("Music")
            .font(theme::display(theme::SIZE_L))
            .color(theme::TEXT_BRIGHT),
    );

    let mut right = super::child(ui, head.shrink2(vec2(theme::SP_3, 5.0)), super::right_row(), "bro2");
    let search = egui::TextEdit::singleline(&mut app.search)
        .hint_text("Search title, artist or key")
        .desired_width(220.0)
        .font(FontId::proportional(theme::SIZE_M));
    let search = right.add(search);
    if app.focus_search {
        app.focus_search = false;
        search.request_focus();
    }

    right.add_space(theme::SP_2);
    let assist = super::chip(&mut right, "Assist", vec2(68.0, theme::CONTROL_S), app.assist, true);
    if assist.clicked() {
        app.assist = !app.assist;
        // Turning it on is only half an answer if the crate is still sorted
        // by artist, so it takes the ordering with it.
        if app.assist {
            app.sort = (Column::Match, true);
        } else if app.sort.0 == Column::Match {
            app.sort = (Column::Artist, true);
        }
    }
    assist.on_hover_text(
        "Match levels on load, and order the crate by what mixes with what is playing.",
    );
    let rows = super::rows(app);
    right.label(
        RichText::new(format!("{}", rows.len()))
            .font(FontId::monospace(theme::SIZE_XS))
            .color(theme::TEXT_MUTE),
    );

    ui.painter().line_segment(
        [head.left_bottom(), head.right_bottom()],
        Stroke::new(theme::LINE, theme::EDGE),
    );

    if app.records.is_empty() {
        empty_state(app, ui, body);
        return;
    }
    if rows.is_empty() {
        let mut empty = super::child(ui, body.shrink(24.0), Layout::top_down(Align::Center), "no_matches");
        empty.add_space(theme::SP_5);
        empty.label(RichText::new("No matching tracks").font(theme::display(theme::SIZE_L)).color(theme::TEXT));
        empty.label("Try another title, artist or Camelot key.");
        if super::chip(&mut empty, "Clear search", vec2(120.0, theme::CONTROL_M), false, true).clicked() {
            app.search.clear();
        }
        return;
    }
    table(app, ui, body, &rows);
}

fn empty_state(app: &Defalt, ui: &mut Ui, rect: Rect) {
    let centre = rect.center();
    ui.painter().text(
        centre - vec2(0.0, 14.0),
        Align2::CENTER_CENTER,
        "Your library is empty",
        theme::display(theme::SIZE_L),
        theme::TEXT,
    );
    if app.library_error.is_none() {
        ui.painter().text(
            centre + vec2(0.0, 12.0),
            Align2::CENTER_CENTER,
            "Import a music folder with radio.importer, then choose Refresh.",
            FontId::monospace(theme::SIZE_XS),
            theme::BLUE,
        );
    }
}

fn table(app: &mut Defalt, ui: &mut Ui, rect: Rect, rows: &[usize]) {
    let mut clicked: Option<usize> = None;
    let mut wanted: Option<(usize, usize)> = None;

    let mut area = super::child(ui, rect, Layout::top_down(Align::Min), "bro3");
    let spacing = area.spacing().item_spacing.x * 7.0 + 14.0;

    let mut table = TableBuilder::new(&mut area).sense(Sense::click());
    if app.scroll_to_selection {
        if let Some(row) = app.selected.and_then(|selected| rows.iter().position(|i| *i == selected)) {
            table = table.scroll_to_row(row, None);
        }
        app.scroll_to_selection = false;
    }
    // Title and artist share what the fixed columns leave, in proportion,
    // so a wide window does not open a gutter between them.
    const FIXED: [f32; 5] = [LOAD_COLUMN, 58.0, 60.0, 48.0, 70.0];
    let words = (rect.width() - FIXED.iter().sum::<f32>() - spacing).max(280.0);
    table
        .striped(true)
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(TableColumn::exact(LOAD_COLUMN))
        .column(TableColumn::exact((words * 0.56).round()).clip(true))
        .column(TableColumn::remainder().at_least(120.0).clip(true))
        .column(TableColumn::exact(FIXED[1]))
        .column(TableColumn::exact(FIXED[2]))
        .column(TableColumn::exact(FIXED[3]))
        .column(TableColumn::exact(FIXED[4]))
        .header(28.0, |mut header| {
            header.col(|ui| {
                ui.add_space(theme::SP_2);
                ui.label(RichText::new("LOAD").font(FontId::proportional(theme::SIZE_XS)).color(theme::TEXT_MUTE));
            });
            for (column, name) in [
                (Column::Title, "TITLE"),
                (Column::Artist, "ARTIST"),
                (Column::Duration, "TIME"),
                (Column::Bpm, "BPM"),
                (Column::Key, "KEY"),
                (Column::Match, "MATCH"),
            ] {
                header.col(|ui| {
                    let active = app.sort.0 == column;
                    let text = RichText::new(name)
                        .font(FontId::proportional(theme::SIZE_XS))
                        .color(if active { theme::TEXT_BRIGHT } else { theme::TEXT_MUTE });
                    let label = ui.add(egui::Label::new(text).sense(Sense::click()));
                    if active {
                        // A drawn chevron: up for ascending, down for
                        // descending, in the panel's accent.
                        let at = label.rect.right_center() + vec2(8.0, 0.0);
                        let glyph = if app.sort.1 { super::Glyph::Up } else { super::Glyph::Down };
                        super::draw_glyph(ui, glyph, at, theme::BLUE);
                    }
                    if label.clicked() {
                        if active {
                            app.sort.1 = !app.sort.1;
                        } else {
                            app.sort = (column, true);
                        }
                    }
                });
            }
        })
        .body(|body| {
            body.rows(32.0, rows.len(), |mut row| {
                // Taken before the first column: `row.col` needs `row`
                // mutably, so a closure that still reads it will not compile.
                let index = rows[row.index()];
                let record = &app.records[index];
                let selected = app.selected == Some(index);
                row.set_selected(selected);
                let loaded: Vec<usize> = (0..2)
                    .filter(|deck| app.decks[*deck].record.as_ref().is_some_and(|r| r.key == record.key))
                    .collect();

                row.col(|ui| {
                    // A record on a deck says which, down its left edge.
                    let cell = ui.max_rect();
                    let share = cell.height() / loaded.len().max(1) as f32;
                    for (at, deck) in loaded.iter().enumerate() {
                        let bar = Rect::from_min_size(cell.left_top() + vec2(0.0, at as f32 * share), vec2(3.0, share));
                        ui.painter().rect_filled(bar, 0.0, theme::DECK_COLOURS[*deck]);
                    }
                    ui.add_space(theme::SP_2);
                    for deck in 0..2 {
                        if load_key(ui, deck, loaded.contains(&deck), app.engine_ready()).clicked() {
                            wanted = Some((deck, index));
                        }
                    }
                });
                row.col(|ui| cell(ui, &record.title, theme::TEXT));
                row.col(|ui| cell(ui, &record.artist, theme::TEXT_DIM));
                row.col(|ui| {
                    number(ui, record.duration.map(super::mmss).unwrap_or_else(|| "--".into()));
                });
                row.col(|ui| {
                    number(ui, record.bpm.map_or("--".into(), |b| format!("{b:.1}")));
                });
                let fit = app.fit_for(index);
                row.col(|ui| {
                    // The key is tinted by how it sits against what is
                    // playing, so harmonic mixing is something you see rather
                    // than something you work out.
                    let colour = match fit.map(|f| f.key) {
                        Some(crate::assist::KeyFit::Same)
                        | Some(crate::assist::KeyFit::Neighbour)
                        | Some(crate::assist::KeyFit::Relative) => theme::CYAN,
                        Some(crate::assist::KeyFit::Clash) => theme::TEXT_MUTE,
                        _ => theme::TEXT_DIM,
                    };
                    ui.label(
                        RichText::new(record.camelot.clone().unwrap_or_else(|| "--".into()))
                            .font(FontId::monospace(theme::SIZE_XS))
                            .color(colour),
                    );
                });
                row.col(|ui| match fit {
                    Some(fit) => {
                        // Green when the pitch fader can make the match in
                        // a small move; everything else is just a number.
                        let colour = if match_is_close(fit.shift, fit.reachable) {
                            theme::GREEN
                        } else {
                            theme::TEXT_MUTE
                        };
                        ui.label(
                            RichText::new(fit.badge())
                                .font(FontId::monospace(theme::SIZE_XS))
                                .color(colour),
                        )
                        .on_hover_text(format!(
                            "{} - {}",
                            fit.key.label(),
                            match fit.shift {
                                Some(s) if fit.reachable =>
                                    format!("{s:+.1}% on the pitch fader"),
                                Some(s) => format!("{s:+.1}%, past the fader"),
                                None => "tempo unknown".to_string(),
                            }
                        ));
                    }
                    None => {
                        ui.label(
                            RichText::new("--")
                                .font(FontId::monospace(theme::SIZE_XS))
                                .color(theme::TEXT_MUTE),
                        );
                    }
                });

                if row.response().clicked() {
                    clicked = Some(index);
                }
            });
        });

    if let Some(index) = clicked {
        app.selected = Some(index);
    }
    if let Some((deck, index)) = wanted {
        let record = app.records[index].clone();
        app.selected = Some(index);
        app.load(deck, record);
    }
}

/// What the catalogue thinks you meant.
///
/// Clicking one does two things: it puts the catalogue's exact spelling in
/// the box, and it hands the resolver a duration. The second is the one that
/// matters -- on title alone a record and a documentary about the record
/// score the same.
fn suggestions(app: &mut Defalt, ui: &mut Ui, width: f32) {
    super::suggestion_status(ui, &app.catalogue, !app.pull_query.trim().is_empty());
    if app.catalogue.showing.is_empty() || app.catalogue.error.is_some() {
        return;
    }
    ui.add_space(6.0);
    if let Some(at) = super::suggestion_list(ui, &app.catalogue.showing, width, usize::MAX) {
        app.take_suggestion(at);
    }
}

/// The load column: a lamp strip and two keys.
const LOAD_COLUMN: f32 = 88.0;

/// Within this many percent of the playing tempo, a record is an easy mix.
const CLOSE_MATCH: f64 = 3.0;

fn match_is_close(shift: Option<f64>, reachable: bool) -> bool {
    reachable && shift.is_some_and(|s| s.abs() <= CLOSE_MATCH)
}

/// A row's load key: quiet until the pointer or focus reaches it, then in
/// its deck's colour; lit in that colour while the record is on the deck.
fn load_key(ui: &mut Ui, deck: usize, loaded: bool, live: bool) -> egui::Response {
    let colour = theme::DECK_COLOURS[deck];
    let letter = theme::DECK_LETTERS[deck];
    let size = vec2(34.0, theme::CONTROL_S);
    let (rect, response) = ui.allocate_exact_size(size, if live { Sense::click() } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, live, loaded,
                                                         format!("Load on deck {letter}")));
    let hovered = live && (response.hovered() || response.has_focus());
    let look = if loaded { Look::secondary(true, live) } else { Look::ghost(false, live) }.accent(colour);
    let look = if hovered && !loaded { look.outlined() } else { look };
    let ink = super::paint_control(ui, rect, look, hovered, live && response.is_pointer_button_down_on());
    let ink = if hovered && !loaded { colour } else { ink };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, letter, theme::display(theme::SIZE_S), ink);
    if response.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), theme::R_M + 2.0, Stroke::new(theme::LINE_MID, theme::BLUE),
                                 egui::StrokeKind::Outside);
    }
    response.on_hover_text(format!("Load on deck {letter}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_small_reachable_tempo_move_is_called_a_match() {
        assert!(match_is_close(Some(0.0), true));
        assert!(match_is_close(Some(-3.0), true));
        assert!(!match_is_close(Some(3.1), true));
        assert!(!match_is_close(Some(1.0), false), "past the fader is never a match");
        assert!(!match_is_close(None, true));
    }
}

fn cell(ui: &mut Ui, text: &str, colour: egui::Color32) {
    ui.add(egui::Label::new(RichText::new(text).size(theme::SIZE_M).color(colour)).truncate()).on_hover_text(text);
}

fn number(ui: &mut Ui, text: String) {
    ui.label(
        RichText::new(text)
            .font(FontId::monospace(theme::SIZE_S))
            .color(theme::TEXT_DIM),
    );
}
