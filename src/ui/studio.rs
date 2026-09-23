//! The live booth. Drawing never advances or schedules audio.
use super::theme;
use egui::{pos2, vec2, Align2, Color32, ColorImage, FontId, Rect, Sense, Stroke, TextureHandle, Ui};
use std::{
    f64::consts::TAU,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

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
/// The neon ON AIR sign on the booth wall, with its glow, in scene units.
const SIGN: [f32; 4] = [1236., 72., 262., 132.];
/// The part of each cat pose's canvas the cat is actually in, in that
/// image's own pixels.
const CAT_CROPS: [[f32; 4]; 4] = [
    [1., 3., 264., 137.],
    [58., 33., 1635., 848.],
    [19., 10., 1678., 888.],
    [40., 14., 1641., 878.],
];

/// A layer cut down to the part of the scene it covers.
struct Patch {
    texture: TextureHandle,
    /// Where the texture sits, in scene units (the `W` x `H` canvas).
    area: [f32; 4],
}

struct Art {
    base: TextureHandle,
    hosts: [TextureHandle; 2],
    phones: [TextureHandle; 2],
    mouths: [Patch; 2],
    eyes: [Patch; 2],
    cats: [TextureHandle; 4],
    mugs: [TextureHandle; 2],
    microphones: Patch,
    /// The sign with its tubes cold, laid over the wall while off air.
    sign_off: Patch,
}

/// The same, decoded but not yet uploaded: what the worker hands back.
struct Decoded<T> {
    base: T,
    hosts: [T; 2],
    phones: [T; 2],
    mouths: [(T, [f32; 4]); 2],
    eyes: [(T, [f32; 4]); 2],
    cats: [T; 4],
    mugs: [T; 2],
    microphones: (T, [f32; 4]),
    sign_off: (T, [f32; 4]),
}

type Cover = (String, Option<(ColorImage, String)>);
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
    /// The artwork decoding off the UI thread, until it arrives.
    art_in: Option<mpsc::Receiver<Decoded<ColorImage>>>,
    preview: bool,
    /// Seconds of animation. f64, because an f32 that only ever grows loses
    /// the fraction of a second a blink needs after a few hours on air.
    clock: f64,
    last: Instant,
    holds: [f64; 2],
    next_cat: f64,
    cat_start: f64,
    routine: usize,
    next_routine: usize,
    cover_key: String,
    cover: Option<TextureHandle>,
    cover_source: String,
    cover_in: mpsc::Receiver<Cover>,
    cover_out: mpsc::Sender<Cover>,
    /// One HTTP agent for every artwork fetch, so its connections are reused.
    agent: Option<ureq::Agent>,
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
            art_in: None,
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
            agent: None,
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

    /// Start decoding the artwork, once. Ten megabytes of PNG is most of a
    /// second of work; done here it would be a frozen window the first time
    /// the radio view opened.
    pub fn preload(&mut self, ctx: &egui::Context) {
        if self.art.is_some() || self.art_in.is_some() {
            return;
        }
        let (send, receive) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = send.send(decode_art());
            ctx.request_repaint();
        });
        self.art_in = Some(receive);
    }

    /// Upload the artwork if the worker has finished with it.
    fn collect_art(&mut self, ctx: &egui::Context) {
        let Some(receive) = &self.art_in else { return };
        // A posed screenshot waits for the real booth rather than capturing
        // the placeholder; everything else carries on drawing meanwhile.
        let decoded = if self.preview { receive.recv().ok() } else { receive.try_recv().ok() };
        if let Some(decoded) = decoded {
            self.art = Some(Art::upload(ctx, decoded));
            self.art_in = None;
        }
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
                let agent = self.agent.get_or_insert_with(|| {
                    ureq::Agent::config_builder()
                        .timeout_global(Some(Duration::from_secs(35)))
                        .build()
                        .new_agent()
                }).clone();
                std::thread::spawn(move || {
                    let _ = sender.send((key, fetch_cover(&agent, &url)));
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

    /// Advance the animation clock and the cat's routine, and return the
    /// pose to draw it in.
    fn step(&mut self, levels: [f32; 2]) -> usize {
        let elapsed = self.last.elapsed().as_secs_f64();
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
        let age = (t - self.cat_start) as f32;
        let (pose, duration) = cat_pose(self.routine, age);
        if self.routine != usize::MAX && age > duration {
            self.routine = usize::MAX;
            self.next_cat = t + 90. + (sine(t, 17. / TAU, 0.).abs() * 90.) as f64;
        }
        if self.routine == usize::MAX { 0 } else { pose }
    }

    fn scene(&mut self, ui: &mut Ui, scene: Rect, levels: [f32; 2], on_air: bool) {
        if !ui.is_rect_visible(scene) {
            self.last = Instant::now();
            return;
        }
        self.collect_art(ui.ctx());
        let pose = self.step(levels);
        let p = ui.painter().with_clip_rect(scene.intersect(ui.clip_rect()));
        let Some(a) = self.art.as_ref() else {
            // Still decoding: an empty booth rather than a stalled window.
            p.rect_filled(scene, 6., theme::PANEL);
            p.text(scene.center(), Align2::CENTER_CENTER, "Setting up the booth…",
                   FontId::proportional(theme::SIZE_M), theme::TEXT_MUTE);
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            return;
        };
        let t = self.clock;
        // The room moves with the music, a little: the kick lifts the city
        // lights and thickens the rain. Nothing at all in reduced motion.
        let energy = if self.reduced { 0. } else { self.spectrum.low_energy() };
        layer(&p, scene, &a.base, [0., 0., W, H], full_uv());
        // The sign tells the truth: lit only while the station is.
        if !on_air {
            layer(&p, scene, &a.sign_off.texture, a.sign_off.area, full_uv());
        }
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
                    let i64 = i as f64;
                    let glow = 0.5 + 0.5 * sine(t, 1. / (TAU * (5. + i64 * 0.4)), i64);
                    let alpha = (15. + 26. * glow + 34. * energy).min(255.) as u8;
                    p.rect_filled(
                        Rect::from_min_size(at(*x, *y), vec2(9., 14.) * s),
                        0.,
                        theme::WINDOW_LIGHT.gamma_multiply_u8(alpha),
                    );
                }
            }
            if self.rain {
                let drops = 64 + (energy * 32.) as usize;
                let alpha = (48. + 50. * energy).min(255.) as u8;
                for i in 0..drops {
                    let i = i as f64;
                    let x = (468. + (i * 79.73) % 660.) as f32;
                    let y = (i * 49.17 + t * (42. + i % 6. * 7.)).rem_euclid(650.) as f32 - 25.;
                    let end = pos2(x - 2., y + 11. + (i % 9.) as f32);
                    if glass(x, y) && glass(end.x, end.y) {
                        p.line_segment(
                            [at(x, y), at(end.x, end.y)],
                            Stroke::new((1.1 * s).max(0.45), theme::RAIN.gamma_multiply_u8(alpha)),
                        );
                    }
                }
            }
        }
        let breath = if self.reduced { 0. } else { 0.7 * (1. - cosine(t, 1. / 4.8, 0.)) };
        let cat_texture = &a.cats[pose];
        layer(
            &p,
            scene,
            cat_texture,
            [CAT[0], CAT[1] - breath, CAT[2], CAT[3] + breath],
            full_uv(),
        );
        if pose == 0 {
            for phase in [0., 2.] {
                let k = if self.reduced { 0.4 } else { ((t + phase).rem_euclid(4.) / 4.) as f32 };
                p.text(
                    at(155. + k * 9., 261. - k * 32.),
                    Align2::LEFT_BOTTOM,
                    "z",
                    FontId::monospace((18. * s).max(7.)),
                    theme::SNORE.gamma_multiply_u8((153. * (std::f32::consts::PI * k).sin()) as u8),
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
                1. + 0.0016 * (1. - cosine(t, 1. / (5.4 + i as f64 * 0.6), i as f64 / TAU))
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
                face_patch(&p, scene, &a.mouths[i], MOUTHS[i], posed(MOUTHS[i]));
            }
            let (offset, period) = if i == 0 { (0., 5.1) } else { (1.7, 6.7) };
            if !self.reduced && (t + offset).rem_euclid(period) < 0.14 {
                for eye in EYES[i] {
                    face_patch(&p, scene, &a.eyes[i], eye, posed(eye));
                }
            }
        }
        layer(&p, scene, &a.mugs[0], [416., 892., 164., 175.], full_uv());
        layer(&p, scene, &a.mugs[1], [1095., 921., 184., 175.], full_uv());
        layer(&p, scene, &a.microphones.texture, a.microphones.area, full_uv());
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

/// `sin` of a phase that turns `cycles_per_second` times a second, worked
/// out in f64 and wrapped before the trig so hours of clock stay exact.
fn sine(t: f64, cycles_per_second: f64, offset: f64) -> f32 {
    ((t * cycles_per_second * TAU + offset).rem_euclid(TAU)).sin() as f32
}

/// The same for `cos`; `offset` is in turns.
fn cosine(t: f64, cycles_per_second: f64, offset: f64) -> f32 {
    ((t * cycles_per_second + offset).rem_euclid(1.) * TAU).cos() as f32
}

fn fetch_cover(agent: &ureq::Agent, url: &str) -> Option<(ColorImage, String)> {
    use std::io::Read;
    let mut response = agent.get(url).call().ok()?;
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
        ColorImage::from_rgba_unmultiplied(
            [decoded.width() as usize, decoded.height() as usize],
            decoded.as_raw(),
        ),
        source,
    ))
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
fn face_patch(p: &egui::Painter, scene: Rect, patch: &Patch, source: [f32; 4], dest: [f32; 4]) {
    // An elliptical mesh samples only the mouth/eyelids; no rectangular skin seams.
    let mut mesh = egui::Mesh::with_texture(patch.texture.id());
    let s = scene.width() / W;
    let area = patch.area;
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
                (source[0] + v.x * source[2] - area[0]) / area[2],
                (source[1] + v.y * source[3] - area[1]) / area[3],
            ),
            color: Color32::WHITE,
        });
        if i > 1 {
            mesh.indices.extend_from_slice(&[0, i - 1, i]);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}

fn colour_image(image: &image::RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw())
}

/// Cut `area` (scene units, padded a little) out of a full-canvas image, and
/// say what area of the scene the cut actually covers.
fn cut(image: &image::RgbaImage, area: [f32; 4]) -> (ColorImage, [f32; 4]) {
    let (piece, covered) = cut_rgba(image, area);
    (colour_image(&piece), covered)
}

/// A neon sign with the power off: the red light taken out of every pixel
/// in proportion to how much of it there was, so the tubes go dark glass and
/// the glow on the wall around them goes with them. Worked on a copy cut
/// from the background at load; the artwork on disk is never touched.
fn unlit(image: &mut image::RgbaImage) {
    let (width, height) = (image.width() as f32, image.height() as f32);
    // The wall is a warm brown with some red in it already, so only red
    // beyond the wall's own counts as the sign's light; and the change fades
    // out towards the edges of the patch, so there is no seam where it
    // meets the untouched wall.
    const WALL: f32 = 45.0;
    const FEATHER: f32 = 18.0;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let [r, g, b, a] = pixel.0.map(f32::from);
        let edge = (x as f32).min(y as f32).min(width - 1.0 - x as f32).min(height - 1.0 - y as f32);
        let feather = (edge / FEATHER).clamp(0.0, 1.0);
        let glow = (r - g.max(b) - WALL).max(0.0);
        // How much of this pixel is the sign's own light.
        let lit = (glow / 90.0).clamp(0.0, 1.0) * feather;
        if lit <= 0.0 {
            continue;
        }
        let dim = 1.0 - 0.72 * lit;
        let r = (r - glow * 0.85 * feather) * dim;
        pixel.0 = [r, g * dim, b * dim, a].map(|v| v.round().clamp(0.0, 255.0) as u8);
    }
}

fn cut_rgba(image: &image::RgbaImage, area: [f32; 4]) -> (image::RgbaImage, [f32; 4]) {
    let k = image.width() as f32 / W;
    let pad = 2.;
    let left = ((area[0] - pad) * k).floor().max(0.) as u32;
    let top = ((area[1] - pad) * k).floor().max(0.) as u32;
    let right = (((area[0] + area[2] + pad) * k).ceil() as u32).min(image.width());
    let bottom = (((area[1] + area[3] + pad) * k).ceil() as u32).min(image.height());
    let piece = image::imageops::crop_imm(image, left, top, right - left, bottom - top).to_image();
    let covered = [left as f32 / k, top as f32 / k, (right - left) as f32 / k, (bottom - top) as f32 / k];
    (piece, covered)
}

/// The smallest rectangle holding everything that is not transparent.
fn opaque_area(image: &image::RgbaImage) -> [f32; 4] {
    let (mut left, mut top, mut right, mut bottom) = (image.width(), image.height(), 0, 0);
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] > 0 {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    if right <= left || bottom <= top {
        return [0., 0., W, H];
    }
    let k = image.width() as f32 / W;
    [left as f32 / k, top as f32 / k, (right - left) as f32 / k, (bottom - top) as f32 / k]
}

/// Decode, and cut every layer down to what the scene uses of it.
///
/// Several layers are drawn from full-size canvases of which only a sliver
/// is ever sampled -- two mouths out of a whole frame of `speaking.jpg`, a cat
/// a few hundred pixels wide out of a canvas nearly two thousand wide. Kept
/// whole they are most of the booth's video memory. The files themselves are
/// untouched; the browser player uses them as they are.
fn decode_art() -> Decoded<ColorImage> {
    fn rgba(bytes: &[u8]) -> image::RgbaImage {
        image::load_from_memory(bytes).expect("embedded studio layer").to_rgba8()
    }
    macro_rules! art {
        ($name:literal) => {
            rgba(include_bytes!(concat!("../../web/static/studio-v2/", $name)))
        };
    }
    let whole = |image: image::RgbaImage| colour_image(&image);
    let speaking = art!("speaking.jpg");
    let blink = art!("blink.png");
    let mouths = MOUTHS.map(|mouth| cut(&speaking, mouth));
    let eyes = EYES.map(|[a, b]| {
        let left = a[0].min(b[0]);
        let top = a[1].min(b[1]);
        let right = (a[0] + a[2]).max(b[0] + b[2]);
        let bottom = (a[1] + a[3]).max(b[1] + b[3]);
        cut(&blink, [left, top, right - left, bottom - top])
    });
    let microphones = art!("microphones.png");
    let microphones = cut(&microphones, opaque_area(&microphones));
    let background = art!("background.png");
    let (mut sign, sign_area) = cut_rgba(&background, SIGN);
    unlit(&mut sign);
    let cats = [
        art!("sleeping-cat.png"),
        art!("cat-awake.png"),
        art!("cat-yawn.png"),
        art!("cat-groom.png"),
    ];
    let mut index = 0;
    let cats = cats.map(|cat| {
        let [x, y, w, h] = CAT_CROPS[index];
        index += 1;
        let piece = image::imageops::crop_imm(&cat, x as u32, y as u32, w as u32, h as u32).to_image();
        // The cat is drawn a few hundred scene units wide; twice that is
        // enough for any display the booth fits on.
        let (most_w, most_h) = ((CAT[2] * 2.) as u32, (CAT[3] * 2.) as u32);
        let piece = if piece.width() > most_w || piece.height() > most_h {
            let scale = (most_w as f32 / piece.width() as f32).min(most_h as f32 / piece.height() as f32);
            image::imageops::resize(&piece, (piece.width() as f32 * scale) as u32,
                                    (piece.height() as f32 * scale) as u32,
                                    image::imageops::FilterType::Triangle)
        } else {
            piece
        };
        colour_image(&piece)
    });
    Decoded {
        sign_off: (colour_image(&sign), sign_area),
        base: whole(background),
        hosts: [whole(art!("man.png")), whole(art!("woman.png"))],
        phones: [whole(art!("headphones-mav.png")), whole(art!("headphones-rue.png"))],
        mouths,
        eyes,
        cats,
        mugs: [whole(art!("black-mug.png")), whole(art!("white-mug.png"))],
        microphones,
    }
}

impl Art {
    fn upload(ctx: &egui::Context, decoded: Decoded<ColorImage>) -> Self {
        let texture = |name: &str, image: ColorImage| ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
        let patch = |name: &str, (image, area): (ColorImage, [f32; 4])| Patch { texture: texture(name, image), area };
        let [mouth_a, mouth_b] = decoded.mouths;
        let [eyes_a, eyes_b] = decoded.eyes;
        let [host_a, host_b] = decoded.hosts;
        let [phones_a, phones_b] = decoded.phones;
        let [cat_0, cat_1, cat_2, cat_3] = decoded.cats;
        let [mug_a, mug_b] = decoded.mugs;
        Self {
            base: texture("studio-background", decoded.base),
            hosts: [texture("studio-mav", host_a), texture("studio-rue", host_b)],
            phones: [texture("studio-phones-mav", phones_a), texture("studio-phones-rue", phones_b)],
            mouths: [patch("studio-mouth-mav", mouth_a), patch("studio-mouth-rue", mouth_b)],
            eyes: [patch("studio-eyes-mav", eyes_a), patch("studio-eyes-rue", eyes_b)],
            cats: [
                texture("studio-cat-sleeping", cat_0),
                texture("studio-cat-awake", cat_1),
                texture("studio-cat-yawn", cat_2),
                texture("studio-cat-groom", cat_3),
            ],
            mugs: [texture("studio-mug-black", mug_a), texture("studio-mug-white", mug_b)],
            microphones: patch("studio-microphones", decoded.microphones),
            sign_off: patch("studio-sign-off", decoded.sign_off),
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

/// What the booth shows as the record playing: a few small fields, copied
/// rather than the whole schedule entry or station status cloned each frame.
struct NowPlaying {
    key: String,
    title: String,
    artist: String,
    position: f64,
    duration: f64,
}

impl NowPlaying {
    fn of(app: &crate::Defalt) -> Self {
        let status = app.station.status();
        if let Some(item) = app.airtime.current() {
            return NowPlaying {
                key: item.key.clone(),
                title: item.title.clone(),
                artist: item.artist.clone(),
                position: (app.airtime.station_now - item.start_at).max(0.),
                duration: item.duration,
            };
        }
        NowPlaying {
            key: status.map(|s| s.track_key.clone()).unwrap_or_default(),
            title: status.and_then(|s| s.title.clone())
                .unwrap_or_else(|| "Your station. Your soundtrack.".into()),
            artist: status.and_then(|s| s.artist.clone())
                .unwrap_or_else(|| "Go on air to start your station.".into()),
            position: status.map_or(0., |s| s.position),
            duration: status.map_or(0., |s| s.duration),
        }
    }
}

/// The booth's parts, top to bottom, and how tall each is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Column {
    pub rect: Rect,
    pub header: Rect,
    pub scene: Rect,
    pub card: Rect,
    pub transcript: Option<Rect>,
    pub preview: Option<Rect>,
    pub spectrum: Option<Rect>,
}

const HEADER: f32 = 32.;
const CARD: f32 = 96.;

/// Lay the booth out as one column: the art, the record under it, the
/// transcript, the next mix and the spectrum, all on the art's left edge and
/// at its width, with the same gap between each. The art takes whatever
/// height the rest leaves, at its own 3:2, and the stack is centred in the
/// space so any spare falls evenly above and below it rather than opening a
/// hole in the middle.
pub fn column(area: Rect, transcript: bool, preview: bool, spectrum: bool) -> Column {
    let gap = theme::SP_3;
    let transcript_h = if transcript { (area.height() * 0.2).clamp(112., 180.) } else { 0. };
    let preview_h = if preview { super::transition_preview::HEIGHT } else { 0. };
    let spectrum_h = if spectrum { (area.height() * 0.14).clamp(84., 132.) } else { 0. };
    let parts = [transcript_h, preview_h, spectrum_h];
    let fixed = HEADER + CARD + parts.iter().sum::<f32>()
        + gap * (2 + parts.iter().filter(|h| **h > 0.).count()) as f32;
    let scene_w = ((area.height() - fixed).max(120.) * 1.5).min(area.width());
    let scene_h = scene_w / 1.5;
    let width = scene_w;
    let total = fixed + scene_h;
    let mut y = area.top() + ((area.height() - total) / 2.).max(0.);
    let left = area.center().x - width / 2.;
    let mut take = |height: f32| {
        let rect = Rect::from_min_size(pos2(left, y), vec2(width, height));
        y += height + gap;
        rect
    };
    let header = take(HEADER);
    let row = take(scene_h);
    let scene = Rect::from_center_size(row.center(), vec2(scene_w, scene_h));
    let card = take(CARD);
    let transcript = transcript.then(|| take(transcript_h));
    let preview = preview.then(|| take(preview_h));
    let spectrum = spectrum.then(|| take(spectrum_h));
    Column { rect: Rect::from_min_max(header.min, pos2(left + width, y - gap)), header, scene, card, transcript, preview, spectrum }
}

pub fn draw(app: &mut crate::Defalt, ui: &mut Ui, rect: Rect, preview: bool) {
    let playing = NowPlaying::of(app);
    let spinning = app.airtime.on && app.airtime.current().is_some() && !app.studio.reduced;
    let url = app.station.url();
    app.studio.artwork(ui.ctx(), &playing.key, &url);
    // The transcript earns its place once there is a station to hear; off
    // air, the card's one line says how to start.
    let live = app.station.status().is_some() || matches!(app.station.health, crate::station::Health::Starting);
    let layout = column(rect.shrink2(vec2(theme::SP_4, theme::SP_3)), live, preview, app.studio.visualizer);

    let mut header = super::child(ui, layout.header, super::left_row(), "booth-header");
    settings_row(app, &mut header);
    let on_air = app.station.ready();
    app.studio.scene(ui, layout.scene, app.host_levels, on_air);
    let mut card = super::child(ui, layout.card, egui::Layout::top_down(egui::Align::Min), "booth-card");
    record_card(&app.studio, &mut card, &playing, spinning);
    if let Some(rect) = layout.transcript {
        let mut area = super::child(ui, rect, egui::Layout::top_down(egui::Align::Min), "booth-transcript");
        transcript(app, &mut area);
    }
    if let Some(rect) = layout.preview {
        super::transition_preview::draw(ui, rect, &app.airtime.schedule, app.airtime.station_now);
    }
    if let Some(rect) = layout.spectrum {
        super::visualizer::draw(app, ui, rect);
    }
}

fn settings_row(app: &mut crate::Defalt, ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Inside the booth").font(theme::display(theme::SIZE_XL)).color(theme::TEXT_BRIGHT));
        let mut changed = false;
        ui.menu_button("Studio settings", |ui| {
            changed |= ui.checkbox(&mut app.studio.rain, "Rain").changed();
            changed |= ui.checkbox(&mut app.studio.lights, "City lights").changed();
            changed |= ui.checkbox(&mut app.studio.cat, "Cat antics").changed();
            changed |= ui
                .checkbox(&mut app.studio.reduced, "Reduced motion")
                .changed();
            ui.small("The cat keeps breathing between antics. With the visualizer on, the lights and rain follow the bass.");
        });
        if changed {
            app.studio.save();
        }
    });
}

/// The record on air: its cover turning on a label, title, artist, time.
fn record_card(studio: &Studio, ui: &mut Ui, playing: &NowPlaying, spinning: bool) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 96.), Sense::hover());
    let center = r.min + vec2(46., 46.);
    let p = ui.painter();
    p.circle_filled(center, 43., theme::VINYL);
    for radius in [24., 28., 32., 36., 40.] {
        p.circle_stroke(center, radius, Stroke::new(1., theme::VINYL_GROOVE));
    }
    if let Some(cover) = &studio.cover {
        let size = cover.size_vec2();
        let uv_scale = vec2((size.y / size.x).min(1.0), (size.x / size.y).min(1.0)) * 0.5;
        let angle = if spinning { ((studio.clock / 5.).rem_euclid(1.) * TAU) as f32 } else { 0. };
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
        p.circle_filled(center, 19., theme::AMBER);
        p.text(
            center,
            Align2::CENTER_CENTER,
            "DEFALT",
            // Printed on a record label, not read as text: the one place
            // below the type scale, because the label is 38 pixels across.
            theme::display(8.),
            theme::GROUND,
        );
    }
    p.circle_filled(center, 2., theme::TEXT_DIM);
    let text = Rect::from_min_max(r.min + vec2(104., 3.), r.max);
    super::clipped_text(
        ui,
        Rect::from_min_size(text.min, vec2(text.width(), 28.)),
        &playing.title,
        theme::display(theme::SIZE_XL),
        theme::TEXT_BRIGHT,
    );
    super::clipped_label(
        ui,
        Rect::from_min_size(text.min + vec2(0., 27.), vec2(text.width(), 20.)),
        &playing.artist,
        theme::SIZE_S,
        theme::TEXT_DIM,
    );
    let info = format!(
        "{} / {}   {}",
        super::mmss(playing.position),
        super::mmss(playing.duration),
        studio.cover_source
    );
    super::label(ui, text.min + vec2(0., 53.), Align2::LEFT_TOP, &info, theme::SIZE_XS, theme::TEXT_DIM);
    if playing.duration > 0. {
        let y = text.min.y + 76.;
        ui.painter().line_segment(
            [pos2(text.left(), y), pos2(text.right(), y)],
            Stroke::new(2., theme::EDGE),
        );
        let played = (playing.position / playing.duration).clamp(0., 1.) as f32;
        ui.painter().line_segment(
            [pos2(text.left(), y), pos2(text.left() + text.width() * played, y)],
            Stroke::new(2., theme::AMBER),
        );
    }
}

fn transcript(app: &mut crate::Defalt, ui: &mut Ui) {
    // Off air the record card has already said how to start; saying it a
    // third time here is noise.
    let Some(status) = app.station.status() else {
        if matches!(app.station.health, crate::station::Health::Starting) {
            ui.label(super::rich("Bringing the station on air…", theme::SIZE_M, theme::TEXT_DIM));
        }
        return;
    };
    super::transcript_header(ui, &status.transcript, &mut app.transcript_follow, false);
    let style = super::TranscriptStyle { body: theme::SIZE_M, accent: theme::AMBER };
    super::transcript_view(ui, "studio-transcript", &status.transcript, app.transcript_follow, style,
                           "Host lines appear here as they air.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dimensions(bytes: &[u8]) -> (u32, u32) {
        image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format().unwrap().into_dimensions().unwrap()
    }

    #[test]
    fn the_face_canvases_are_the_shapes_the_rectangles_assume() {
        assert_eq!(dimensions(include_bytes!("../../web/static/studio-v2/speaking.jpg")), (1728, 1152));
        assert_eq!(dimensions(include_bytes!("../../web/static/studio-v2/blink.png")), (1536, 1024));
        assert_eq!(dimensions(include_bytes!("../../web/static/studio-v2/microphones.png")), (1536, 1024));
    }

    #[test]
    fn layers_are_cut_down_to_what_the_scene_samples() {
        let art = decode_art();
        let contains = |area: [f32; 4], inner: [f32; 4]| {
            area[0] <= inner[0] && area[1] <= inner[1]
                && area[0] + area[2] >= inner[0] + inner[2]
                && area[1] + area[3] >= inner[1] + inner[3]
        };
        for i in 0..2 {
            let (image, area) = &art.mouths[i];
            assert!(contains(*area, MOUTHS[i]), "mouth {i} cut short: {area:?}");
            assert!(image.size[0] < 120 && image.size[1] < 80, "mouth {i} kept {:?}", image.size);
            let (image, area) = &art.eyes[i];
            for eye in EYES[i] {
                assert!(contains(*area, eye), "eyes {i} cut short: {area:?}");
            }
            assert!(image.size[0] < 200, "eyes {i} kept {:?}", image.size);
        }
        let (image, area) = &art.microphones;
        assert!(image.size[1] < 1024 / 2, "microphones kept {:?}", image.size);
        assert!(area[1] > 0. && area[1] + area[3] <= H + 1.);
        for (i, cat) in art.cats.iter().enumerate() {
            assert!(cat.size[0] as f32 <= CAT[2] * 2. + 1. && cat.size[1] as f32 <= CAT[3] * 2. + 1.,
                    "cat {i} kept {:?}", cat.size);
            // The pose keeps its proportions, or it would squash on screen.
            let crop = CAT_CROPS[i];
            let want = crop[2] / crop[3];
            let got = cat.size[0] as f32 / cat.size[1] as f32;
            assert!((want - got).abs() < 0.03, "cat {i} aspect {got} for {want}");
        }
    }

    #[test]
    fn the_animation_clock_keeps_its_fractions_after_a_long_night() {
        // Ten hours in, a blink is still a blink: the phase maths wraps in
        // f64 before any trig, so the same moment in the cycle reads the same.
        let late = 36_000.0 * 4.8;
        assert!((cosine(late, 1. / 4.8, 0.) - 1.).abs() < 1e-5);
        assert!((cosine(late + 2.4, 1. / 4.8, 0.) + 1.).abs() < 1e-5);
        assert!((sine(late + 1.2, 1. / 4.8, 0.) - 1.).abs() < 1e-5);
    }

    #[test]
    fn the_sign_goes_dark_off_air_and_nothing_else_changes() {
        let background = image::load_from_memory(include_bytes!("../../web/static/studio-v2/background.png"))
            .unwrap().to_rgba8();
        let (lit, area) = cut_rgba(&background, SIGN);
        let mut dark = lit.clone();
        unlit(&mut dark);
        let red = |image: &image::RgbaImage| image.pixels().map(|p| p[0] as f64).sum::<f64>() / image.len() as f64;
        assert!(red(&dark) < red(&lit) * 0.7, "the tubes stayed lit: {} of {}", red(&dark), red(&lit));
        // A grey pixel -- the wall, not the light -- is left exactly alone.
        let mut wall = image::RgbaImage::from_pixel(1, 1, image::Rgba([90, 90, 96, 255]));
        unlit(&mut wall);
        assert_eq!(wall.get_pixel(0, 0).0, [90, 90, 96, 255]);
        // The patch covers the sign, and sits on the wall above the hosts.
        assert!(area[0] <= 1255. && area[0] + area[2] >= 1477. && area[1] + area[3] < HOSTS[1][1]);
    }

    #[test]
    fn the_booth_is_one_column_on_the_art_s_edge_with_no_hole_in_it() {
        for (size, live) in [((1168., 806.), false), ((1168., 806.), true), ((752., 546.), false), ((752., 546.), true)] {
            let area = Rect::from_min_size(pos2(260., 50.), vec2(size.0, size.1));
            let c = column(area, live, false, true);
            let spectrum = c.spectrum.unwrap();
            assert_eq!(c.card.left(), c.scene.left(), "the record card is off the art's edge at {size:?}");
            assert_eq!(spectrum.left(), c.scene.left());
            assert_eq!(spectrum.width(), c.scene.width());
            let above = c.transcript.map_or(c.card.bottom(), |t| t.bottom());
            assert_eq!(spectrum.top() - above, theme::SP_3, "a gap opened above the spectrum at {size:?}");
            assert!((c.scene.width() / c.scene.height() - 1.5).abs() < 1e-3);
            assert!(c.rect.top() >= area.top() && c.rect.bottom() <= area.bottom() + 0.5, "{c:?} spills out of {area:?}");
            let (over, under) = (c.rect.top() - area.top(), area.bottom() - c.rect.bottom());
            assert!((over - under).abs() < 1., "not centred: {over} over, {under} under");
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
