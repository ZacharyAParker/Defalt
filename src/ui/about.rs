//! Release information is embedded so these pages also work with the station off.
use egui::{Align, Layout, RichText, Ui};

use super::theme;
use crate::Defalt;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COPYRIGHT: &str = "© 2026 Zachary Parker";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page { Patches, Privacy, Terms, Copyright }

impl Page {
    pub const ALL: [Self; 4] = [Self::Patches, Self::Privacy, Self::Terms, Self::Copyright];
    fn label(self) -> &'static str {
        match self { Self::Patches => "Patches", Self::Privacy => "Privacy", Self::Terms => "Terms", Self::Copyright => "Copyright" }
    }
    fn content(self) -> &'static str {
        match self {
            Self::Patches => include_str!("../../CHANGELOG.md"),
            Self::Privacy => include_str!("../../PRIVACY.md"),
            Self::Terms => include_str!("../../TERMS.md"),
            Self::Copyright => include_str!("../../COPYRIGHT.md"),
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|page| page.label().eq_ignore_ascii_case(name))
    }
}

/// The height of the strip along the foot of the window.
pub const FOOTER: f32 = 28.0;

pub fn footer(app: &mut Defalt, ui: &mut Ui) {
    egui::Panel::bottom("release_footer")
        .exact_size(FOOTER)
        .frame(egui::Frame::NONE.fill(theme::GROUND).inner_margin(egui::Margin::symmetric(12, 4)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(format!("Defalt v{VERSION}  ·  {COPYRIGHT}")).size(theme::SIZE_S).color(theme::TEXT_DIM));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    super::feedback::footer_button(app, ui);
                    for page in Page::ALL.into_iter().rev() {
                        if ui.add(egui::Button::new(RichText::new(page.label()).size(theme::SIZE_S)).frame(false)).clicked() {
                            app.info_page = Some(page);
                        }
                    }
                });
            });
        });
}

pub fn overlay(app: &mut Defalt, ctx: &egui::Context) {
    let Some(mut page) = app.info_page else { return; };
    crate::keys::release_bends(app);
    let bounds = ctx.content_rect();
    let mut close = false;
    let response = egui::Modal::new(egui::Id::new("release_information"))
        .frame(egui::Frame::popup(&ctx.style_of(egui::Theme::Dark)).fill(theme::PANEL).inner_margin(20))
        .show(ctx, |ui| {
            ui.set_width((bounds.width() - 72.0).clamp(240.0, 720.0));
            ui.horizontal(|ui| {
                ui.heading("Defalt");
                ui.label(RichText::new(format!("v{VERSION}")).color(theme::TEXT_DIM));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = ui.button("Close").clicked();
                });
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for candidate in Page::ALL {
                    ui.selectable_value(&mut page, candidate, candidate.label());
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt(page.label())
                .max_height((bounds.height() - 230.0).max(120.0))
                .auto_shrink([false, false])
                .show(ui, |ui| document(ui, page.content()));
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("v{VERSION}  ·  {COPYRIGHT}")).size(theme::SIZE_S).color(theme::TEXT_DIM));
                ui.hyperlink_to("Repository", "https://github.com/ZacharyAParker/Defalt");
            });
        });
    app.info_page = if close || response.should_close() { None } else { Some(page) };
}

fn document(ui: &mut Ui, text: &str) {
    // The repository pages deliberately use headings, paragraphs and bullets.
    // Render as selectable text, never as HTML or executable content.
    for line in text.split("\n---\n").next().unwrap_or(text).lines() {
        let line = line.trim();
        if line.is_empty() { ui.add_space(6.0); }
        else if let Some(title) = line.strip_prefix("### ") {
            ui.add_space(8.0);
            ui.label(RichText::new(title).size(theme::SIZE_L).strong());
        } else if let Some(title) = line.strip_prefix("## ") {
            ui.add_space(10.0);
            ui.label(RichText::new(title).size(theme::SIZE_L).strong());
        } else if let Some(title) = line.strip_prefix("# ") {
            ui.label(RichText::new(title).size(theme::SIZE_XXL).strong());
        } else if let Some(item) = line.strip_prefix("- ") {
            ui.horizontal_top(|ui| {
                ui.label("•");
                ui.add(egui::Label::new(RichText::new(item).size(theme::SIZE_M)).wrap().selectable(true));
            });
            ui.add_space(4.0);
        } else {
            ui.add(egui::Label::new(RichText::new(line).size(theme::SIZE_M)).wrap().selectable(true));
        }
    }
}
