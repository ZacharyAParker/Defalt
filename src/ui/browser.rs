//! The browser: a source rail on the left, the crate on the right.
//!
//! Selecting a row here is what the load buttons in the overview strip act
//! on, so picking a record and choosing a deck are two separate decisions --
//! which is how you audition something without committing it to a deck.

use egui::{vec2, Align, Align2, FontId, Layout, Rect, RichText, Sense, Stroke, Ui};
use egui_extras::{Column as TableColumn, TableBuilder};

use super::{theme, Column};
use crate::Defalt;

const RAIL: f32 = 224.0;

pub fn draw(app: &mut Defalt, ui: &mut Ui) {
    let full = ui.max_rect();
    let rail = Rect::from_min_size(full.min, vec2(RAIL.min(full.width() * 0.4), full.height()));
    let main = Rect::from_min_max(egui::pos2(rail.right(), full.top()), full.max);

    sidebar(app, ui, rail);
    ui.painter().line_segment(
        [rail.right_top(), rail.right_bottom()],
        Stroke::new(1.0, theme::EDGE),
    );
    crate_pane(app, ui, main);
}

/* ── Sidebar ─────────────────────────────────────────────────────────── */
fn sidebar(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, theme::GROUND);
    let inner = rect.shrink2(vec2(8.0, 8.0));

    let mut column = super::child(ui, inner, Layout::top_down(Align::Min), "bro0");
    egui::ScrollArea::vertical().id_salt("library_sources").show(&mut column, |column| {
    column.spacing_mut().item_spacing.y = 6.0;

    column.label(
        RichText::new("Library")
            .font(FontId::proportional(14.0))
            .color(theme::TEXT_MUTE),
    );

    // The one real source: everything the importer has put in the database.
    let (row, response) = column.allocate_exact_size(
        vec2(column.available_width(), 32.0),
        Sense::click(),
    );
    let selected = true;
    ui.painter().rect_filled(
        row,
        4.0,
        if selected { theme::BLUE_DEEP } else if response.hovered() { theme::RAISED } else { theme::GROUND },
    );
    ui.painter().circle_filled(row.left_center() + vec2(11.0, 0.0), 5.0, theme::CYAN);
    ui.painter().text(
        row.left_center() + vec2(24.0, 0.0),
        Align2::LEFT_CENTER,
        "Music",
        FontId::proportional(11.5),
        if selected { theme::TEXT_BRIGHT } else { theme::TEXT },
    );
    ui.painter().text(
        row.right_center() - vec2(8.0, 0.0),
        Align2::RIGHT_CENTER,
        &format!("{}", app.records.len()),
        FontId::monospace(9.5),
        if selected { theme::TEXT } else { theme::TEXT_MUTE },
    );

    column.add_space(4.0);
    let rescan_width = column.available_width().min(96.0);
    if super::chip(column, "Refresh", vec2(rescan_width, 28.0), false, true).on_hover_text("Reload imported tracks from the library").clicked() {
        app.reload_library();
    }

    if let Some(error) = &app.library_error {
        column.add_space(6.0);
        column.label(RichText::new(error).font(FontId::proportional(10.0)).color(theme::RED));
    }

    column.add_space(12.0);
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
            .font(FontId::proportional(14.0))
            .color(theme::TEXT_MUTE),
    );
    ui.add_space(3.0);

    if !app.can_pull() {
        ui.label(
            RichText::new("Needs the station's Python environment.")
                .font(FontId::proportional(10.0))
                .color(theme::TEXT_MUTE),
        );
        return;
    }

    let width = ui.available_width();
    let field = ui.add(
        egui::TextEdit::singleline(&mut app.pull_query)
            .hint_text("Artist - Title or YouTube link")
            .desired_width(width)
            .font(FontId::proportional(11.0)),
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

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let ready = !app.pull_query.trim().is_empty();
        if super::chip(ui, "Get it", vec2(76.0, 20.0), false, ready).clicked() {
            app.begin_pull();
        }
        // The length is the useful half of taking a suggestion, so it is
        // worth showing that it was taken.
        if let Some(ms) = app.pull_duration_ms {
            let whole = ms / 1000;
            ui.label(
                RichText::new(format!("{}:{:02}", whole / 60, whole % 60))
                    .font(FontId::monospace(9.5))
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
                    .font(FontId::monospace(10.0))
                    .color(colour),
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    RichText::new(super::elide(&job.query, 24))
                        .font(FontId::proportional(10.5))
                        .color(theme::TEXT),
                );
                ui.label(
                    RichText::new(super::elide(&job.stage.label(), 30))
                        .font(FontId::proportional(9.5))
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

    let mut bar = super::child(ui, head.shrink2(vec2(10.0, 5.0)), super::left_row(), "bro1");
    bar.label(
        RichText::new("Music")
            .font(FontId::proportional(16.0))
            .color(theme::TEXT),
    );

    let mut right = super::child(ui, head.shrink2(vec2(10.0, 5.0)), super::right_row(), "bro2");
    let search = egui::TextEdit::singleline(&mut app.search)
        .hint_text("Search title, artist or key")
        .desired_width(220.0)
        .font(FontId::proportional(13.0));
    let search = right.add(search);
    if app.focus_search {
        app.focus_search = false;
        search.request_focus();
    }

    right.add_space(8.0);
    let assist = super::chip(&mut right, "Assist", vec2(62.0, 28.0), app.assist, true);
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
    right.label(
        RichText::new(format!("{}", super::filtered(app).len()))
            .font(FontId::monospace(9.5))
            .color(theme::TEXT_MUTE),
    );

    ui.painter().line_segment(
        [head.left_bottom(), head.right_bottom()],
        Stroke::new(1.0, theme::EDGE),
    );

    if app.records.is_empty() {
        empty_state(app, ui, body);
        return;
    }
    if super::filtered(app).is_empty() {
        let mut empty = super::child(ui, body.shrink(24.0), Layout::top_down(Align::Center), "no_matches");
        empty.add_space(22.0);
        empty.label(super::rich("No matching tracks", 16.0, theme::TEXT));
        empty.label("Try another title, artist or Camelot key.");
        if super::chip(&mut empty, "Clear search", vec2(110.0, 30.0), false, true).clicked() {
            app.search.clear();
        }
        return;
    }
    table(app, ui, body);
}

fn empty_state(app: &Defalt, ui: &mut Ui, rect: Rect) {
    let centre = rect.center();
    ui.painter().text(
        centre - vec2(0.0, 12.0),
        Align2::CENTER_CENTER,
        "Your library is empty",
        FontId::proportional(14.0),
        theme::TEXT_MUTE,
    );
    if app.library_error.is_none() {
        ui.painter().text(
            centre + vec2(0.0, 10.0),
            Align2::CENTER_CENTER,
            "Import a music folder with radio.importer, then choose Refresh.",
            FontId::monospace(10.5),
            theme::BLUE,
        );
    }
}

fn table(app: &mut Defalt, ui: &mut Ui, rect: Rect) {
    let rows = super::filtered(app);
    let mut clicked: Option<usize> = None;
    let mut wanted: Option<(usize, usize)> = None;

    let mut area = super::child(ui, rect, Layout::top_down(Align::Min), "bro3");

    let mut table = TableBuilder::new(&mut area).sense(Sense::click());
    if app.scroll_to_selection {
        if let Some(row) = app.selected.and_then(|selected| rows.iter().position(|i| *i == selected)) {
            table = table.scroll_to_row(row, None);
        }
        app.scroll_to_selection = false;
    }
    table
        .striped(true)
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(TableColumn::exact(76.0))
        .column(TableColumn::initial((rect.width() * 0.34).max(160.0)).at_least(160.0).clip(true))
        .column(TableColumn::remainder().at_least(120.0).clip(true))
        .column(TableColumn::exact(54.0))
        .column(TableColumn::exact(54.0))
        .column(TableColumn::exact(44.0))
        .column(TableColumn::exact(62.0))
        .header(28.0, |mut header| {
            header.col(|ui| {
                ui.label(RichText::new("LOAD").font(FontId::proportional(10.5)).color(theme::TEXT_DIM));
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
                    let title = if active { format!("{} {}", name, if app.sort.1 { "+" } else { "-" }) } else { name.to_owned() };
                    let text = RichText::new(title)
                        .font(FontId::proportional(10.5))
                        .color(if active { theme::BLUE } else { theme::TEXT_MUTE });
                    if ui.add(egui::Label::new(text).sense(Sense::click())).clicked() {
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

                row.col(|ui| {
                    for deck in 0..2 {
                        let label = if deck == 0 { "A" } else { "B" };
                        let hit = super::chip(
                            ui,
                            label,
                            vec2(32.0, 24.0),
                            app.decks[deck].record.as_ref().is_some_and(|r| r.key == record.key),
                            app.engine_ready(),
                        );
                        if hit.clicked() {
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
                            .font(FontId::monospace(10.0))
                            .color(colour),
                    );
                });
                row.col(|ui| match fit {
                    Some(fit) => {
                        let colour = if !fit.reachable {
                            theme::TEXT_MUTE
                        } else if fit.score > 0.8 {
                            theme::CYAN
                        } else if fit.score > 0.55 {
                            theme::BLUE
                        } else {
                            theme::TEXT_DIM
                        };
                        ui.label(
                            RichText::new(fit.badge())
                                .font(FontId::monospace(10.0))
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
                                .font(FontId::monospace(10.0))
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
    if !app.catalogue.available() {
        if !app.pull_query.trim().is_empty() {
            ui.add_space(4.0);
            ui.label(
                RichText::new("No Spotify credentials, so no suggestions.")
                    .font(FontId::proportional(9.5))
                    .color(theme::TEXT_MUTE),
            );
        }
        return;
    }

    if let Some(error) = &app.catalogue.error {
        ui.add_space(4.0);
        ui.label(
            RichText::new(error)
                .font(FontId::proportional(9.5))
                .color(theme::RED),
        );
        return;
    }

    if app.catalogue.showing.is_empty() {
        if app.catalogue.busy {
            ui.add_space(4.0);
            ui.label(
                RichText::new("searching...")
                    .font(FontId::proportional(9.5))
                    .color(theme::TEXT_MUTE),
            );
        }
        return;
    }

    ui.add_space(6.0);
    let mut chosen: Option<usize> = None;
    for (at, found) in app.catalogue.showing.iter().enumerate() {
        let (rect, response) = ui.allocate_exact_size(
            vec2(width, 30.0),
            Sense::click(),
        );
        if response.hovered() {
            ui.painter().rect_filled(rect, 4.0, theme::RAISED);
        }
        let inner = rect.shrink2(vec2(6.0, 3.0));

        ui.painter().text(
            inner.left_top(),
            Align2::LEFT_TOP,
            super::elide(&found.title, 26),
            FontId::proportional(10.5),
            theme::TEXT,
        );
        ui.painter().text(
            inner.left_bottom() - vec2(0.0, 11.0),
            Align2::LEFT_TOP,
            super::elide(&found.artist, 28),
            FontId::proportional(9.5),
            theme::TEXT_DIM,
        );
        ui.painter().text(
            inner.right_top(),
            Align2::RIGHT_TOP,
            found.length(),
            FontId::monospace(9.5),
            theme::TEXT_MUTE,
        );
        if let Some(year) = &found.year {
            ui.painter().text(
                inner.right_bottom() - vec2(0.0, 11.0),
                Align2::RIGHT_TOP,
                year,
                FontId::monospace(9.0),
                theme::TEXT_MUTE,
            );
        }

        if response.clicked() {
            chosen = Some(at);
        }
        response.on_hover_text(match &found.album {
            Some(album) => format!("{album} - {}", found.length()),
            None => found.length(),
        });
    }

    if let Some(at) = chosen {
        app.take_suggestion(at);
    }
}

fn cell(ui: &mut Ui, text: &str, colour: egui::Color32) {
    ui.add(egui::Label::new(RichText::new(text).size(13.0).color(colour)).truncate()).on_hover_text(text);
}

fn number(ui: &mut Ui, text: String) {
    ui.label(
        RichText::new(text)
            .font(FontId::monospace(11.5))
            .color(theme::TEXT_DIM),
    );
}
