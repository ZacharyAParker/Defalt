//! The live booth. Drawing never advances or schedules audio.
use super::theme;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke, TextureHandle, Ui};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

const AMBER: Color32 = Color32::from_rgb(239, 189, 113);
const CAT: [f32; 4] = [52., 437., 168., 118.];
const MOUTHS: [[f32; 4]; 2] = [[464., 421., 70., 42.], [1005., 462., 67., 44.]];
const EYES: [[f32; 4]; 2] = [[433., 352., 151., 48.], [978., 397., 149., 55.]];
const POSES: [&[u8]; 3] = [
    include_bytes!("../../web/static/studio/cat-awake.png"),
    include_bytes!("../../web/static/studio/cat-yawn.png"),
    include_bytes!("../../web/static/studio/cat-groom.png"),
];

struct Art {
    base: TextureHandle,
    mouths: Vec<TextureHandle>,
    eyes: Vec<TextureHandle>,
    cats: Vec<TextureHandle>,
    body: TextureHandle,
}
type Cover = (String, Option<(egui::ColorImage, String)>);
pub struct Studio {
    pub enabled: bool,
    rain: bool,
    lights: bool,
    cat: bool,
    reduced: bool,
    preferences: PathBuf,
    art: Option<Art>,
    clock: f32,
    last: Instant,
    holds: [f32; 2],
    next_cat: f32,
    cat_start: f32,
    routine: usize,
    next_routine: usize,
    cover_key: String,
    cover: Option<TextureHandle>,
    cover_source: String,
    cover_in: mpsc::Receiver<Cover>,
    cover_out: mpsc::Sender<Cover>,
}
impl Studio {
    pub fn new(root: &Path) -> Self {
        let preferences = root.join("cache/studio-preferences.json");
        let settings = std::fs::read(&preferences)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .unwrap_or_default();
        let (cover_out, cover_in) = mpsc::channel();
        Self {
            enabled: settings["enabled"].as_bool().unwrap_or(true),
            rain: settings["rain"].as_bool().unwrap_or(true),
            lights: settings["lights"].as_bool().unwrap_or(true),
            cat: settings["cat"].as_bool().unwrap_or(true),
            reduced: settings["reduced"].as_bool().unwrap_or(false),
            preferences,
            art: None,
            clock: 0.,
            last: Instant::now(),
            holds: [0.; 2],
            next_cat: 120.,
            cat_start: 0.,
            routine: usize::MAX,
            next_routine: 0,
            cover_key: String::new(),
            cover: None,
            cover_source: String::new(),
            cover_in,
            cover_out,
        }
    }
    pub fn save(&self) {
        if let Some(parent) = self.preferences.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let data = serde_json::json!({"enabled":self.enabled,"rain":self.rain,"lights":self.lights,"cat":self.cat,"reduced":self.reduced});
        let _ = std::fs::write(&self.preferences, data.to_string());
    }
    fn artwork(&mut self, ctx: &egui::Context, key: &str, url: &str) {
        if key != self.cover_key {
            self.cover_key = key.to_owned();
            self.cover = None;
            self.cover_source.clear();
            if !key.is_empty() {
                let key = key.to_owned();
                let sender = self.cover_out.clone();
                let ctx = ctx.clone();
                let encoded = key.bytes().map(|b| format!("%{b:02X}")).collect::<String>();
                let url = format!("{url}/api/artwork?key={encoded}");
                std::thread::spawn(move || {
                    let result = (|| {
                        use std::io::Read;
                        let agent = ureq::Agent::config_builder()
                            .timeout_global(Some(Duration::from_secs(35)))
                            .build()
                            .new_agent();
                        let mut response = agent.get(&url).call().ok()?;
                        let source = response
                            .headers()
                            .get("X-Artwork-Source")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("Artwork")
                            .to_string();
                        let mut bytes = Vec::new();
                        response
                            .body_mut()
                            .as_reader()
                            .take(4 * 1024 * 1024)
                            .read_to_end(&mut bytes)
                            .ok()?;
                        let decoded = image::load_from_memory(&bytes).ok()?.to_rgba8();
                        Some((
                            egui::ColorImage::from_rgba_unmultiplied(
                                [decoded.width() as usize, decoded.height() as usize],
                                decoded.as_raw(),
                            ),
                            source,
                        ))
                    })();
                    let _ = sender.send((key, result));
                    ctx.request_repaint();
                });
            }
        }
        while let Ok((key, result)) = self.cover_in.try_recv() {
            if key == self.cover_key {
                if let Some((image, source)) = result {
                    self.cover =
                        Some(ctx.load_texture("record-cover", image, egui::TextureOptions::LINEAR));
                    self.cover_source = source;
                }
            }
        }
    }
    fn scene(&mut self, ui: &mut Ui, levels: [f32; 2]) {
        let width = ui.available_width();
        let (scene, _) = ui.allocate_exact_size(vec2(width, width * 2. / 3.), Sense::hover());
        if !ui.is_rect_visible(scene) {
            self.last = Instant::now();
            return;
        }
        if self.art.is_none() {
            self.art = Some(Art::load(ui.ctx()));
        }
        let elapsed = self.last.elapsed().as_secs_f32();
        self.last = Instant::now();
        // Returning from another page or a minimized window does not fast-forward the cat.
        let dt = if elapsed < 0.25 { elapsed } else { 0. };
        if !self.reduced {
            self.clock += dt;
        }
        for (i, level) in levels.iter().enumerate() {
            if *level > 0.018 {
                self.holds[i] = self.clock + 0.075;
            }
        }
        let t = self.clock;
        if !self.cat || self.reduced {
            self.routine = usize::MAX;
            self.next_cat = t + 120.;
        }
        if self.cat && !self.reduced && t >= self.next_cat && self.routine == usize::MAX {
            self.routine = self.next_routine;
            self.next_routine = (self.next_routine + 3) % 7;
            self.cat_start = t;
        }
        let age = t - self.cat_start;
        let (pose, duration) = cat_pose(self.routine, age);
        if self.routine != usize::MAX && age > duration {
            self.routine = usize::MAX;
            self.next_cat = t + 90. + ((t * 17.).sin().abs() * 90.);
        }
        let pose = if self.routine == usize::MAX { 0 } else { pose };
        let a = self.art.as_ref().unwrap();
        let p = ui.painter().with_clip_rect(scene.intersect(ui.clip_rect()));
        p.image(
            a.base.id(),
            scene,
            Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
            Color32::WHITE,
        );
        let s = scene.width() / 1536.;
        let at = |x: f32, y: f32| scene.min + vec2(x * s, y * s);
        if !self.reduced {
            if self.lights {
                for (i, (x, y)) in [
                    (678., 279.),
                    (731., 298.),
                    (713., 360.),
                    (819., 330.),
                    (924., 323.),
                    (899., 235.),
                    (1051., 258.),
                    (1114., 265.),
                    (756., 394.),
                    (821., 391.),
                ]
                .iter()
                .enumerate()
                {
                    let alpha =
                        (18. + 42. * (t / (5. + i as f32 * 0.43) + i as f32).sin().abs()) as u8;
                    p.rect_filled(
                        Rect::from_center_size(at(*x, *y), vec2(8. * s, 12. * s)),
                        0.,
                        Color32::from_rgba_unmultiplied(255, 198, 113, alpha),
                    );
                }
                for x in [712., 755., 817., 866.] {
                    for j in 0..5 {
                        let y = 432. + j as f32 * 12.;
                        if glass(x, y) {
                            p.line_segment(
                                [at(x - 5. + (t + j as f32).sin() * 2., y), at(x + 5., y)],
                                Stroke::new(
                                    s.max(0.5),
                                    Color32::from_rgba_unmultiplied(
                                        231,
                                        170,
                                        105,
                                        (15. + 25. * (t * 0.6 + j as f32).sin().abs()) as u8,
                                    ),
                                ),
                            );
                        }
                    }
                }
            }
            if self.rain {
                for i in 0..75 {
                    let i = i as f32;
                    let x = 465. + (i * 73.13) % 665.;
                    let y = 125. + (i * 53.7 + t * (45. + i % 5. * 12.)) % 390.;
                    let len = if i > 65. { 8. } else { 12. + i % 12. };
                    if glass(x, y) && glass(x - 2., y + len) {
                        p.line_segment(
                            [at(x, y), at(x - 2., y + len)],
                            Stroke::new(
                                (1.2 * s).max(0.45),
                                Color32::from_rgba_unmultiplied(165, 187, 226, 58),
                            ),
                        );
                    }
                }
            }
        }
        if pose > 0 {
            patch(&p, scene, &a.cats[pose - 1], CAT, vec2(0., 0.));
        }
        if !self.reduced {
            patch(
                &p,
                scene,
                &a.body,
                [58., 492., 65., 40.],
                vec2(0., -0.7 * (1. - (t * std::f32::consts::TAU / 4.8).cos())),
            );
            if pose == 0 {
                for phase in [0., 2.] {
                    let k = (t + phase) % 4. / 4.;
                    p.text(
                        at(140. + k * 6., 463. - k * 30.),
                        Align2::CENTER_CENTER,
                        "z",
                        FontId::monospace((17. * s).max(7.)),
                        Color32::from_rgba_unmultiplied(
                            194,
                            180,
                            188,
                            (130. * (std::f32::consts::PI * k).sin()) as u8,
                        ),
                    );
                }
            }
        }
        for i in 0..2 {
            let speaking = if self.reduced {
                levels[i] > 0.018
            } else {
                t < self.holds[i]
            };
            if speaking {
                patch(&p, scene, &a.mouths[i], MOUTHS[i], vec2(0., 0.));
            }
            if !self.reduced
                && (t + if i == 0 { 0. } else { 1.7 }) % if i == 0 { 5.1 } else { 6.7 } < 0.14
            {
                patch(&p, scene, &a.eyes[i], EYES[i], vec2(0., 0.));
            }
            let r = Rect::from_min_size(
                at(if i == 0 { 280. } else { 1040. }, 944.),
                vec2(180. * s, 43. * s),
            );
            p.rect_filled(r, 2., Color32::from_black_alpha(190));
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                if i == 0 { "MAV" } else { "RUE" },
                FontId::proportional((18. * s).max(9.)),
                if speaking { AMBER } else { theme::TEXT_DIM },
            );
        }
        if ui
            .interact(
                Rect::from_min_size(at(52., 437.), vec2(168. * s, 118. * s)),
                ui.id().with("cat"),
                Sense::click(),
            )
            .on_hover_text("Say hello to the studio cat")
            .clicked()
            && !self.reduced
            && self.cat
            && self.routine == usize::MAX
        {
            self.routine = 5;
            self.cat_start = t;
        }
        if !self.reduced {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
    }
}

fn patch(
    p: &egui::Painter,
    scene: Rect,
    texture: &TextureHandle,
    crop: [f32; 4],
    offset: egui::Vec2,
) {
    let s = scene.width() / 1536.;
    let r = Rect::from_min_size(
        scene.min + vec2(crop[0] + offset.x, crop[1] + offset.y) * s,
        vec2(crop[2], crop[3]) * s,
    );
    p.image(
        texture.id(),
        r,
        Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
        Color32::WHITE,
    );
}
impl Art {
    fn load(ctx: &egui::Context) -> Self {
        let idle =
            image::load_from_memory(include_bytes!("../../web/static/studio/studio-idle.png"))
                .expect("embedded studio");
        let speaking = image::load_from_memory(include_bytes!(
            "../../web/static/studio/studio-speaking.png"
        ))
        .expect("embedded mouths");
        let blinking =
            image::load_from_memory(include_bytes!("../../web/static/studio/studio-blink.png"))
                .expect("embedded eyes");
        fn texture(ctx: &egui::Context, name: &str, image: image::DynamicImage) -> TextureHandle {
            let data = image.to_rgba8();
            ctx.load_texture(
                name,
                egui::ColorImage::from_rgba_unmultiplied(
                    [data.width() as usize, data.height() as usize],
                    data.as_raw(),
                ),
                egui::TextureOptions::NEAREST,
            )
        }
        fn cropped(image: &image::DynamicImage, c: [f32; 4]) -> image::DynamicImage {
            image.crop_imm(c[0] as u32, c[1] as u32, c[2] as u32, c[3] as u32)
        }
        Self {
            mouths: MOUTHS
                .iter()
                .enumerate()
                .map(|(i, c)| texture(ctx, &format!("mouth-{i}"), cropped(&speaking, *c)))
                .collect(),
            eyes: EYES
                .iter()
                .enumerate()
                .map(|(i, c)| texture(ctx, &format!("eyes-{i}"), cropped(&blinking, *c)))
                .collect(),
            cats: POSES
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    texture(
                        ctx,
                        &format!("cat-{i}"),
                        cropped(&image::load_from_memory(b).expect("embedded cat"), CAT),
                    )
                })
                .collect(),
            body: texture(ctx, "cat-body", cropped(&idle, [58., 492., 65., 40.])),
            base: texture(ctx, "studio", idle),
        }
    }
}

fn cat_pose(routine: usize, t: f32) -> (usize, f32) {
    let frames: &[(f32, usize)] = match routine {
        0 => &[(0., 1), (2.6, 0), (3.1, 1), (4.5, 0)],
        1 => &[(0., 1), (0.7, 2), (2.2, 1), (3.2, 0)],
        2 => &[
            (0., 1),
            (0.8, 3),
            (1.6, 1),
            (2., 3),
            (2.8, 1),
            (3.2, 3),
            (4.1, 1),
            (5., 0),
        ],
        3 => &[(0., 1), (1.4, 0), (2.1, 1), (3., 0), (3.5, 1), (4.1, 0)],
        4 => &[(0., 0)],
        5 => &[(0., 1), (1.8, 0), (2.15, 1), (3.8, 0)],
        6 => &[(0., 1), (1., 2), (2.5, 1), (3.3, 3), (4.4, 1), (5.6, 0)],
        _ => &[(0., 0)],
    };
    (
        frames
            .iter()
            .rev()
            .find(|(at, _)| t >= *at)
            .map(|(_, p)| *p)
            .unwrap_or(0),
        [5., 4., 5.5, 5., 9., 4.5, 6.]
            .get(routine)
            .copied()
            .unwrap_or(0.),
    )
}
fn glass(x: f32, y: f32) -> bool {
    if !(465. ..1130.).contains(&x)
        || !(125. ..507.).contains(&y)
        || (551. ..575.).contains(&x)
        || (990. ..1009.).contains(&x)
    {
        return false;
    }
    let boundary = [
        (465., 188.),
        (474., 188.),
        (535., 207.),
        (590., 231.),
        (640., 275.),
        (671., 330.),
        (675., 390.),
        (666., 448.),
        (709., 460.),
        (762., 481.),
        (790., 507.),
        (826., 507.),
        (843., 480.),
        (864., 442.),
        (888., 408.),
        (912., 364.),
        (940., 326.),
        (978., 294.),
        (1030., 277.),
        (1070., 262.),
        (1130., 260.),
    ];
    // Point-in-polygon keeps drops behind the irregular silhouettes of the hosts.
    let mut polygon = [(0.0, 0.0); 23];
    polygon[0] = (465., 125.);
    polygon[1] = (1130., 125.);
    for (i, point) in boundary.iter().rev().enumerate() {
        polygon[i + 2] = *point;
    }
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[j]);
        if (a.1 > y) != (b.1 > y) && x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
        j = i;
    }
    inside
}

pub fn draw(app: &mut crate::Defalt, ui: &mut Ui, rect: Rect) {
    let status = match &app.station.health {
        crate::station::Health::Live(s) => Some((**s).clone()),
        _ => None,
    };
    let current = app.airtime.current().cloned();
    let key = current
        .as_ref()
        .map(|s| s.key.as_str())
        .or(status.as_ref().map(|s| s.track_key.as_str()))
        .unwrap_or("");
    app.studio.artwork(ui.ctx(), key, &app.station.url());
    let mut area = super::child(
        ui,
        rect.shrink(10.),
        egui::Layout::top_down(egui::Align::Min),
        "live-studio",
    );
    egui::ScrollArea::vertical()
        .id_salt("studio-scroll")
        .show(&mut area, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(super::rich("Inside the booth", 18., theme::TEXT));
                let mut changed = false;
                ui.menu_button("Studio settings", |ui| {
                    changed |= ui.checkbox(&mut app.studio.rain, "Rain").changed();
                    changed |= ui.checkbox(&mut app.studio.lights, "City lights").changed();
                    changed |= ui.checkbox(&mut app.studio.cat, "Cat antics").changed();
                    changed |= ui
                        .checkbox(&mut app.studio.reduced, "Reduced motion")
                        .changed();
                    ui.small("The cat keeps breathing between antics.");
                });
                if changed {
                    app.studio.save();
                }
            });
            app.studio.scene(ui, app.host_levels);
            ui.add_space(8.);
            let title = current
                .as_ref()
                .map(|s| s.title.as_str())
                .or(status.as_ref().and_then(|s| s.title.as_deref()))
                .unwrap_or("Your station. Your soundtrack.");
            let artist = current
                .as_ref()
                .map(|s| s.artist.as_str())
                .or(status.as_ref().and_then(|s| s.artist.as_deref()))
                .unwrap_or("Start radio to bring the booth on air.");
            let (position, duration) = current
                .as_ref()
                .map(|s| ((app.airtime.station_now - s.start_at).max(0.), s.duration))
                .unwrap_or_else(|| {
                    status
                        .as_ref()
                        .map(|s| (s.position, s.duration))
                        .unwrap_or((0., 0.))
                });
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 96.), Sense::hover());
            let center = r.min + vec2(46., 46.);
            let p = ui.painter();
            p.circle_filled(center, 43., Color32::from_rgb(19, 18, 24));
            for radius in [24., 28., 32., 36., 40.] {
                p.circle_stroke(
                    center,
                    radius,
                    Stroke::new(1., Color32::from_rgb(49, 44, 55)),
                );
            }
            if let Some(cover) = &app.studio.cover {
                let size = cover.size_vec2();
                let uv_scale = vec2((size.y / size.x).min(1.0), (size.x / size.y).min(1.0)) * 0.5;
                let angle = if app.airtime.on && current.is_some() && !app.studio.reduced {
                    app.studio.clock * std::f32::consts::TAU / 5.
                } else {
                    0.
                };
                let mut mesh = egui::Mesh::with_texture(cover.id());
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: center,
                    uv: pos2(0.5, 0.5),
                    color: Color32::WHITE,
                });
                for i in 0..=64 {
                    let a = i as f32 * std::f32::consts::TAU / 64.;
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: center + vec2(a.cos(), a.sin()) * 19.,
                        uv: pos2(
                            0.5 + (a - angle).cos() * uv_scale.x,
                            0.5 + (a - angle).sin() * uv_scale.y,
                        ),
                        color: Color32::WHITE,
                    });
                    if i > 0 {
                        mesh.indices.extend_from_slice(&[0, i, i + 1]);
                    }
                }
                p.add(egui::Shape::mesh(mesh));
            } else {
                p.circle_filled(center, 19., AMBER);
                p.text(
                    center,
                    Align2::CENTER_CENTER,
                    "DEFALT",
                    FontId::proportional(7.),
                    theme::GROUND,
                );
            }
            p.circle_filled(center, 2., theme::TEXT_DIM);
            let text = Rect::from_min_max(r.min + vec2(104., 3.), r.max);
            super::clipped_label(
                ui,
                Rect::from_min_size(text.min, vec2(text.width(), 25.)),
                title,
                19.,
                theme::TEXT,
            );
            super::clipped_label(
                ui,
                Rect::from_min_size(text.min + vec2(0., 27.), vec2(text.width(), 20.)),
                artist,
                12.,
                theme::TEXT_DIM,
            );
            let info = format!(
                "{} / {}   {}",
                super::mmss(position),
                super::mmss(duration),
                app.studio.cover_source
            );
            super::label(
                ui,
                text.min + vec2(0., 53.),
                Align2::LEFT_TOP,
                &info,
                10.,
                theme::TEXT_DIM,
            );
            if duration > 0. {
                let y = text.min.y + 76.;
                ui.painter().line_segment(
                    [pos2(text.left(), y), pos2(text.right(), y)],
                    Stroke::new(2., theme::EDGE),
                );
                ui.painter().line_segment(
                    [
                        pos2(text.left(), y),
                        pos2(
                            text.left() + text.width() * (position / duration).clamp(0., 1.) as f32,
                            y,
                        ),
                    ],
                    Stroke::new(2., AMBER),
                );
            }
            if let Some(status) = &status {
                ui.horizontal(|ui| {
                    ui.label(super::rich("Transcript", 16., theme::TEXT));
                    ui.checkbox(&mut app.transcript_follow, "Follow live");
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(
                            status
                                .transcript
                                .iter()
                                .map(|s| format!("{}: {}", s.host, s.text))
                                .collect::<Vec<_>>()
                                .join("\n\n"),
                        );
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("studio-transcript")
                    .max_height(210.)
                    .stick_to_bottom(app.transcript_follow)
                    .show(ui, |ui| {
                        if status.transcript.is_empty() {
                            ui.label("Host lines appear here as they air.");
                        }
                        for line in &status.transcript {
                            ui.label(super::rich(
                                &format!("{} · {}", line.host, super::mmss(line.start_at)),
                                11.,
                                AMBER,
                            ));
                            ui.label(&line.text);
                            if let Some(source) = &line.source {
                                if let Some(url) = &line.source_url {
                                    ui.hyperlink_to(source, url);
                                } else {
                                    ui.small(source);
                                }
                            }
                            ui.add_space(8.);
                        }
                    });
            } else {
                ui.label(
                    if matches!(app.station.health, crate::station::Health::Starting) {
                        "Bringing the station on air…"
                    } else {
                        "Use Go on air to start your station."
                    },
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rain_stays_off_hosts_and_window_frames() {
        assert!(glass(800., 160.));
        assert!(!glass(520., 350.));
        assert!(!glass(1050., 400.));
        assert!(!glass(560., 150.));
    }
    #[test]
    fn every_cat_routine_returns_to_sleep() {
        for i in 0..7 {
            let (_, d) = cat_pose(i, 0.);
            assert_eq!(cat_pose(i, d).0, 0);
        }
    }
}
