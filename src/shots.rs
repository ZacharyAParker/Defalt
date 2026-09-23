//! Self-portraits: `DEFALT_SHOT=<png>` poses the panel once it has settled,
//! saves a screenshot and quits; F12 saves one any time into `target/`.

use std::path::PathBuf;
use std::sync::Arc;

use crate::{station, ui, Defalt, View};

impl Defalt {
    pub(crate) fn screenshots(&mut self, ctx: &egui::Context) {
        self.frames += 1;

        // Capture the real local library without starting playback, network
        // searches, stem separation, or a station just to take a screenshot.
        let posing = self.shot_on_launch.is_some();
        if posing && self.frames == 5 {
            self.wait_for_library();
            if std::env::var_os("DEFALT_SHOT_EMPTY").is_none() {
                for deck in 0..self.records.len().min(2) {
                    self.load(deck, self.records[deck].clone());
                }
            }
            if std::env::var_os("DEFALT_SHOT_RADIO").is_some() { self.view = View::Radio; }
            if let Ok(pose) = std::env::var("DEFALT_SHOT_STUDIO") {
                self.view = View::Radio;
                self.studio.pose(&pose);
            }
            if std::env::var_os("DEFALT_SHOT_SPECTRUM").is_some() {
                self.view = View::Radio;
                self.studio.visualizer = true;
                self.studio.spectrum.pose();
            }
            if std::env::var_os("DEFALT_SHOT_CHAT").is_some() {
                self.view = View::Radio;
                self.airtime.chat.open = true;
                self.airtime.chat.preview = true;
                if let Some(path)=std::env::var_os("DEFALT_SHOT_CHAT_DRAFT") {
                    self.airtime.chat.draft=std::fs::read_to_string(path).unwrap_or_default();
                }
                self.airtime.chat.state = serde_json::json!({"messages":[
                    {"role":"user","text":"Keep this energy, but less rap."},
                    {"role":"director","text":"For this session: mellow soul and funk. This starts with unprepared automatic picks; current songs, prepared transitions and your requests stay in place."},
                    {"role":"user","text":"Less talking for twenty minutes."},
                    {"role":"director","text":"Fewer automatic host breaks for 20 minutes. Already prepared speech and explicitly requested segments still play."}],
                    "direction":{"description":"Mellow soul and funk"},"quiet_minutes":20,"busy":false});
            }
            if let Ok(page) = std::env::var("DEFALT_SHOT_INFO") {
                self.info_page = ui::about::Page::from_name(&page);
            }
            if std::env::var_os("DEFALT_SHOT_ARTICLE").is_some() {
                self.airtime.request_is_article = true;
                self.airtime.article = "https://www.mindstudio.ai/blog/gemini-4-release-date-rumors".into();
                self.view = View::Radio;
            }
            if let Some(path) = std::env::var_os("DEFALT_SHOT_STATUS") {
                if let Ok(body) = std::fs::read(path).ok().and_then(|s| serde_json::from_slice(&s).ok()).ok_or(()) {
                    let status = station::on_air_from(&body);
                    self.mix_settings = status.mix_config.clone();
                    self.station.health = station::Health::Live(Box::new(status));
                    self.view = View::Radio;
                    self.mix_settings_open = std::env::var_os("DEFALT_SHOT_SETTINGS").is_some();
                }
            }
            if std::env::var_os("DEFALT_SHOT_FOLDED").is_some() {
                self.view_state.library.collapsed = true;
            }
            if std::env::var_os("DEFALT_SHOT_RACKS").is_some() {
                self.show_grid = true;
                self.show_stems = true;
            }
        }
        let settled = self.decks.iter().all(|d| !d.loading);
        if posing && settled && self.frames > 5 && !self.posed {
            self.posed = true;
            self.pose_frame = self.frames;
            for deck in 0..2 { self.seek(deck, self.decks[deck].length * 0.25); }
            if std::env::var_os("DEFALT_SHOT_TRANSITIONS").is_some()
                && self.decks.iter().all(|d| d.length > 16.0) {
                let lengths = [self.decks[0].length, self.decks[1].length];
                self.airtime.pose_transition_pair(lengths);
                self.seek(0, lengths[0] - 12.0);
                self.seek(1, 0.0);
            }
        }
        // A missing decoder or empty library cannot hold capture open forever.
        let ready = posing && ((self.posed && self.frames >= self.pose_frame + 12) || self.frames >= 600);
        let launch_shot = ready && !self.asked_for_shot;
        if launch_shot {
            self.asked_for_shot = true;
        }
        let manual = ctx.input(|i| i.key_pressed(egui::Key::F12));
        if launch_shot || manual {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }

        let shots: Vec<Arc<egui::ColorImage>> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Screenshot { image, user_data, .. } if !ui::feedback::is_ours(user_data) => Some(image.clone()),
                    _ => None,
                })
                .collect()
        });

        for image in shots {
            let path: PathBuf = match self.shot_on_launch.take() {
                Some(path) => path,
                None => {
                    self.shots += 1;
                    self.root.join("target").join(format!("shot-{}.png", self.shots))
                }
            };
            match save_png(&image, &path) {
                Ok(()) => println!("screenshot: {}", path.display()),
                Err(error) => eprintln!("screenshot failed: {error}"),
            }
            if std::env::var_os("DEFALT_SHOT").is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

pub fn save_png(image: &egui::ColorImage, path: &std::path::Path) -> Result<(), String> {
    let [width, height] = image.size;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_array());
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    image::save_buffer(path, &rgba, width as u32, height as u32, image::ColorType::Rgba8)
        .map_err(|error| error.to_string())
}
