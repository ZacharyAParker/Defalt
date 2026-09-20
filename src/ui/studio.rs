//! The live booth. Drawing never advances or schedules audio.
use super::theme;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke, TextureHandle, Ui};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

const AMBER: Color32 = Color32::from_rgb(239, 189, 113);
const W: f32 = 1728.;
const H: f32 = 1152.;
const CAT: [f32; 4] = [49., 251., 264., 137.];
const HOSTS: [[f32; 4]; 2] = [[101., 183., 781., 861.], [910., 213., 681., 824.]];
const PHONES: [[f32; 4]; 2] = [[383., 185., 328., 296.], [1077., 216., 304., 305.]];
const ANCHORS: [f32; 2] = [1005., 1010.];
const MOUTHS: [[f32; 4]; 2] = [[585., 463., 89., 51.], [1252., 477., 84., 55.]];
const EYES: [[[f32; 4]; 2]; 2] = [
    [[550., 374., 64., 40.], [634., 372., 48., 41.]],
    [[1224., 391., 71., 47.], [1323., 405., 54., 48.]],
];
const CAT_CROPS: [[f32; 4]; 4] = [
    [1., 3., 264., 137.],
    [58., 33., 1635., 848.],
    [19., 10., 1678., 888.],
    [40., 14., 1641., 878.],
];
struct Art {
    base: TextureHandle,
    hosts: [TextureHandle; 2],
    phones: [TextureHandle; 2],
    mouths: TextureHandle,
    eyes: TextureHandle,
    cats: [TextureHandle; 4],
    mugs: [TextureHandle; 2],
    microphones: TextureHandle,
}

type Cover = (String, Option<(egui::ColorImage, String)>);
pub struct Studio {
    pub enabled: bool,
    pub visualizer: bool,
    pub spectrum: super::visualizer::State,
    rain: bool,
    lights: bool,
    cat: bool,
    pub(super) reduced: bool,
    preferences: PathBuf,
    art: Option<Art>,
    preview: bool,
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
            visualizer: settings["visualizer"].as_bool().unwrap_or(true),
            spectrum: super::visualizer::State::default(),
            rain: settings["rain"].as_bool().unwrap_or(true),
            lights: settings["lights"].as_bool().unwrap_or(true),
            cat: settings["cat"].as_bool().unwrap_or(true),
            reduced: settings["reduced"].as_bool().unwrap_or(false),
            preferences,
            art: None,
            preview: false,
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
    pub(crate) fn pose(&mut self, name: &str) {
        self.preview = true;
        self.reduced = name == "reduced";
        self.clock = if name == "blink" { 0.05 } else { 1. };
        self.holds = match name {
            "mav" => [60., 0.],
            "rue" => [0., 60.],
            "both" => [60., 60.],
            _ => [0., 0.],
        };
        if name == "yawn" {
            self.routine = 1;
            self.cat_start = 0.;
        }
    }
    pub fn save(&self) {
        if let Some(parent) = self.preferences.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let data = serde_json::json!({"enabled":self.enabled,"visualizer":self.visualizer,"rain":self.rain,"lights":self.lights,"cat":self.cat,"reduced":self.reduced});
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
    fn scene(&mut self, ui: &mut Ui, levels: [f32; 2], max_height: f32) {
        let width = ui.available_width().min(max_height.max(0.) * 1.5);
        let size = vec2(width, width * 2. / 3.);
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), size.y), Sense::hover());
        let scene = Rect::from_center_size(row.center(), size);
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
        if !self.reduced && !self.preview {
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
        layer(&p, scene, &a.base, [0., 0., W, H], full_uv());
        let s = scene.width() / W;
        let at = |x: f32, y: f32| scene.min + vec2(x * s, y * s);
        // Weather is painted before the host layers, so silhouettes occlude it.
        if !self.reduced {
            if self.lights {
                for (i, (x, y)) in [
                    (771., 389.),
                    (818., 474.),
                    (905., 383.),
                    (987., 430.),
                    (1000., 514.),
                    (748., 436.),
                ]
                .iter()
                .enumerate()
                {
                    let alpha = (15.
                        + 26. * (0.5 + 0.5 * (t / (5. + i as f32 * 0.4) + i as f32).sin()))
                        as u8;
                    p.rect_filled(
                        Rect::from_min_size(at(*x, *y), vec2(9., 14.) * s),
                        0.,
                        Color32::from_rgba_unmultiplied(255, 194, 109, alpha),
                    );
                }
            }
            if self.rain {
                for i in 0..64 {
                    let i = i as f32;
                    let x = 468. + (i * 79.73) % 660.;
                    let y = (i * 49.17 + t * (42. + i % 6. * 7.)) % 650. - 25.;
                    let end = pos2(x - 2., y + 11. + i % 9.);
                    if glass(x, y) && glass(end.x, end.y) {
                        p.line_segment(
                            [at(x, y), at(end.x, end.y)],
                            Stroke::new(
                                (1.1 * s).max(0.45),
                                Color32::from_rgba_unmultiplied(173, 192, 220, 48),
                            ),
                        );
                    }
                }
            }
        }
        let breath = if self.reduced {
            0.
        } else {
            0.7 * (1. - (t * std::f32::consts::TAU / 4.8).cos())
        };
        let cat_texture = &a.cats[pose];
        let c = CAT_CROPS[pose];
        let sz = cat_texture.size_vec2();
        let uv = Rect::from_min_max(
            pos2(c[0] / sz.x, c[1] / sz.y),
            pos2((c[0] + c[2]) / sz.x, (c[1] + c[3]) / sz.y),
        );
        layer(
            &p,
            scene,
            cat_texture,
            [CAT[0], CAT[1] - breath, CAT[2], CAT[3] + breath],
            uv,
        );
        if pose == 0 {
            for phase in [0., 2.] {
                let k = if self.reduced {
                    0.4
                } else {
                    (t + phase) % 4. / 4.
                };
                p.text(
                    at(155. + k * 9., 261. - k * 32.),
                    Align2::LEFT_BOTTOM,
                    "z",
                    FontId::monospace((18. * s).max(7.)),
                    Color32::from_rgba_unmultiplied(
                        204,
                        191,
                        200,
                        (153. * (std::f32::consts::PI * k).sin()) as u8,
                    ),
                );
                if self.reduced {
                    break;
                }
            }
        }
        for i in 0..2 {
            let stretch = if self.reduced {
                1.
            } else {
                1. + 0.0016
                    * (1. - (t * std::f32::consts::TAU / (5.4 + i as f32 * 0.6) + i as f32).cos())
            };
            let posed = |r| breathing_rect(r, ANCHORS[i], stretch);
            layer(&p, scene, &a.hosts[i], posed(HOSTS[i]), full_uv());
            layer(&p, scene, &a.phones[i], posed(PHONES[i]), full_uv());
            let speaking = if self.reduced {
                levels[i] > 0.018
            } else {
                t < self.holds[i]
            };
            if speaking {
                face_patch(&p, scene, &a.mouths, MOUTHS[i], posed(MOUTHS[i]));
            }
            if !self.reduced
                && (t + if i == 0 { 0. } else { 1.7 }) % if i == 0 { 5.1 } else { 6.7 } < 0.14
            {
                for eye in EYES[i] {
                    face_patch(&p, scene, &a.eyes, eye, posed(eye));
                }
            }
        }
        layer(&p, scene, &a.mugs[0], [416., 892., 164., 175.], full_uv());
        layer(&p, scene, &a.mugs[1], [1095., 921., 184., 175.], full_uv());
        layer(&p, scene, &a.microphones, [0., 0., W, H], full_uv());
        if ui
            .interact(
                Rect::from_min_size(at(CAT[0], CAT[1]), vec2(CAT[2], CAT[3]) * s),
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

fn full_uv() -> Rect {
    Rect::from_min_max(pos2(0., 0.), pos2(1., 1.))
}
fn breathing_rect(mut r: [f32; 4], anchor: f32, scale: f32) -> [f32; 4] {
    r[1] = anchor + (r[1] - anchor) * scale;
    r[3] *= scale;
    r
}
fn layer(p: &egui::Painter, scene: Rect, texture: &TextureHandle, r: [f32; 4], uv: Rect) {
    let s = scene.width() / W;
    p.image(
        texture.id(),
        Rect::from_min_size(scene.min + vec2(r[0], r[1]) * s, vec2(r[2], r[3]) * s),
        uv,
        Color32::WHITE,
    );
}
fn face_patch(
    p: &egui::Painter,
    scene: Rect,
    texture: &TextureHandle,
    source: [f32; 4],
    dest: [f32; 4],
) {
    // An elliptical mesh samples only the mouth/eyelids; no rectangular skin seams.
    let mut mesh = egui::Mesh::with_texture(texture.id());
    let s = scene.width() / W;
    for i in 0..=49 {
        let v = if i == 0 {
            vec2(0.5, 0.5)
        } else {
            let a = (i - 1) as f32 * std::f32::consts::TAU / 48.;
            vec2(0.5 + 0.5 * a.cos(), 0.5 + 0.5 * a.sin())
        };
        mesh.vertices.push(egui::epaint::Vertex {
            pos: scene.min + vec2(dest[0] + v.x * dest[2], dest[1] + v.y * dest[3]) * s,
            uv: pos2(
                (source[0] + v.x * source[2]) / W,
                (source[1] + v.y * source[3]) / H,
            ),
            color: Color32::WHITE,
        });
        if i > 1 {
            mesh.indices.extend_from_slice(&[0, i - 1, i]);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}
impl Art {
    fn load(ctx: &egui::Context) -> Self {
        fn texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> TextureHandle {
            let data = image::load_from_memory(bytes)
                .expect("embedded studio layer")
                .to_rgba8();
            ctx.load_texture(
                name,
                egui::ColorImage::from_rgba_unmultiplied(
                    [data.width() as usize, data.height() as usize],
                    data.as_raw(),
                ),
                egui::TextureOptions::LINEAR,
            )
        }
        macro_rules! art {
            ($name:literal) => {
                texture(
                    ctx,
                    $name,
                    include_bytes!(concat!("../../web/static/studio-v2/", $name)),
                )
            };
        }
        Self {
            base: art!("background.png"),
            hosts: [art!("man.png"), art!("woman.png")],
            phones: [art!("headphones-mav.png"), art!("headphones-rue.png")],
            mouths: art!("speaking.jpg"),
            eyes: art!("blink.png"),
            cats: [
                art!("sleeping-cat.png"),
                art!("cat-awake.png"),
                art!("cat-yawn.png"),
                art!("cat-groom.png"),
            ],
            mugs: [art!("black-mug.png"), art!("white-mug.png")],
            microphones: art!("microphones.png"),
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
    (466. ..1131.).contains(&x) && (0. ..618.).contains(&y) && !(709. ..726.).contains(&x)
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
    {
        let ui = &mut area;
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
        // Keep the record and live transcript in view at every window size.
        // Only transcript history scrolls; the booth always fits in full.
        let transcript_height = (ui.available_height() * 0.24).clamp(112., 180.);
        let scene_height =
            ui.available_height() - 96. - transcript_height - 8. - ui.spacing().item_spacing.y * 3.;
        app.studio.scene(ui, app.host_levels, scene_height);
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
                .max_height(ui.available_height().max(0.))
                .auto_shrink([false, false])
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layered_art_decodes_with_matching_face_canvases() {
        let art = Art::load(&egui::Context::default());
        assert_eq!(art.mouths.size(), [1728, 1152]);
        assert_eq!(art.eyes.size(), [1536, 1024]);
        assert_eq!(art.microphones.size(), [1536, 1024]);
        for (i, cat) in art.cats.iter().enumerate() {
            let crop = CAT_CROPS[i];
            assert!(crop[0] + crop[2] <= cat.size()[0] as f32);
            assert!(crop[1] + crop[3] <= cat.size()[1] as f32);
        }
    }
    #[test]
    fn rain_stays_inside_window_panes() {
        assert!(glass(800., 160.));
        assert!(!glass(450., 350.));
        assert!(!glass(800., 650.));
        assert!(!glass(718., 150.));
    }
    #[test]
    fn every_cat_routine_returns_to_sleep() {
        for i in 0..7 {
            let (_, d) = cat_pose(i, 0.);
            assert_eq!(cat_pose(i, d).0, 0);
        }
    }
}
