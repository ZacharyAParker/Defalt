//! The live booth. Drawing never advances or schedules audio.
mod motion;

use super::theme;
use egui::{epaint::Vertex, pos2, vec2, Align2, Color32, ColorImage, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, TextureId, Ui};
use motion::{Cat, Eyes, Lights, Lips, Rain, Sign, Steam};
use std::{
    f64::consts::TAU,
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

const W: f32 = 1728.;
const H: f32 = 1152.;
/// Where the cat can be clicked, in scene units.
const CAT: [f32; 4] = [49., 251., 264., 137.];
const HOSTS: [[f32; 4]; 2] = [[101., 183., 781., 861.], [910., 213., 681., 824.]];
const PHONES: [[f32; 4]; 2] = [[383., 185., 328., 296.], [1077., 216., 304., 305.]];
/// Each host's body, top to bottom: the head rides rigidly above the neck,
/// the chest rises between the shoulders and the desk, the arms on the desk
/// stay put.
const NECK: [f32; 2] = [545., 575.];
const SHOULDERS: [f32; 2] = [610., 650.];
const DESK: [f32; 2] = [1005., 1010.];
/// The neon ON AIR sign on the booth wall, with its glow, in scene units.
#[cfg(test)]
const SIGN: [f32; 4] = [1236., 72., 262., 132.];
/// The frame atlas and where everything in it goes, built by
/// tools/build-studio-frames.py from the booth's own art.
const ATLAS: &[u8] = include_bytes!("../../web/static/studio-v2/atlas.png");
const FRAMES: &[u8] = include_bytes!("../../web/static/studio-v2/web/frames.json");

/// A layer cut down to the part of the scene it covers.
struct Patch {
    texture: TextureHandle,
    /// Where the texture sits, in scene units (the `W` x `H` canvas).
    area: [f32; 4],
}

/// One picture in the atlas: its texture coordinates, and where it goes in
/// the scene.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Frame {
    uv: Rect,
    at: [f32; 4],
}

/// Every frame the booth animates with, read from the atlas's manifest.
#[derive(Clone, Debug)]
struct Frames {
    /// Per host: the five open mouths, the half and closed lids, and the two
    /// glances. A missing one is simply never drawn.
    mouths: [[Option<Frame>; 5]; 2],
    lids: [[Option<Frame>; 2]; 2],
    looks: [[Option<Frame>; 2]; 2],
    cat: Vec<Frame>,
    lights: Vec<(Frame, Frame)>,
    window: Frame,
    flash: Frame,
    sign_off: Frame,
    sign_glow: Frame,
    z: Frame,
    soft: Frame,
    lamp: Pos2,
    mugs: [Pos2; 2],
    snore: Pos2,
}

impl Frames {
    fn parse(json: &[u8]) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_slice(json).ok()?;
        let size = [v["atlas"][0].as_f64()? as f32, v["atlas"][1].as_f64()? as f32];
        let four = |v: &serde_json::Value| -> Option<[f32; 4]> {
            Some([v[0].as_f64()? as f32, v[1].as_f64()? as f32, v[2].as_f64()? as f32, v[3].as_f64()? as f32])
        };
        let frame = |v: &serde_json::Value| -> Option<Frame> {
            let src = four(&v["src"])?;
            let uv = Rect::from_min_size(pos2(src[0] / size[0], src[1] / size[1]), vec2(src[2] / size[0], src[3] / size[1]));
            Some(Frame { uv, at: four(&v["at"])? })
        };
        let point = |v: &serde_json::Value| -> Option<Pos2> { Some(pos2(v[0].as_f64()? as f32, v[1].as_f64()? as f32)) };
        let list = |v: &serde_json::Value, n: usize| -> [Option<Frame>; 5] {
            std::array::from_fn(|i| if i < n { frame(&v[i]) } else { None })
        };
        let host = |name: &str| {
            let h = &v["faces"][name];
            let mouths = list(&h["mouths"], 5);
            let lids = list(&h["lids"], 2);
            let looks = list(&h["looks"], 2);
            (mouths, [lids[0], lids[1]], [looks[0], looks[1]])
        };
        let (mav, rue) = (host("mav"), host("rue"));
        let cat = v["cat"].as_array()?.iter().map(frame).collect::<Option<Vec<_>>>()?;
        if cat.len() != motion::CAT_FRAMES.len() {
            return None;
        }
        let lights = v["lights"].as_array()?.iter()
            .map(|l| Some((frame(&l["lit"])?, frame(&l["dim"])?)))
            .collect::<Option<Vec<_>>>()?;
        Some(Frames {
            mouths: [mav.0, rue.0],
            lids: [mav.1, rue.1],
            looks: [mav.2, rue.2],
            cat,
            lights,
            window: frame(&v["window"])?,
            flash: frame(&v["flash"])?,
            sign_off: frame(&v["sign_off"])?,
            sign_glow: frame(&v["sign_glow"])?,
            z: frame(&v["z"])?,
            soft: frame(&v["soft"])?,
            lamp: point(&v["lamp"])?,
            mugs: [point(&v["mugs"][0])?, point(&v["mugs"][1])?],
            snore: point(&v["snore"])?,
        })
    }
}

struct Art {
    base: TextureHandle,
    hosts: [TextureHandle; 2],
    phones: [TextureHandle; 2],
    mugs: [TextureHandle; 2],
    microphones: Patch,
    atlas: TextureHandle,
    frames: Frames,
}

/// The same, decoded but not yet uploaded: what the worker hands back.
struct Decoded<T> {
    base: T,
    hosts: [T; 2],
    phones: [T; 2],
    mugs: [T; 2],
    microphones: (T, [f32; 4]),
    atlas: T,
    frames: Frames,
}

/// Meshes kept from frame to frame. egui holds on to a frame's shapes only
/// until it has drawn them, so by the next frame each of these is ours
/// alone again and is refilled in place rather than allocated anew.
#[derive(Default)]
struct Meshes {
    pool: Vec<Arc<Mesh>>,
    used: usize,
}

impl Meshes {
    fn begin(&mut self) {
        self.used = 0;
    }
    fn next(&mut self, texture: TextureId) -> &mut Mesh {
        if self.used == self.pool.len() {
            self.pool.push(Arc::new(Mesh::default()));
        }
        let mesh = Arc::make_mut(&mut self.pool[self.used]);
        self.used += 1;
        mesh.clear();
        mesh.texture_id = texture;
        mesh
    }
    /// Hand the mesh just filled to the painter.
    fn paint(&self, p: &egui::Painter) {
        if let Some(mesh) = self.pool[..self.used].last() {
            if !mesh.is_empty() {
                p.add(Shape::Mesh(mesh.clone()));
            }
        }
    }
}

/// What the booth hears this frame.
#[derive(Clone, Copy, Debug, Default)]
struct Heard {
    levels: [f32; 2],
    tones: [f32; 2],
    /// The low end of the music, 0..1, and its beat's flash.
    energy: f32,
    beat: f32,
    on_air: bool,
}

/// A screenshot run: the booth played from a script instead of the station,
/// a thirtieth of a second per frame, so every run looks the same.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Script {
    voices: [bool; 2],
    music: bool,
    on_air_at: Option<f64>,
    strike_at: Option<f64>,
}

type Cover = (String, Option<(ColorImage, String)>);
pub struct Studio {
    pub enabled: bool,
    pub visualizer: bool,
    pub spectrum: super::visualizer::State,
    rain: bool,
    lights: bool,
    cat: bool,
    lightning: bool,
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
    lips: [Lips; 2],
    eyes: [Eyes; 2],
    looks: [motion::Look; 2],
    kitty: Cat,
    pose: motion::CatPose,
    /// How much the Zs show: they fade as the cat wakes, and back.
    snoring: f32,
    weather: Box<Rain>,
    city: Lights,
    steam: Steam,
    sign: Sign,
    both: bool,
    meshes: Meshes,
    script: Option<Script>,
    /// Seconds a screenshot run has asked the booth to move on by.
    pending: f64,
    /// Where the scene was last drawn, in points, for a screenshot run to crop to.
    pub(crate) drawn: Option<Rect>,
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
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(7, |d| d.subsec_nanos());
        let mut studio = Self {
            enabled: settings["enabled"].as_bool().unwrap_or(true),
            visualizer: settings["visualizer"].as_bool().unwrap_or(true),
            spectrum: super::visualizer::State::default(),
            rain: settings["rain"].as_bool().unwrap_or(true),
            lights: settings["lights"].as_bool().unwrap_or(true),
            cat: settings["cat"].as_bool().unwrap_or(true),
            lightning: settings["lightning"].as_bool().unwrap_or(true),
            reduced: settings["reduced"].as_bool().unwrap_or(false),
            preferences,
            art: None,
            art_in: None,
            preview: false,
            clock: 0.,
            last: Instant::now(),
            lips: [Lips::default(); 2],
            eyes: [Eyes::new(1, true), Eyes::new(2, false)],
            looks: [motion::Look { lid: 0, glance: 0 }; 2],
            kitty: Cat::new(3, 0.),
            pose: motion::CatPose { frame: 0, asleep: true },
            snoring: 1.,
            weather: Box::new(Rain::new(4)),
            city: Lights::new(5, 0),
            steam: Steam::new(6),
            sign: Sign::default(),
            both: false,
            meshes: Meshes::default(),
            script: None,
            pending: 0.,
            drawn: None,
            cover_key: String::new(),
            cover: None,
            cover_source: String::new(),
            cover_in,
            cover_out,
            agent: None,
        };
        studio.reseed(seed);
        studio
    }

    /// Shuffle what the booth does next, so no two sessions run alike.
    fn reseed(&mut self, seed: u32) {
        let seed = seed | 1;
        self.eyes = [Eyes::new(seed.wrapping_mul(3), true), Eyes::new(seed.wrapping_mul(5), false)];
        self.kitty = Cat::new(seed.wrapping_mul(7), self.clock);
        self.weather = Box::new(Rain::new(seed.wrapping_mul(11)));
        self.city = Lights::new(seed.wrapping_mul(13), self.city.count);
        self.steam = Steam::new(seed.wrapping_mul(17));
    }

    /// Set the booth up for a screenshot: frozen on one moment, or (with
    /// `advance`) played a frame at a time from a script.
    pub(crate) fn pose(&mut self, name: &str) {
        self.preview = true;
        self.reduced = name == "reduced";
        self.clock = 0.;
        self.reseed(12_345);
        let mut script = Script::default();
        match name {
            "mav" => script.voices = [true, false],
            "rue" => script.voices = [false, true],
            "both" | "speaking" => script.voices = [true, true],
            "sign" => script.on_air_at = Some(0.5),
            "rain" => {
                script.music = true;
                script.on_air_at = Some(0.);
                script.strike_at = Some(2.2);
            }
            "overview" => {
                script = Script { voices: [true, true], music: true, on_air_at: Some(0.), strike_at: Some(3.) };
            }
            "lights" | "blink" => script.on_air_at = Some(0.),
            _ => {}
        }
        self.script = Some(script);
        match name {
            // Frozen poses land on the moment they are named for.
            "mav" | "rue" | "both" => self.warm(1.2),
            "blink" => {
                self.warm(0.6);
                self.eyes[0].next = self.clock;
                self.eyes[1].next = self.clock;
                self.warm(0.08);
            }
            "yawn" => {
                self.kitty.play(3, self.clock);
                self.warm(2.3);
            }
            "cat" => self.kitty.play(3, self.clock),
            "groom" => self.kitty.play(4, self.clock),
            "stretch" => self.kitty.play(5, self.clock),
            "perk" => self.kitty.poke(self.clock),
            _ => self.warm(0.05),
        }
    }

    /// Run the posed booth forward without drawing.
    fn warm(&mut self, seconds: f64) {
        let steps = (seconds * 30.).round() as usize;
        for _ in 0..steps {
            let heard = self.scripted();
            self.step(1. / 30., heard);
        }
    }

    /// Move a screenshot run on by `seconds` before its next frame.
    pub(crate) fn advance(&mut self, seconds: f64) {
        self.pending += seconds;
    }

    pub fn save(&self) {
        if let Some(parent) = self.preferences.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let data = serde_json::json!({"enabled":self.enabled,"visualizer":self.visualizer,"rain":self.rain,"lights":self.lights,"cat":self.cat,"lightning":self.lightning,"reduced":self.reduced});
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
            let art = Art::upload(ctx, decoded);
            self.city.count = art.frames.lights.len().min(motion::MAX_WINDOWS);
            self.art = Some(art);
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

    /// What a screenshot run hears at this moment of its script.
    fn scripted(&self) -> Heard {
        let script = self.script.unwrap_or_default();
        let t = self.clock;
        let mut heard = Heard { on_air: script.on_air_at.is_some_and(|at| t >= at), ..Default::default() };
        for host in 0..2 {
            if script.voices[host] {
                let (level, tone) = synthetic_voice(t, host, script.voices == [true, true]);
                heard.levels[host] = level;
                heard.tones[host] = tone;
            }
        }
        if script.music {
            let beat = (-(t.rem_euclid(0.5)) / 0.12).exp() as f32;
            heard.beat = beat;
            heard.energy = 0.35 + 0.35 * beat;
        }
        heard
    }

    /// Move everything on by `dt` seconds.
    fn step(&mut self, dt: f64, heard: Heard) {
        self.clock += dt;
        let t = self.clock;
        let dt = dt as f32;
        let reduced = self.reduced;
        for host in 0..2 {
            self.lips[host].step(dt, heard.levels[host], heard.tones[host], reduced);
        }
        let talking = [self.lips[0].talking(), self.lips[1].talking()];
        for host in 0..2 {
            self.looks[host] = self.eyes[host].step(t, talking[host], talking[1 - host], reduced);
        }
        let both = self.lips[0].viseme != motion::REST && self.lips[1].viseme != motion::REST;
        if both && !self.both && self.cat && !reduced {
            self.kitty.crowd(t);
        }
        self.both = both;
        self.pose = self.kitty.step(t, self.cat, reduced);
        let target = if self.pose.asleep { 1. } else { 0. };
        self.snoring += (target - self.snoring).clamp(-dt / 0.5, dt / 0.5);
        if !reduced {
            self.weather.step(dt, t, heard.energy);
            self.weather.storm(t, self.lightning && self.rain);
            if let Some(at) = self.script.and_then(|s| s.strike_at) {
                if t >= at && t - (dt as f64) < at {
                    self.weather.strike(t);
                }
            }
            self.city.step(dt, t);
            self.steam.step(dt);
        }
        self.sign.step(t, heard.on_air, reduced);
    }

    fn scene(&mut self, ui: &mut Ui, scene: Rect, mut heard: Heard) {
        if !ui.is_rect_visible(scene) {
            self.last = Instant::now();
            return;
        }
        self.collect_art(ui.ctx());
        let elapsed = self.last.elapsed().as_secs_f64();
        self.last = Instant::now();
        // Returning from another page or a minimized window does not fast-forward the cat.
        let dt = if elapsed < 0.25 { elapsed } else { 0. };
        if self.preview {
            // A screenshot run steps exactly a thirtieth of a second at a
            // time, from its script, and only when asked to.
            let steps = (std::mem::take(&mut self.pending) * 30.).round() as usize;
            for _ in 0..steps {
                let scripted = self.scripted();
                self.step(1. / 30., scripted);
            }
            heard = self.scripted();
        } else if dt > 0. {
            self.step(dt, heard);
        }
        if self.reduced {
            heard.energy = 0.;
            heard.beat = 0.;
        }
        self.drawn = Some(scene);
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
        let s = scene.width() / W;
        let at = |x: f32, y: f32| scene.min + vec2(x * s, y * s);
        let rect = |r: [f32; 4]| Rect::from_min_size(at(r[0], r[1]), vec2(r[2], r[3]) * s);
        let f = &a.frames;
        let atlas = a.atlas.id();
        let still = self.reduced;
        let meshes = &mut self.meshes;
        meshes.begin();
        layer(&p, scene, &a.base, [0., 0., W, H], full_uv());

        // The city: lit windows breathe a little, and now and then one goes
        // out and comes back. The music lifts them, a touch.
        if self.lights && !still {
            let mesh = meshes.next(atlas);
            for (i, (lit, dim)) in f.lights.iter().enumerate().take(self.city.count) {
                let off = 1. - self.city.windows[i].on;
                if off > 0.01 {
                    mesh.add_rect_with_uv(rect(dim.at), dim.uv, Color32::from_white_alpha((off * 235.) as u8));
                }
                let glow = self.city.glow(i, t, heard.energy);
                mesh.add_rect_with_uv(rect(lit.at), lit.uv, additive(glow));
            }
            meshes.paint(&p);
        }
        let window = rect(motion::WINDOW);
        if self.rain && !still {
            let flash = self.weather.flash(t);
            if flash > 0. {
                let mesh = meshes.next(atlas);
                mesh.add_rect_with_uv(rect(f.flash.at), f.flash.uv, additive(0.9 * flash));
                meshes.paint(&p);
            }
            let wet = p.with_clip_rect(window.intersect(p.clip_rect()));
            let mesh = meshes.next(TextureId::default());
            rain_mesh(mesh, &self.weather, &at, s);
            meshes.paint(&wet);
            let mesh = meshes.next(atlas);
            for bead in self.weather.beads.iter().filter(|b| b.state > 0) {
                let r = bead.r * 1.7;
                let dot = Rect::from_center_size(at(bead.x, bead.y), vec2(r, r) * 2. * s);
                mesh.add_rect_with_uv(dot, f.soft.uv, Color32::from_rgba_unmultiplied(205, 218, 240, 120));
                if bead.state == 2 && bead.y - bead.top > 2. {
                    let trail = Rect::from_min_max(at(bead.x - 0.6, bead.top), at(bead.x + 0.6, bead.y));
                    mesh.add_rect_with_uv(trail, f.soft.uv, Color32::from_rgba_unmultiplied(190, 205, 235, 70));
                }
            }
            meshes.paint(&wet);
            let mesh = meshes.next(atlas);
            mesh.add_rect_with_uv(rect(f.window.at), f.window.uv, Color32::WHITE);
            meshes.paint(&p);
        }

        // The sign: lit only while the station is, warming up with a flicker,
        // its glow breathing while it burns.
        let burn = self.sign.level * self.sign.pulse(t, still);
        {
            let mesh = meshes.next(atlas);
            let dark = (1. - burn).clamp(0., 1.);
            if dark > 0.004 {
                mesh.add_rect_with_uv(rect(f.sign_off.at), f.sign_off.uv, Color32::from_white_alpha((dark * 255.) as u8));
            }
            if burn > 1. {
                mesh.add_rect_with_uv(rect(f.sign_glow.at), f.sign_glow.uv, additive((burn - 1.) * 3.));
            }
            // The lamp flutters, and dips now and then.
            if !still {
                let lamp = self.city.lamp(t) - 1.;
                let glow = Rect::from_center_size(at(f.lamp.x, f.lamp.y + 30.), vec2(330., 300.) * s);
                let colour = if lamp > 0. {
                    let k = lamp * 1.6;
                    Color32::from_rgba_premultiplied((255. * k) as u8, (185. * k) as u8, (105. * k) as u8, 0)
                } else {
                    Color32::from_black_alpha((-lamp * 300.).min(255.) as u8)
                };
                mesh.add_rect_with_uv(glow, f.soft.uv, colour);
            }
            meshes.paint(&p);
        }

        // The cat, and its Zs while it sleeps.
        {
            let mesh = meshes.next(atlas);
            let cat = f.cat[self.pose.frame.min(f.cat.len() - 1)];
            mesh.add_rect_with_uv(rect(cat.at), cat.uv, Color32::WHITE);
            if self.snoring > 0.01 {
                let zs: &[f32] = if still { &[0.45] } else { &[0., 1. / 3., 2. / 3.] };
                for phase in zs {
                    let k = if still { *phase } else { ((t / 3.6) as f32 + phase).fract() };
                    let (dx, dy, size, alpha) = motion::z_at(k);
                    let alpha = if still { 0.5 } else { alpha * self.snoring };
                    let z = Rect::from_center_size(at(f.snore.x + dx, f.snore.y + dy), vec2(18., 18.) * size * s);
                    mesh.add_rect_with_uv(z, f.z.uv, theme::SNORE.gamma_multiply(alpha));
                }
            }
            meshes.paint(&p);
        }

        // The hosts: breathing from the chest, the head riding rigidly on
        // top, a nod when a word lands and a small bob to the beat.
        for i in 0..2 {
            let lips = &self.lips[i];
            let (chest, head) = if still {
                (0., 0.)
            } else {
                let chest = motion::breath(t, i);
                let bob = if lips.talking() { 0. } else { 0.9 * heard.beat };
                // Whole screen pixels, so the face never shimmers.
                (chest, ((chest + lips.nod() + bob) * s).round() / s)
            };
            let bands = [(NECK[i], head), (SHOULDERS[i], chest), (DESK[i], 0.)];
            let mesh = meshes.next(a.hosts[i].id());
            warp(mesh, HOSTS[i], &bands, &at);
            meshes.paint(&p);
            let mesh = meshes.next(a.phones[i].id());
            warp(mesh, PHONES[i], &bands, &at);
            meshes.paint(&p);
            let mesh = meshes.next(atlas);
            let mut face = |frame: Option<Frame>| {
                if let Some(frame) = frame {
                    let [x, y, w, h] = frame.at;
                    mesh.add_rect_with_uv(rect([x, y + head, w, h]), frame.uv, Color32::WHITE);
                }
            };
            let viseme = lips.viseme;
            if viseme != motion::REST {
                face(f.mouths[i][viseme - 1]);
            }
            let look = self.looks[i];
            if look.glance != 0 {
                face(f.looks[i][look.glance - 1]);
            }
            if look.lid != motion::OPEN {
                face(f.lids[i][look.lid - 1]);
            }
            meshes.paint(&p);
        }
        layer(&p, scene, &a.mugs[0], [416., 892., 164., 175.], full_uv());
        layer(&p, scene, &a.mugs[1], [1095., 921., 184., 175.], full_uv());
        // Steam off both mugs, curling as it rises and gone before it reaches the faces.
        if !still {
            let mesh = meshes.next(atlas);
            for (mug, puffs) in self.steam.puffs.iter().enumerate() {
                for puff in puffs {
                    let (x, y, r, alpha) = motion::puff_at(puff);
                    let centre = f.mugs[mug] + vec2(x, y);
                    let dot = Rect::from_center_size(at(centre.x, centre.y), vec2(r, r * 1.2) * 2. * s);
                    mesh.add_rect_with_uv(dot, f.soft.uv, Color32::from_rgba_unmultiplied(236, 226, 214, (alpha * 255.) as u8));
                }
            }
            meshes.paint(&p);
        }
        layer(&p, scene, &a.microphones.texture, a.microphones.area, full_uv());
        if ui
            .interact(rect(CAT), ui.id().with("cat"), Sense::click())
            .on_hover_text("Say hello to the studio cat")
            .clicked()
            && !self.reduced
            && self.cat
        {
            self.kitty.poke(t);
        }
        if !self.reduced {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
    }
}

/// A voice for screenshot runs: phrases of syllables at about four a second,
/// mostly open vowels with the odd hiss and round one, a breath between
/// phrases. With both hosts talking they take turns and overlap a little.
fn synthetic_voice(t: f64, host: usize, both: bool) -> (f32, f32) {
    let (phrase, gap, offset) = if both { (2.6, 2.2, if host == 0 { 0. } else { 2.1 }) } else { (2.4, 0.7, 0.) };
    let cycle = (t + 20. - offset).rem_euclid(phrase + gap);
    if cycle >= phrase {
        return (0., 0.);
    }
    let syllable = cycle * (3.7 + host as f64 * 0.6);
    let n = syllable.floor() as u32;
    let hash = n.wrapping_mul(2_654_435_761).wrapping_add(host as u32 * 97) >> 16;
    let within = syllable.fract() as f32;
    let loud = 0.35 + 0.65 * ((hash % 7) as f32 / 6.);
    let shape = (std::f32::consts::PI * within).sin().powf(0.7);
    let level = 0.004 + 0.2 * loud * shape;
    let tone = match hash % 5 {
        0 if within < 0.5 => 0.4,
        1 => 0.01,
        _ => 0.05,
    };
    (level, tone)
}

/// A colour that adds `k` of a texture's own light rather than covering.
fn additive(k: f32) -> Color32 {
    let v = (k.clamp(0., 1.) * 255.) as u8;
    Color32::from_rgba_premultiplied(v, v, v, 0)
}

/// Draw a layer bent vertically: each band's line moves down by its offset,
/// everything between follows in proportion, and above the first band and
/// below the last the layer just moves.
fn warp(mesh: &mut Mesh, r: [f32; 4], bands: &[(f32, f32)], at: &impl Fn(f32, f32) -> Pos2) {
    let offset = |y: f32| -> f32 {
        let (first, last) = (bands[0], bands[bands.len() - 1]);
        if y <= first.0 {
            return first.1;
        }
        if y >= last.0 {
            return last.1;
        }
        for pair in bands.windows(2) {
            if y <= pair[1].0 {
                let k = (y - pair[0].0) / (pair[1].0 - pair[0].0);
                return pair[0].1 + (pair[1].1 - pair[0].1) * k;
            }
        }
        last.1
    };
    let (top, bottom) = (r[1], r[1] + r[3]);
    let mut row = |y: f32| {
        let v = (y - top) / r[3];
        let dy = offset(y);
        for (u, x) in [(0., r[0]), (1., r[0] + r[2])] {
            mesh.vertices.push(Vertex { pos: at(x, y + dy), uv: pos2(u, v), color: Color32::WHITE });
        }
    };
    row(top);
    for &(y, _) in bands {
        if y > top && y < bottom {
            row(y);
        }
    }
    row(bottom);
    let rows = mesh.vertices.len() as u32 / 2;
    for i in 0..rows - 1 {
        let k = i * 2;
        mesh.indices.extend_from_slice(&[k, k + 1, k + 2, k + 1, k + 3, k + 2]);
    }
}

/// Every drop as a thin streak, brightest at its head, and the splashes on the sill.
fn rain_mesh(mesh: &mut Mesh, rain: &Rain, at: &impl Fn(f32, f32) -> Pos2, s: f32) {
    let uv = egui::epaint::WHITE_UV;
    let weight = 0.85 + 0.4 * rain.weight;
    let mut quad = |a: Pos2, b: Pos2, width: f32, tail: Color32, head: Color32| {
        let along = b - a;
        let length = along.length().max(1e-3);
        let n = vec2(-along.y, along.x) / length * (width * 0.5);
        let k = mesh.vertices.len() as u32;
        for (pos, color) in [(a - n, tail), (a + n, tail), (b - n, head), (b + n, head)] {
            mesh.vertices.push(Vertex { pos, uv, color });
        }
        mesh.indices.extend_from_slice(&[k, k + 1, k + 2, k + 1, k + 3, k + 2]);
    };
    let (r, g, b) = (theme::RAIN.r(), theme::RAIN.g(), theme::RAIN.b());
    for drop in &rain.drops {
        let lean = rain.lean(drop.layer);
        let head = at(drop.x, drop.y);
        let tail = at(drop.x - lean * drop.len, drop.y - drop.len);
        let alpha = (drop.alpha * weight).min(1.);
        let width = (motion::LAYERS[drop.layer].4 * s).max(0.7);
        quad(tail, head, width, Color32::TRANSPARENT, Color32::from_rgba_unmultiplied(r, g, b, (alpha * 255.) as u8));
    }
    for splash in rain.splashes.iter().filter(|s| s.age < motion::SPLASH_LIFE) {
        let k = splash.age / motion::SPLASH_LIFE;
        let alpha = ((1. - k) * 150.) as u8;
        let colour = Color32::from_rgba_unmultiplied(r, g, b, alpha);
        for side in [-1., 1.] {
            let x = splash.x + side * (2. + 6. * k) * splash.size;
            let y = 604. - 7. * (std::f32::consts::PI * k).sin() * splash.size;
            quad(at(x, y + 1.2), at(x, y - 1.2), (1.2 * s).max(0.7), colour, colour);
        }
    }
}

/// `sin` of a phase that turns `cycles_per_second` times a second, worked
/// out in f64 and wrapped before the trig so hours of clock stay exact.
#[cfg(test)]
fn sine(t: f64, cycles_per_second: f64, offset: f64) -> f32 {
    ((t * cycles_per_second * TAU + offset).rem_euclid(TAU)).sin() as f32
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
fn layer(p: &egui::Painter, scene: Rect, texture: &TextureHandle, r: [f32; 4], uv: Rect) {
    let s = scene.width() / W;
    p.image(
        texture.id(),
        Rect::from_min_size(scene.min + vec2(r[0], r[1]) * s, vec2(r[2], r[3]) * s),
        uv,
        Color32::WHITE,
    );
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

/// Decode, and cut every layer down to what the scene uses of it. The
/// microphones are a whole canvas of which a band is drawn; kept whole they
/// would be most of the booth's video memory. Everything that moves comes
/// from the one atlas.
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
    let microphones = art!("microphones.png");
    let microphones = cut(&microphones, opaque_area(&microphones));
    Decoded {
        base: whole(art!("background.png")),
        hosts: [whole(art!("man.png")), whole(art!("woman.png"))],
        phones: [whole(art!("headphones-mav.png")), whole(art!("headphones-rue.png"))],
        mugs: [whole(art!("black-mug.png")), whole(art!("white-mug.png"))],
        microphones,
        atlas: whole(rgba(ATLAS)),
        frames: Frames::parse(FRAMES).expect("the studio frame manifest matches its atlas"),
    }
}

impl Art {
    fn upload(ctx: &egui::Context, decoded: Decoded<ColorImage>) -> Self {
        let texture = |name: &str, image: ColorImage| ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
        let patch = |name: &str, (image, area): (ColorImage, [f32; 4])| Patch { texture: texture(name, image), area };
        let [host_a, host_b] = decoded.hosts;
        let [phones_a, phones_b] = decoded.phones;
        let [mug_a, mug_b] = decoded.mugs;
        Self {
            base: texture("studio-background", decoded.base),
            hosts: [texture("studio-mav", host_a), texture("studio-rue", host_b)],
            phones: [texture("studio-phones-mav", phones_a), texture("studio-phones-rue", phones_b)],
            mugs: [texture("studio-mug-black", mug_a), texture("studio-mug-white", mug_b)],
            microphones: patch("studio-microphones", decoded.microphones),
            atlas: texture("studio-frames", decoded.atlas),
            frames: decoded.frames,
        }
    }
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
    let music = app.airtime.on && !app.studio.reduced;
    let heard = Heard {
        levels: app.host_levels,
        tones: app.host_tones,
        energy: if music { app.studio.spectrum.low_energy() } else { 0. },
        beat: if music { app.studio.spectrum.beat() } else { 0. },
        on_air: app.station.ready(),
    };
    app.studio.scene(ui, layout.scene, heard);
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
            changed |= ui.checkbox(&mut app.studio.lightning, "Lightning").changed();
            changed |= ui
                .checkbox(&mut app.studio.reduced, "Reduced motion")
                .changed();
            ui.small("The cat keeps breathing between antics. The rain and the city lights follow the bass.");
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

    fn studio() -> Studio {
        Studio::new(&std::env::temp_dir().join("defalt-studio-test"))
    }

    #[test]
    fn the_frame_manifest_matches_its_atlas_and_has_every_face() {
        let frames = Frames::parse(FRAMES).expect("frames.json parses");
        let atlas = image::load_from_memory(ATLAS).unwrap();
        assert!(atlas.width() <= 2048 && atlas.height() <= 2048, "atlas is {}x{}", atlas.width(), atlas.height());
        let inside = |f: &Frame| f.uv.min.x >= 0. && f.uv.min.y >= 0. && f.uv.max.x <= 1. && f.uv.max.y <= 1.;
        for host in 0..2 {
            for mouth in &frames.mouths[host] {
                let mouth = mouth.expect("every mouth shape has a frame");
                assert!(inside(&mouth));
                // On the face, and small: a patch, not a head.
                assert!(mouth.at[2] < 160. && mouth.at[3] < 130., "{:?}", mouth.at);
                assert!(HOSTS[host][0] < mouth.at[0] && mouth.at[1] + mouth.at[3] < NECK[host]);
            }
            for lid in frames.lids[host].iter().chain(&frames.looks[host]) {
                let lid = lid.expect("every eye frame is there");
                assert!(inside(&lid) && lid.at[1] + lid.at[3] < NECK[host]);
            }
        }
        assert_eq!(frames.cat.len(), motion::CAT_FRAMES.len());
        for cat in &frames.cat {
            assert!(inside(cat));
            assert!(cat.at[0] >= 0. && cat.at[1] >= 150. && cat.at[1] + cat.at[3] <= 395., "{:?}", cat.at);
        }
        for host in 0..2 {
            assert_eq!(frames.mouths[host].len(), motion::VISEMES.len());
        }
        assert!(!frames.lights.is_empty() && frames.lights.len() <= motion::MAX_WINDOWS);
        for (lit, _) in &frames.lights {
            let centre = (lit.at[0] + lit.at[2] / 2., lit.at[1] + lit.at[3] / 2.);
            assert!(motion::on_glass(centre.0, centre.1), "a city light off the glass at {centre:?}");
        }
        assert!(frames.sign_off.at[0] <= SIGN[0] + 20. && frames.sign_off.at[0] + frames.sign_off.at[2] >= SIGN[0] + SIGN[2] - 20.);
        assert_eq!(frames.window.at[0].round(), motion::WINDOW[0].round());
    }

    #[test]
    fn the_sign_goes_dark_off_air() {
        let frames = Frames::parse(FRAMES).unwrap();
        let atlas = image::load_from_memory(ATLAS).unwrap().to_rgba8();
        let background = image::load_from_memory(include_bytes!("../../web/static/studio-v2/background.png")).unwrap().to_rgba8();
        let (w, h) = (atlas.width() as f32, atlas.height() as f32);
        let f = frames.sign_off;
        let off = image::imageops::crop_imm(&atlas, (f.uv.min.x * w).round() as u32, (f.uv.min.y * h).round() as u32,
            (f.uv.width() * w).round() as u32, (f.uv.height() * h).round() as u32).to_image();
        let (lit, _) = cut_rgba(&background, [f.at[0] + 2., f.at[1] + 2., f.at[2] - 4., f.at[3] - 4.]);
        let red = |image: &image::RgbaImage| image.pixels().map(|p| p[0] as f64).sum::<f64>() / image.pixels().len() as f64;
        assert!(red(&off) < red(&lit) * 0.8, "the tubes stayed lit: {} of {}", red(&off), red(&lit));
    }

    #[test]
    fn a_warped_host_keeps_the_head_rigid_and_the_desk_still() {
        let mut mesh = Mesh::default();
        let at = |x: f32, y: f32| pos2(x, y);
        warp(&mut mesh, HOSTS[0], &[(NECK[0], 2.), (SHOULDERS[0], 1.5), (DESK[0], 0.)], &at);
        for v in &mesh.vertices {
            let source = HOSTS[0][1] + v.uv.y * HOSTS[0][3];
            let moved = v.pos.y - source;
            if source <= NECK[0] { assert!((moved - 2.).abs() < 1e-3, "the head stretched at {source}"); }
            if source >= DESK[0] { assert!(moved.abs() < 1e-3, "the arms moved at {source}"); }
        }
        assert_eq!(mesh.vertices.len(), 10);
        assert_eq!(mesh.indices.len(), 4 * 6);
    }

    #[test]
    fn the_animation_clock_keeps_its_fractions_after_a_long_night() {
        // Ten hours in, a cycle is still a cycle: the phase maths wraps in
        // f64 before any trig, so the same moment reads the same.
        let late = 36_000.0 * 4.8;
        assert!((sine(late + 1.2, 1. / 4.8, 0.) - 1.).abs() < 1e-5);
        assert!((motion::breath(36_000. * 4.1, 0) - motion::breath(0., 0)).abs() < 1e-3);
    }

    #[test]
    fn a_scripted_run_talks_overlaps_and_goes_on_air() {
        let mut studio = studio();
        studio.pose("speaking");
        let mut talked = [0; 2];
        let mut together = 0;
        let mut shapes = std::collections::BTreeSet::new();
        for _ in 0..30 * 12 {
            let heard = studio.scripted();
            studio.step(1. / 30., heard);
            for host in 0..2 {
                if studio.lips[host].viseme != motion::REST { talked[host] += 1; }
                shapes.insert(studio.lips[host].viseme);
            }
            if studio.lips[0].talking() && studio.lips[1].talking() { together += 1; }
        }
        assert!(talked[0] > 60 && talked[1] > 60, "{talked:?}");
        assert!(together > 10, "the hosts never overlapped");
        assert!(shapes.len() >= 5, "only {shapes:?}");
        let mut sign = self::studio();
        sign.pose("sign");
        for _ in 0..90 {
            let heard = sign.scripted();
            sign.step(1. / 30., heard);
        }
        assert_eq!(sign.sign.level, 1.);
    }

    #[test]
    fn reduced_motion_holds_the_booth_still() {
        let mut studio = studio();
        studio.pose("reduced");
        let drops: Vec<f32> = studio.weather.drops.iter().map(|d| d.y).collect();
        for i in 0..300 {
            let loud = if (i / 4) % 2 == 0 { 0.3 } else { 0. };
            studio.step(1. / 30., Heard { levels: [loud, 0.], tones: [0.05, 0.], energy: 1., beat: 1., on_air: true });
            assert!(studio.lips[0].viseme <= motion::SLIGHT);
            assert_eq!(studio.looks[0].lid, motion::OPEN);
            assert_eq!(motion::CAT_FRAMES[studio.pose.frame], "sleep-0");
        }
        assert!(studio.weather.drops.iter().map(|d| d.y).eq(drops), "the rain moved");
        assert_eq!(studio.sign.level, 1.);
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
}
