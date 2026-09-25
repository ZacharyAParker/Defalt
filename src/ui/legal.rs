//! The first-run notice. It sits over a dimmed console until you agree or
//! quit, and comes back only when TERMS.md gets a new terms version.

use egui::{vec2, Align, Color32, Layout, RichText, Ui};

use super::{about::Page, theme, Look};
use crate::Defalt;

/// Draws the notice if it is due. True when it was drawn.
pub fn overlay(app: &mut Defalt, ctx: &egui::Context) -> bool {
    // A page opened from the notice is read on its own; the notice is back
    // as soon as that page closes.
    if app.legal.accepted() || app.info_page.is_some() {
        return false;
    }
    crate::keys::release_bends(app);
    let bounds = ctx.content_rect();
    let id = egui::Id::new("first_run_notice");
    let mut agree = false;
    let mut quit = false;
    egui::Modal::new(id)
        .area(egui::Modal::default_area(id).fade_in(false))
        .backdrop_color(Color32::from_black_alpha(200))
        .frame(egui::Frame::popup(&ctx.style_of(egui::Theme::Dark)).fill(theme::PANEL).inner_margin(24))
        .show(ctx, |ui| {
            ui.set_width((bounds.width() - 72.0).clamp(300.0, 600.0));
            egui::ScrollArea::vertical()
                .max_height((bounds.height() - 150.0).max(160.0))
                .auto_shrink([false, true])
                .show(ui, |ui| body(app, ui));
            ui.add_space(theme::SP_3);
            ui.separator();
            ui.add_space(theme::SP_2);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Terms version {}", crate::legal::TERMS_VERSION))
                    .size(theme::SIZE_XS).color(theme::TEXT_MUTE));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    agree = super::button(ui, "I agree", vec2(112.0, theme::CONTROL_M), Look::primary(true)).clicked();
                    quit = super::button(ui, "Quit", vec2(80.0, theme::CONTROL_M), Look::secondary(false, true)).clicked();
                });
            });
        });
    if agree {
        if let Err(error) = app.legal.accept() {
            crate::logfile::log!("legal: couldn't save the acceptance: {error}");
        }
    }
    if quit {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    true
}

fn body(app: &mut Defalt, ui: &mut Ui) {
    ui.label(RichText::new("Before you start").font(theme::display(theme::SIZE_XXL)).color(theme::TEXT_BRIGHT));
    ui.add_space(theme::SP_2);
    text(ui, "Defalt is a personal DJ console and radio. By using it you accept the Terms of use, the License and the Privacy policy. It comes with no warranty, and you're responsible for what you download, play and share with it.");
    ui.add_space(theme::SP_1);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = theme::SP_4;
        for (label, page) in [("Terms of use", Page::Terms), ("License", Page::License), ("Privacy policy", Page::Privacy)] {
            if ui.link(RichText::new(label).size(theme::SIZE_M).color(theme::AMBER)).clicked() {
                app.info_page = Some(page);
            }
        }
    });

    heading(ui, "Flashing lights");
    text(ui, "The radio booth has lightning, a flickering neon sign and a visualizer that pulses with the music. If you or anyone watching is sensitive to flashing light or has ever had a seizure, turn on Reduced motion. Stop right away if you feel dizzy or unwell.");
    let reduced = ui.checkbox(&mut app.studio.reduced,
                              RichText::new("Reduced motion (stops the lightning and flicker)").size(theme::SIZE_M));
    if reduced.changed() {
        app.studio.save();
    }

    heading(ui, "Volume");
    text(ui, "Start quiet and keep it at a safe level, especially on headphones. Mixes and voices can change loudness suddenly, and the limiter doesn't protect your hearing.");

    heading(ui, "Explicit content");
    text(ui, "Songs play in their explicit versions, and the hosts roast, swear and joke. What they say, including the fake ads, is generated, can be wrong or offensive, and isn't anyone's real view or endorsement.");
}

fn heading(ui: &mut Ui, title: &str) {
    ui.add_space(theme::SP_4);
    ui.label(RichText::new(title).size(theme::SIZE_L).strong().color(theme::TEXT_BRIGHT));
    ui.add_space(theme::SP_1);
}

fn text(ui: &mut Ui, line: &str) {
    ui.add(egui::Label::new(RichText::new(line).size(theme::SIZE_M).color(theme::TEXT)).wrap());
}
