//! Self-portraits: `DEFALT_SHOT=<png>` poses the panel once it has settled,
//! saves a screenshot and quits; F12 saves one any time into `target/`.
//!
//! With `DEFALT_SHOT_STUDIO=<pose>` and `DEFALT_SHOT_FRAMES=<n>` it instead
//! plays the booth from a script, a thirtieth of a second a frame, and saves
//! `n` consecutive frames of just the booth into `target/anim/<pose>/` (or
//! `DEFALT_SHOT_FRAMES_DIR`), then quits.

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
        // DEFALT_SHOT_NOTICE holds a notice up for the shot. A short one goes
        // first, the way a real session has already said something small.
        if posing {
            if let Ok(text) = std::env::var("DEFALT_SHOT_NOTICE") {
                self.say(if self.posed && self.frames > self.pose_frame + 3 { &text } else { "Skipped." });
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

        let frames = std::env::var("DEFALT_SHOT_FRAMES").ok().and_then(|n| n.parse::<usize>().ok()).filter(|_| posing);
        if let Some(total) = frames {
            for image in shots {
                self.save_booth_frame(&image, ctx);
                self.shot_frames += 1;
                if self.shot_frames >= total {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                self.studio.advance(1. / 30.);
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                ctx.request_repaint();
            }
            return;
        }
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

impl Defalt {
    /// One frame of a booth recording: the scene only, numbered.
    fn save_booth_frame(&self, image: &egui::ColorImage, ctx: &egui::Context) {
        let pose = std::env::var("DEFALT_SHOT_STUDIO").unwrap_or_else(|_| "booth".into());
        let dir = std::env::var_os("DEFALT_SHOT_FRAMES_DIR").map(PathBuf::from)
            .unwrap_or_else(|| self.root.join("target").join("anim").join(&pose));
        let path = dir.join(format!("frame_{:04}.png", self.shot_frames));
        let scale = ctx.pixels_per_point();
        let crop = self.studio.drawn.map(|r| {
            let [w, h] = image.size;
            let x0 = ((r.min.x * scale).round().max(0.) as usize).min(w);
            let y0 = ((r.min.y * scale).round().max(0.) as usize).min(h);
            let x1 = ((r.max.x * scale).round() as usize).clamp(x0, w);
            let y1 = ((r.max.y * scale).round() as usize).clamp(y0, h);
            (x0, y0, x1, y1)
        });
        let saved = match crop {
            Some((x0, y0, x1, y1)) if x1 > x0 && y1 > y0 => {
                let mut pixels = Vec::with_capacity((x1 - x0) * (y1 - y0));
                for y in y0..y1 {
                    pixels.extend_from_slice(&image.pixels[y * image.size[0] + x0..y * image.size[0] + x1]);
                }
                save_png(&egui::ColorImage::new([x1 - x0, y1 - y0], pixels), &path)
            }
            _ => save_png(image, &path),
        };
        if let Err(error) = saved {
            eprintln!("booth frame failed: {error}");
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
