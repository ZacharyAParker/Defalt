//! What moves in the booth, and when. Plain state stepped by the clock;
//! nothing in here draws. web/static/studio-motion.js is the same machinery,
//! number for number, so the browser booth and this one behave alike.
//!
//! Every piece owns its storage up front (fixed arrays, no Vecs), so stepping
//! a frame allocates nothing.

use std::f32::consts::{PI, TAU};

/// A small repeatable random source (xorshift32). Seeded, so a screenshot
/// run plays the same booth every time.
#[derive(Clone, Copy, Debug)]
pub struct Dice(u32);

impl Dice {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }
    /// 0 up to (not including) 1.
    pub fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / 16_777_216.
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

fn approach(value: f32, target: f32, dt: f32, seconds: f32) -> f32 {
    value + (target - value) * (1. - (-dt / seconds.max(1e-4)).exp())
}

// ── Mouths ───────────────────────────────────────────────────────────────

pub const REST: usize = 0;
pub const SLIGHT: usize = 1;
pub const AH: usize = 2;
pub const WIDE: usize = 3;
pub const OO: usize = 4;
pub const EE: usize = 5;
/// The mouth shapes each host has, in atlas order after the resting mouth.
#[cfg(test)]
pub const VISEMES: [&str; 5] = ["slight", "ah", "wide", "oo", "ee"];

const ATTACK: f32 = 0.015;
const RELEASE: f32 = 0.080;
/// The shortest a mouth shape stays up, so fast speech reads as shapes and
/// not as a flicker.
pub const MIN_HOLD: f32 = 0.075;
const REDUCED_HOLD: f32 = 0.25;
/// Brightness (difference energy over energy) above which a sound is a
/// hiss -- s, f, t, ee -- and below which an open vowel is a round one.
const TEETH: f32 = 0.22;
const ROUND: f32 = 0.018;

/// How open a voice is, 0..1, from its level: -42 dBFS closed, -16 wide.
pub fn openness(level: f32) -> f32 {
    ((20. * level.max(1e-5).log10() + 42.) / 26.).clamp(0., 1.)
}

/// The mouth shape a sound asks for, with a little stickiness toward the
/// shape already showing so it does not dither between two.
pub fn viseme_for(open: f32, tone: f32, current: usize) -> usize {
    let sticky = |shape: usize| if current == shape { 0.05 } else { 0. };
    if open < 0.10 - if current == REST { 0. } else { 0.03 } {
        return REST;
    }
    if tone > TEETH - sticky(EE) * 0.6 && open < 0.75 {
        return EE;
    }
    if open < 0.34 + sticky(SLIGHT) {
        return SLIGHT;
    }
    if tone < ROUND + sticky(OO) * 0.1 {
        return OO;
    }
    if open < 0.68 + sticky(AH) - sticky(WIDE) {
        AH
    } else {
        WIDE
    }
}

/// One host's lips: an envelope over their voice, a mouth shape chosen from
/// it, and a small nod when a word lands harder than the ones around it.
#[derive(Clone, Copy, Debug)]
pub struct Lips {
    pub level: f32,
    slow: f32,
    tone: f32,
    pub viseme: usize,
    held: f32,
    nod_age: f32,
    /// Seconds since the voice was last open.
    quiet: f32,
}

impl Default for Lips {
    /// Quiet, closed, and free to open on the first word.
    fn default() -> Self {
        Self { level: 0., slow: 0., tone: 0., viseme: REST, held: 9., nod_age: 9., quiet: 9. }
    }
}

impl Lips {
    /// `rms` is the voice's level over the last few milliseconds, `tone`
    /// its brightness (see `TEETH`).
    pub fn step(&mut self, dt: f32, rms: f32, tone: f32, reduced: bool) -> usize {
        let dt = dt.clamp(0., 0.25);
        let rms = if rms.is_finite() { rms.max(0.) } else { 0. };
        self.level = approach(self.level, rms, dt, if rms > self.level { ATTACK } else { RELEASE });
        self.slow = approach(self.slow, self.level, dt, 0.6);
        let open = openness(self.level);
        if open > 0.10 {
            self.tone = approach(self.tone, if tone.is_finite() { tone } else { 0. }, dt, 0.04);
            self.quiet = 0.;
        } else {
            self.quiet += dt;
        }
        self.held += dt;
        self.nod_age += dt;
        let target = if reduced {
            if open > 0.10 { SLIGHT } else { REST }
        } else {
            viseme_for(open, self.tone, self.viseme)
        };
        let hold = if reduced { REDUCED_HOLD } else { MIN_HOLD };
        if target != self.viseme && self.held >= hold {
            self.viseme = target;
            self.held = 0.;
        }
        if !reduced && open > 0.45 && self.level > self.slow * 1.8 && self.nod_age > 0.7 {
            self.nod_age = 0.;
        }
        self.viseme
    }

    /// Talking, give or take the gaps between words.
    pub fn talking(&self) -> bool {
        self.quiet < 0.5
    }

    /// How far the head dips for emphasis right now, in scene units.
    pub fn nod(&self) -> f32 {
        if self.nod_age < 0.42 { 1.6 * (PI * self.nod_age / 0.42).sin() } else { 0. }
    }
}

// ── Eyes ─────────────────────────────────────────────────────────────────

pub const OPEN: usize = 0;
pub const HALF: usize = 1;
pub const CLOSED: usize = 2;
pub const BLINK: f64 = 0.18;
const DOUBLE_GAP: f64 = 0.26;

/// Which eyelid frame shows `age` seconds into a blink.
pub fn lid_at(age: f64) -> usize {
    if age < 0. {
        OPEN
    } else if age < 0.05 {
        HALF
    } else if age < 0.12 {
        CLOSED
    } else if age < BLINK {
        HALF
    } else {
        OPEN
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub lid: usize,
    /// 0 the drawn gaze; 1 the host's other way (Mav to the room, Rue to
    /// Mav); 2 down at the desk.
    pub glance: usize,
}

/// A host's blinks and glances.
#[derive(Clone, Copy, Debug)]
pub struct Eyes {
    dice: Dice,
    pub next: f64,
    start: f64,
    double: bool,
    glance: usize,
    until: f64,
    next_look: f64,
    /// Mav is drawn looking at Rue already; Rue is drawn looking out.
    faces_other: bool,
}

impl Eyes {
    pub fn new(seed: u32, faces_other: bool) -> Self {
        let mut dice = Dice::new(seed);
        let next = dice.range(0.8, 3.) as f64;
        Self { dice, next, start: -10., double: false, glance: 0, until: 0., next_look: 2., faces_other }
    }

    fn length(&self) -> f64 {
        if self.double { DOUBLE_GAP + BLINK } else { BLINK }
    }

    pub fn step(&mut self, t: f64, talking: bool, other_talking: bool, reduced: bool) -> Look {
        if reduced {
            self.glance = 0;
            self.start = -10.;
            return Look { lid: OPEN, glance: 0 };
        }
        let idle = !talking && !other_talking;
        if self.glance != 0 && t >= self.until {
            self.glance = 0;
            self.next_look = t + self.dice.range(1.5, 4.) as f64;
        }
        if self.glance == 0 && t >= self.next_look {
            let roll = self.dice.next();
            let wanted = if other_talking {
                if self.faces_other { usize::from(roll < 0.12) * 2 } else if roll < 0.65 { 1 } else { 0 }
            } else if talking {
                if roll < 0.35 && self.faces_other { 1 } else if roll < 0.5 { 2 } else { 0 }
            } else if roll < 0.3 {
                2
            } else if roll < 0.5 {
                1
            } else {
                0
            };
            if wanted != 0 {
                self.glance = wanted;
                // Watching the other host talk lasts longer than a glance away.
                let (lo, hi) = if other_talking && wanted == 1 { (1.5, 3.5) } else { (0.8, 2.2) };
                self.until = t + self.dice.range(lo, hi) as f64;
                // Most changes of gaze take a blink with them.
                if self.dice.next() < 0.5 && t - self.start > self.length() {
                    self.next = t;
                }
            } else {
                self.next_look = t + self.dice.range(1.5, 4.) as f64;
            }
        }
        if t - self.start >= self.length() && t >= self.next {
            self.start = t;
            self.double = self.dice.next() < 0.18;
            let gap = self.dice.range(2., 6.) * if idle { 1.4 } else { 1. };
            self.next = t + self.length() + gap as f64;
        }
        let age = t - self.start;
        let lid = if self.double && age >= DOUBLE_GAP { lid_at(age - DOUBLE_GAP) } else { lid_at(age) };
        Look { lid, glance: self.glance }
    }
}

/// How high a host's chest has risen, in scene units: about a breath every
/// four seconds, eased at both ends.
pub fn breath(t: f64, host: usize) -> f32 {
    let (period, offset) = if host == 0 { (4.1, 0.) } else { (4.7, 0.37) };
    let phase = ((t / period + offset).rem_euclid(1.)) as f32;
    2.0 * (0.5 - 0.5 * (TAU * phase).cos())
}

// ── The cat ──────────────────────────────────────────────────────────────

/// Every cat frame, in atlas order. The first eight are the sleeping breath.
pub const CAT_FRAMES: [&str; 25] = [
    "sleep-0", "sleep-1", "sleep-2", "sleep-3", "sleep-4", "sleep-5", "sleep-6", "sleep-7",
    "ear-flick", "ear-back", "tail-up", "tail-flick", "tail-down",
    "drowsy", "awake", "look", "yawn-a", "yawn-b", "squint",
    "groom-paw", "groom-lick", "groom-wipe", "stretch-a", "stretch-b", "perk",
];
const fn frame(name: &str) -> usize {
    let mut i = 0;
    while i < CAT_FRAMES.len() {
        if eq(CAT_FRAMES[i], name) {
            return i;
        }
        i += 1;
    }
    panic!("unknown cat frame")
}
const fn eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
/// "Whatever the sleeping breath shows now."
pub const BREATHING: usize = usize::MAX;
const SLEEP_FRAMES: usize = 8;
/// The breath's frames in order from out to in.
const SLEEP_ORDER: [usize; SLEEP_FRAMES] =
    [frame("sleep-0"), frame("sleep-1"), frame("sleep-2"), frame("sleep-3"),
     frame("sleep-4"), frame("sleep-5"), frame("sleep-6"), frame("sleep-7")];

type Clip = &'static [(usize, f32)];
macro_rules! clip {
    ($($name:literal $hold:literal),* $(,)?) => { &[$((frame($name), $hold)),*] };
}
const EAR: Clip = clip!["ear-flick" 0.12, "sleep-0" 0.10, "ear-flick" 0.10, "sleep-0" 0.35, "ear-flick" 0.09];
const EAR_BACK: Clip = clip!["ear-back" 1.1, "sleep-0" 0.25, "ear-back" 0.6];
const TAIL: Clip = clip!["tail-up" 0.20, "tail-flick" 0.26, "tail-up" 0.16, "tail-down" 0.55, "tail-up" 0.22,
                        "tail-flick" 0.24, "tail-up" 0.2];
const WAKE: Clip = clip!["drowsy" 0.6, "awake" 1.3, "yawn-a" 0.28, "yawn-b" 1.15, "yawn-a" 0.22, "squint" 0.55,
                        "awake" 1.6, "look" 1.2, "awake" 0.6, "drowsy" 0.7];
const GROOM: Clip = clip!["drowsy" 0.35, "awake" 0.7, "groom-paw" 0.35, "groom-lick" 0.3, "groom-paw" 0.22,
                         "groom-lick" 0.3, "groom-paw" 0.22, "groom-lick" 0.3, "groom-wipe" 0.5, "groom-paw" 0.3,
                         "groom-wipe" 0.45, "awake" 0.9, "drowsy" 0.6];
const STRETCH: Clip = clip!["drowsy" 0.35, "awake" 0.6, "stretch-a" 0.45, "stretch-b" 1.4, "stretch-a" 0.4,
                           "awake" 0.9, "squint" 0.4, "drowsy" 0.6];
const PERK: Clip = clip!["perk" 0.5, "tail-up" 0.14, "tail-flick" 0.2, "perk" 0.25, "look" 0.9, "perk" 0.4,
                        "drowsy" 0.6];
/// What the cat gets up to, and how often it picks each.
pub const CLIPS: [(&str, Clip, f32); 7] = [
    ("ear", EAR, 3.),
    ("ear-back", EAR_BACK, 2.),
    ("tail", TAIL, 3.),
    ("wake", WAKE, 2.),
    ("groom", GROOM, 2.),
    ("stretch", STRETCH, 1.5),
    ("perk", PERK, 0.),
];
pub const PERK_CLIP: usize = 6;

pub fn clip_length(clip: usize) -> f32 {
    CLIPS[clip].1.iter().map(|(_, hold)| hold).sum()
}

/// Which frame of `clip` shows `age` seconds in (`BREATHING` once it is over).
pub fn clip_frame(clip: usize, age: f32) -> usize {
    let mut at = 0.;
    for &(frame, hold) in CLIPS[clip].1 {
        at += hold;
        if age < at {
            return frame;
        }
    }
    BREATHING
}

/// The sleeping breath: one in and out every 3.8 seconds, lingering at the
/// ends the way a real one does.
pub fn sleeping_frame(t: f64) -> usize {
    let phase = (t / 3.8).rem_euclid(1.) as f32;
    let depth = 0.5 - 0.5 * (TAU * phase).cos();
    SLEEP_ORDER[((depth * (SLEEP_FRAMES - 1) as f32).round() as usize).min(SLEEP_FRAMES - 1)]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CatPose {
    pub frame: usize,
    /// Still asleep (a twitch in its sleep counts), so the Zs keep coming.
    pub asleep: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Cat {
    dice: Dice,
    clip: Option<(usize, f64)>,
    pub next: f64,
    last: usize,
    reacted: f64,
}

/// The first antic, from when the booth first appears.
pub const FIRST: (f32, f32) = (20., 40.);
/// Between antics after that.
pub const BETWEEN: (f32, f32) = (45., 120.);

impl Cat {
    pub fn new(seed: u32, t: f64) -> Self {
        let mut dice = Dice::new(seed);
        let next = t + dice.range(FIRST.0, FIRST.1) as f64;
        Self { dice, clip: None, next, last: usize::MAX, reacted: -1e9 }
    }

    #[cfg(test)]
    pub fn playing(&self) -> Option<usize> {
        self.clip.map(|(clip, _)| clip)
    }

    /// Start a routine now (a click, or a screenshot asking for one).
    pub fn play(&mut self, clip: usize, t: f64) {
        self.clip = Some((clip, t));
        self.last = clip;
    }

    /// Say hello: an ear up and a flick of the tail, unless it is already
    /// busy with something bigger than a twitch.
    pub fn poke(&mut self, t: f64) {
        if matches!(self.clip, None | Some((0..=2, _))) {
            self.play(PERK_CLIP, t);
            self.next = self.next.max(t + 30.);
        }
    }

    /// Both hosts talking at once: the cat looks up, now and then.
    pub fn crowd(&mut self, t: f64) {
        if self.clip.is_none() && t - self.reacted > 120. {
            self.reacted = t;
            self.play(PERK_CLIP, t);
        }
    }

    fn choose(&mut self) -> usize {
        let total: f32 = CLIPS.iter().enumerate().filter(|(i, _)| *i != self.last).map(|(_, c)| c.2).sum();
        let mut roll = self.dice.next() * total;
        for (i, clip) in CLIPS.iter().enumerate() {
            if i == self.last || clip.2 <= 0. {
                continue;
            }
            if roll < clip.2 {
                return i;
            }
            roll -= clip.2;
        }
        0
    }

    pub fn step(&mut self, t: f64, enabled: bool, reduced: bool) -> CatPose {
        if !enabled || reduced {
            self.clip = None;
            if t + FIRST.0 as f64 > self.next {
                self.next = t + self.dice.range(FIRST.0, FIRST.1) as f64;
            }
            let frame = if reduced { SLEEP_ORDER[0] } else { sleeping_frame(t) };
            return CatPose { frame, asleep: true };
        }
        if let Some((clip, start)) = self.clip {
            if (t - start) as f32 >= clip_length(clip) {
                self.clip = None;
                self.next = t + self.dice.range(BETWEEN.0, BETWEEN.1) as f64;
            }
        }
        if self.clip.is_none() && t >= self.next {
            let clip = self.choose();
            self.play(clip, t);
        }
        match self.clip {
            Some((clip, start)) => {
                let frame = clip_frame(clip, (t - start) as f32);
                let asleep = clip <= 2;
                let frame = if frame == BREATHING || frame == SLEEP_ORDER[0] { sleeping_frame(t) } else { frame };
                CatPose { frame, asleep }
            }
            None => CatPose { frame: sleeping_frame(t), asleep: true },
        }
    }
}

/// Where one of the cat's Zs is `k` (0..1) of the way through its drift,
/// relative to where they start, with its size and opacity.
pub fn z_at(k: f32) -> (f32, f32, f32, f32) {
    let x = 12. * k + 4. * (TAU * (k * 1.1 + 0.15)).sin() * k;
    let y = -40. * k;
    let size = 0.65 + 0.55 * k;
    let alpha = (PI * k).sin().powf(0.8) * 0.8;
    (x, y, size, alpha)
}

// ── Rain ─────────────────────────────────────────────────────────────────

/// The glass, in scene units: two panes either side of the mullion.
pub const PANES: [[f32; 4]; 2] = [[463., 0., 218., 606.], [722., 0., 387., 606.]];
/// The whole window, frame and all: everything rain can be drawn in.
pub const WINDOW: [f32; 4] = [455., 0., 663., 614.];
const SILL: f32 = 606.;

pub fn on_glass(x: f32, y: f32) -> bool {
    PANES.iter().any(|p| x >= p[0] && x < p[0] + p[2] && y >= p[1] && y < p[1] + p[3])
}

/// Far, middle and near rain: count, speed, length, opacity, width, and how
/// much the wind leans it.
pub const LAYERS: [(usize, [f32; 2], [f32; 2], [f32; 2], f32, f32); 3] = [
    (90, [380., 470.], [9., 15.], [0.2, 0.32], 0.8, 0.7),
    (48, [560., 700.], [17., 26.], [0.3, 0.44], 1.1, 1.0),
    (16, [820., 1000.], [30., 44.], [0.26, 0.38], 1.6, 1.35),
];
pub const DROPS: usize = 90 + 48 + 16;
pub const BEADS: usize = 12;
pub const SPLASHES: usize = 8;

#[derive(Clone, Copy, Debug, Default)]
pub struct Drop {
    pub x: f32,
    pub y: f32,
    pub speed: f32,
    pub len: f32,
    pub alpha: f32,
    pub layer: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Bead {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    grow: f32,
    pub top: f32,
    pub speed: f32,
    pub age: f32,
    delay: f32,
    /// 0 waiting, 1 gathering on the glass, 2 running down it.
    pub state: u8,
    run: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Splash {
    pub x: f32,
    pub age: f32,
    pub size: f32,
}
pub const SPLASH_LIFE: f32 = 0.3;

pub struct Rain {
    dice: Dice,
    pub drops: [Drop; DROPS],
    pub beads: [Bead; BEADS],
    pub splashes: [Splash; SPLASHES],
    /// How far the rain leans: dx per unit of fall.
    pub wind: f32,
    gust: f32,
    gust_at: f64,
    /// The bass, smoothed again, as the rain feels it.
    pub weight: f32,
    flash_at: f64,
    next_flash: f64,
}

impl Rain {
    pub fn new(seed: u32) -> Self {
        let mut rain = Self {
            dice: Dice::new(seed),
            drops: [Drop::default(); DROPS],
            beads: [Bead::default(); BEADS],
            splashes: [Splash { age: SPLASH_LIFE, ..Default::default() }; SPLASHES],
            wind: 0.12,
            gust: 0.,
            gust_at: 6.,
            weight: 0.,
            flash_at: -100.,
            next_flash: 0.,
        };
        let mut i = 0;
        for (layer, spec) in LAYERS.iter().enumerate() {
            for _ in 0..spec.0 {
                let mut drop = rain.fresh(layer);
                drop.y = rain.dice.range(-40., SILL);
                rain.drops[i] = drop;
                i += 1;
            }
        }
        for b in 0..BEADS {
            rain.beads[b] = rain.bead();
            rain.beads[b].delay = rain.dice.range(0., 5.);
        }
        rain.next_flash = rain.dice.range(35., 110.) as f64;
        rain
    }

    fn fresh(&mut self, layer: usize) -> Drop {
        let (_, speed, len, alpha, _, _) = LAYERS[layer];
        let len = self.dice.range(len[0], len[1]);
        Drop {
            // Upwind of the window too, so a leaning drop can arrive from off the side.
            x: self.dice.range(WINDOW[0] - 90., WINDOW[0] + WINDOW[2]),
            y: -len - self.dice.range(0., 60.),
            speed: self.dice.range(speed[0], speed[1]),
            len,
            alpha: self.dice.range(alpha[0], alpha[1]),
            layer,
        }
    }

    fn bead(&mut self) -> Bead {
        let pane = if self.dice.next() < 0.36 { PANES[0] } else { PANES[1] };
        let r = self.dice.range(1.2, 2.6);
        let y = self.dice.range(20., pane[3] - 60.);
        Bead {
            x: self.dice.range(pane[0] + 6., pane[0] + pane[2] - 6.),
            y,
            r: 0.,
            grow: r,
            top: y,
            speed: 0.,
            age: 0.,
            delay: self.dice.range(0.5, 4.),
            state: 0,
            run: self.dice.range(80., 260.),
        }
    }

    /// How far each layer's drops lean right now.
    pub fn lean(&self, layer: usize) -> f32 {
        self.wind * LAYERS[layer].5
    }

    pub fn step(&mut self, dt: f32, t: f64, energy: f32) {
        let dt = dt.clamp(0., 0.1);
        self.weight = approach(self.weight, energy.clamp(0., 1.), dt, 0.5);
        // Wind: a slow wander, and now and then a gust that leans it all over.
        if t >= self.gust_at {
            self.gust = self.dice.range(0.08, 0.2);
            self.gust_at = t + self.dice.range(8., 20.) as f64;
        }
        self.gust = approach(self.gust, 0., dt, 2.5);
        let tf = (t % 10_000.) as f32;
        let wander = 0.10 + 0.05 * (tf * 0.23).sin() + 0.03 * (tf * 0.61 + 1.3).sin();
        self.wind = approach(self.wind, wander + self.gust, dt, 0.8);
        for i in 0..DROPS {
            let lean = self.lean(self.drops[i].layer);
            let drop = &mut self.drops[i];
            let fall = drop.speed * (1. + 0.12 * self.weight) * dt;
            drop.y += fall;
            drop.x += fall * lean;
            if drop.y - drop.len > SILL {
                let (x, layer) = (drop.x, drop.layer);
                if layer > 0 && self.dice.next() < 0.3 && on_glass(x, SILL - 2.) {
                    if let Some(splash) = self.splashes.iter_mut().find(|s| s.age >= SPLASH_LIFE) {
                        *splash = Splash { x, age: 0., size: if layer == 2 { 1.4 } else { 1. } };
                    }
                }
                self.drops[i] = self.fresh(layer);
            }
        }
        for splash in self.splashes.iter_mut() {
            splash.age = (splash.age + dt).min(SPLASH_LIFE);
        }
        for b in 0..BEADS {
            let bead = &mut self.beads[b];
            bead.age += dt;
            match bead.state {
                0 if bead.age >= bead.delay => {
                    bead.state = 1;
                    bead.age = 0.;
                }
                1 => {
                    bead.r = bead.grow * (bead.age / 2.5).min(1.).sqrt();
                    if bead.age > 2.5 + bead.grow {
                        bead.state = 2;
                        bead.age = 0.;
                        bead.top = bead.y;
                    }
                }
                2 => {
                    bead.speed = (bead.speed + dt * 160.).min(150.);
                    bead.y += bead.speed * dt;
                    bead.x += (bead.age * 9.).sin() * 4. * dt;
                    bead.r = (bead.r - dt * 0.35).max(0.8);
                    if bead.y > SILL - 2. || bead.y - bead.top > bead.run {
                        self.beads[b] = self.bead();
                    }
                }
                _ => {}
            }
        }
    }

    /// Lightning, 0..1: a hard flash, a second, and a fading afterglow.
    pub fn flash(&self, t: f64) -> f32 {
        let age = (t - self.flash_at) as f32;
        if !(0. ..2.).contains(&age) {
            return 0.;
        }
        let pulse = |at: f32, fade: f32, peak: f32| if age >= at { peak * (-(age - at) / fade).exp() } else { 0. };
        (pulse(0., 0.07, 1.) + pulse(0.17, 0.11, 0.75) + pulse(0.42, 0.35, 0.22)).min(1.)
    }

    /// Strike now and then, when lightning is allowed.
    pub fn storm(&mut self, t: f64, allowed: bool) {
        if !allowed {
            self.next_flash = self.next_flash.max(t + 20.);
            return;
        }
        if t >= self.next_flash {
            self.strike(t);
        }
    }

    pub fn strike(&mut self, t: f64) {
        self.flash_at = t;
        self.next_flash = t + self.dice.range(35., 110.) as f64;
    }
}

// ── The city, the lamp and the mugs ──────────────────────────────────────

pub const MAX_WINDOWS: usize = 64;

#[derive(Clone, Copy, Debug, Default)]
pub struct Window {
    /// 1 lit, 0 dark, and anything between while it switches.
    pub on: f32,
    target: f32,
    phase: f32,
    rate: f32,
    /// A television rather than a lamp: it flickers.
    screen: bool,
}

pub struct Lights {
    dice: Dice,
    pub windows: [Window; MAX_WINDOWS],
    pub count: usize,
    next: f64,
    dip_at: f64,
    dip: f32,
}

impl Lights {
    pub fn new(seed: u32, count: usize) -> Self {
        let mut dice = Dice::new(seed);
        let mut windows = [Window::default(); MAX_WINDOWS];
        for w in windows.iter_mut() {
            *w = Window { on: 1., target: 1., phase: dice.range(0., TAU), rate: dice.range(0.2, 0.7), screen: dice.next() < 0.12 };
        }
        let next = dice.range(5., 12.) as f64;
        let dip_at = dice.range(12., 30.) as f64;
        Self { dice, windows, count: count.min(MAX_WINDOWS), next, dip_at, dip: 0. }
    }

    pub fn step(&mut self, dt: f32, t: f64) {
        if self.count > 0 && t >= self.next {
            self.next = t + self.dice.range(5., 16.) as f64;
            let pick = ((self.dice.next() * self.count as f32) as usize).min(self.count - 1);
            let dark = self.windows[..self.count].iter().filter(|w| w.target < 0.5).count();
            let window = &mut self.windows[pick];
            window.target = if window.target > 0.5 && dark < 3 { 0. } else { 1. };
        }
        for w in self.windows[..self.count].iter_mut() {
            // Lights snap on and off, but a bulb takes a moment either way.
            let step = dt / 0.3;
            w.on = if w.on < w.target { (w.on + step).min(w.target) } else { (w.on - step).max(w.target) };
        }
        if t >= self.dip_at {
            self.dip = 1.;
            self.dip_at = t + self.dice.range(15., 40.) as f64;
        }
        self.dip = (self.dip - dt / 0.18).max(0.);
    }

    /// How much brighter than painted a lit window is now, 0..~0.3.
    pub fn glow(&self, i: usize, t: f64, energy: f32) -> f32 {
        let w = &self.windows[i];
        let tf = (t % 10_000.) as f32;
        let twinkle = if w.screen {
            0.12 + 0.1 * (tf * 7.1 + w.phase).sin() * (tf * 2.3 + w.phase).sin() + 0.06 * (tf * 13.7).sin()
        } else {
            0.08 + 0.07 * (tf * w.rate + w.phase).sin()
        };
        (twinkle + 0.12 * energy).max(0.) * w.on
    }

    /// The desk lamp, 1 as painted: a soft flutter and now and then a dip.
    pub fn lamp(&self, t: f64) -> f32 {
        let tf = (t % 10_000.) as f32;
        1. + 0.035 * (tf * 7.3).sin() + 0.02 * (tf * 13.1 + 1.).sin() - 0.14 * self.dip * (PI * self.dip).sin()
    }
}

pub const PUFFS: usize = 7;

#[derive(Clone, Copy, Debug, Default)]
pub struct Puff {
    pub age: f32,
    pub life: f32,
    pub drift: f32,
    pub phase: f32,
    pub spread: f32,
}

/// Steam off both mugs: a few soft puffs each, curling as they rise.
pub struct Steam {
    dice: Dice,
    pub puffs: [[Puff; PUFFS]; 2],
}

impl Steam {
    pub fn new(seed: u32) -> Self {
        let mut dice = Dice::new(seed);
        let mut puffs = [[Puff::default(); PUFFS]; 2];
        for mug in puffs.iter_mut() {
            for (i, puff) in mug.iter_mut().enumerate() {
                *puff = Self::puff(&mut dice);
                puff.age = puff.life * i as f32 / PUFFS as f32;
            }
        }
        Self { dice, puffs }
    }

    fn puff(dice: &mut Dice) -> Puff {
        Puff {
            age: 0.,
            life: dice.range(3., 4.4),
            drift: dice.range(-6., 10.),
            phase: dice.range(0., 1.),
            spread: dice.range(-7., 7.),
        }
    }

    pub fn step(&mut self, dt: f32) {
        for mug in 0..2 {
            for i in 0..PUFFS {
                let puff = &mut self.puffs[mug][i];
                puff.age += dt.clamp(0., 0.1);
                if puff.age >= puff.life {
                    self.puffs[mug][i] = Self::puff(&mut self.dice);
                }
            }
        }
    }
}

/// Where a puff is: offset from the mug, radius, opacity.
pub fn puff_at(p: &Puff) -> (f32, f32, f32, f32) {
    let k = (p.age / p.life).clamp(0., 1.);
    let rise = 1. - (1. - k) * (1. - k);
    let x = p.spread * 0.4 + p.drift * k + 6. * k * (TAU * (k * 0.9 + p.phase)).sin();
    let y = -70. * rise;
    let r = 4. + 12. * k;
    let alpha = 0.11 * (PI * k).sin().powf(1.2);
    (x, y, r, alpha)
}

// ── The sign ─────────────────────────────────────────────────────────────

/// Neon warming up: (seconds, brightness), held until the next step.
pub const IGNITION: [(f32, f32); 12] = [
    (0., 0.), (0.08, 0.85), (0.13, 0.05), (0.3, 0.), (0.38, 0.7), (0.43, 0.2),
    (0.55, 0.95), (0.6, 0.45), (0.72, 1.), (1.05, 0.8), (1.12, 1.), (1.6, 1.),
];
pub const IGNITION_LENGTH: f32 = 1.6;

#[derive(Clone, Copy, Debug)]
pub struct Sign {
    on: bool,
    since: f64,
    from: f32,
    pub level: f32,
}

impl Default for Sign {
    fn default() -> Self {
        Self { on: false, since: -100., from: 0., level: 0. }
    }
}

impl Sign {
    pub fn step(&mut self, t: f64, on_air: bool, reduced: bool) -> f32 {
        if on_air != self.on {
            self.on = on_air;
            self.since = t;
            self.from = self.level;
        }
        let age = (t - self.since) as f32;
        self.level = if self.on {
            if reduced {
                (self.from + age / 0.3).min(1.)
            } else if age < IGNITION_LENGTH && self.from < 0.5 {
                IGNITION.iter().rev().find(|(at, _)| age >= *at).map_or(0., |(_, level)| *level)
            } else {
                1.
            }
        } else if reduced {
            (self.from - age / 0.3).max(0.)
        } else {
            // Off: a last blink as the power goes, then dark.
            let fade = (self.from * (1. - age / 0.35)).max(0.);
            if (0.1..0.16).contains(&age) { fade * 0.3 } else { fade }
        };
        self.level
    }

    /// The glow breathing a little while lit, and the hum of the tubes.
    pub fn pulse(&self, t: f64, reduced: bool) -> f32 {
        if reduced {
            return 1.;
        }
        let tf = (t % 10_000.) as f32;
        1. + 0.06 * (TAU * 0.3 * tf).sin() + 0.015 * (TAU * 7.7 * tf).sin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speak(lips: &mut Lips, seconds: f32, rms: f32, tone: f32, reduced: bool) -> Vec<usize> {
        (0..(seconds * 1000.) as usize).map(|_| lips.step(0.001, rms, tone, reduced)).collect()
    }

    #[test]
    fn dice_are_repeatable_and_in_range() {
        let (mut a, mut b) = (Dice::new(7), Dice::new(7));
        for _ in 0..1000 {
            let x = a.next();
            assert_eq!(x, b.next());
            assert!((0. ..1.).contains(&x));
        }
    }

    #[test]
    fn the_mouth_follows_loudness_and_tone() {
        assert_eq!(viseme_for(0.02, 0.05, REST), REST);
        assert_eq!(viseme_for(0.25, 0.05, REST), SLIGHT);
        assert_eq!(viseme_for(0.5, 0.05, REST), AH);
        assert_eq!(viseme_for(0.9, 0.05, REST), WIDE);
        assert_eq!(viseme_for(0.5, 0.005, REST), OO);
        assert_eq!(viseme_for(0.4, 0.4, REST), EE);
        // Sticky: a mouth on the edge between two shapes keeps the one it has.
        assert_eq!(viseme_for(0.70, 0.05, AH), AH);
        assert_eq!(viseme_for(0.70, 0.05, REST), WIDE);
        assert_eq!(viseme_for(0.085, 0.05, SLIGHT), SLIGHT);
        assert_eq!(viseme_for(0.085, 0.05, REST), REST);
    }

    #[test]
    fn a_word_opens_the_mouth_fast_and_a_pause_closes_it() {
        let mut lips = Lips::default();
        let open = speak(&mut lips, 0.06, 0.12, 0.05, false);
        let first = open.iter().position(|v| *v != REST).expect("never opened");
        assert!(first < 30, "took {first} ms to open");
        speak(&mut lips, 0.3, 0.12, 0.05, false);
        assert!(lips.viseme == AH || lips.viseme == WIDE, "{}", lips.viseme);
        let pause = speak(&mut lips, 0.4, 0., 0., false);
        assert_eq!(*pause.last().unwrap(), REST, "the mouth closes at a pause");
        let closed = pause.iter().position(|v| *v == REST).unwrap();
        assert!(closed < 300, "took {closed} ms to close");
    }

    #[test]
    fn shapes_hold_long_enough_to_read() {
        // A voice chattering between loud and quiet every 20 ms still only
        // changes shape every MIN_HOLD at the fastest.
        let mut lips = Lips::default();
        let mut shapes = Vec::new();
        for i in 0..2000 {
            let rms = if (i / 20) % 2 == 0 { 0.2 } else { 0.004 };
            let tone = if (i / 55) % 3 == 0 { 0.4 } else { 0.03 };
            shapes.push(lips.step(0.001, rms, tone, false));
        }
        let mut run = 0;
        let mut shortest = usize::MAX;
        for w in shapes.windows(2) {
            run += 1;
            if w[0] != w[1] {
                shortest = shortest.min(run);
                run = 0;
            }
        }
        assert!(shortest as f32 >= MIN_HOLD * 1000. - 1., "a shape lasted only {shortest} ms");
    }

    #[test]
    fn reduced_motion_is_open_or_closed_and_never_flickers() {
        let mut lips = Lips::default();
        let mut shapes = Vec::new();
        for i in 0..3000 {
            let rms = if (i / 30) % 2 == 0 { 0.3 } else { 0. };
            shapes.push(lips.step(0.001, rms, 0.5, true));
        }
        assert!(shapes.iter().all(|v| *v == REST || *v == SLIGHT));
        let changes = shapes.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(changes <= 3000 / 250 + 1, "{changes} changes in 3 s");
        assert_eq!(lips.nod(), 0.);
    }

    #[test]
    fn a_stressed_word_nods_the_head_and_it_comes_back() {
        let mut lips = Lips::default();
        speak(&mut lips, 1., 0.02, 0.05, false);
        let mut deepest = 0f32;
        for _ in 0..300 {
            lips.step(0.001, 0.3, 0.05, false);
            deepest = deepest.max(lips.nod());
        }
        assert!(deepest > 1. && deepest <= 2., "{deepest}");
        speak(&mut lips, 0.5, 0.3, 0.05, false);
        assert_eq!(lips.nod(), 0.);
    }

    #[test]
    fn blinks_come_every_two_to_six_seconds_and_sometimes_twice() {
        let mut eyes = Eyes::new(3, false);
        let mut starts = Vec::new();
        let mut doubles = 0;
        let mut last = OPEN;
        let mut closed_run = 0;
        let mut t = 0.;
        while t < 600. {
            let look = eyes.step(t, true, false, false);
            if look.lid != OPEN && last == OPEN {
                starts.push(t);
            }
            if look.lid == CLOSED { closed_run += 1; }
            last = look.lid;
            t += 0.01;
        }
        let gaps: Vec<f64> = starts.windows(2).map(|w| w[1] - w[0]).collect();
        for gap in &gaps {
            if *gap < 0.5 { doubles += 1; }
        }
        assert!(doubles > 3, "no double blinks ({doubles})");
        let singles: Vec<f64> = gaps.iter().copied().filter(|g| *g > 0.5).collect();
        let mean = singles.iter().sum::<f64>() / singles.len() as f64;
        assert!((2.5..5.5).contains(&mean), "mean gap {mean}");
        assert!(singles.iter().all(|g| *g < 8.5), "a gap was far too long");
        assert!(closed_run > 0);
        // The half-closed frame shows on the way down and up.
        assert_eq!(lid_at(0.02), HALF);
        assert_eq!(lid_at(0.08), CLOSED);
        assert_eq!(lid_at(0.15), HALF);
        assert_eq!(lid_at(0.3), OPEN);
    }

    #[test]
    fn rue_looks_at_mav_while_he_talks() {
        let mut rue = Eyes::new(11, false);
        let mut toward = 0;
        let mut t = 0.;
        while t < 120. {
            if rue.step(t, false, true, false).glance == 1 { toward += 1; }
            t += 0.05;
        }
        assert!(toward > 600, "Rue barely looked over ({toward} of 2400 frames)");
        // Mav, already drawn looking at her, doesn't look away to do it.
        let mut mav = Eyes::new(12, true);
        let mut away = 0;
        let mut t = 0.;
        while t < 120. {
            if mav.step(t, false, true, false).glance == 1 { away += 1; }
            t += 0.05;
        }
        assert_eq!(away, 0);
        let mut still = Eyes::new(13, false);
        assert_eq!(still.step(50., true, true, true), Look { lid: OPEN, glance: 0 });
    }

    #[test]
    fn breathing_is_a_couple_of_pixels_and_smooth() {
        for host in 0..2 {
            let mut last = breath(0., host);
            let mut t = 0.;
            let mut high = 0f32;
            while t < 20. {
                let b = breath(t, host);
                assert!((b - last).abs() < 0.06, "a jump in the breath");
                high = high.max(b);
                last = b;
                t += 1. / 30.;
            }
            assert!(high > 1.8 && high <= 2.0);
        }
    }

    #[test]
    fn every_cat_frame_is_named_once_and_every_clip_ends_asleep() {
        for (i, name) in CAT_FRAMES.iter().enumerate() {
            assert_eq!(CAT_FRAMES.iter().position(|n| n == name), Some(i), "{name} twice");
        }
        for clip in 0..CLIPS.len() {
            let length = clip_length(clip);
            assert!(length > 0.5 && length < 12., "{} is {length} s", CLIPS[clip].0);
            assert_eq!(clip_frame(clip, length + 0.01), BREATHING);
        }
    }

    #[test]
    fn the_cat_gets_up_within_forty_seconds_then_every_minute_or_two() {
        let mut cat = Cat::new(5, 0.);
        let mut t = 0.;
        let mut starts = Vec::new();
        let mut was = None;
        while t < 1800. {
            cat.step(t, true, false);
            if cat.playing().is_some() && was.is_none() {
                starts.push(t);
            }
            was = cat.playing();
            t += 1. / 30.;
        }
        assert!(starts[0] >= 20. && starts[0] <= 40., "first antic at {}", starts[0]);
        for w in starts.windows(2) {
            assert!(w[1] - w[0] >= 45. && w[1] - w[0] <= 135., "gap {}", w[1] - w[0]);
        }
        assert!(starts.len() >= 14, "{} antics in half an hour", starts.len());
    }

    #[test]
    fn a_click_perks_the_cat_up_without_piling_on() {
        let mut cat = Cat::new(9, 0.);
        cat.step(1., true, false);
        cat.poke(1.);
        assert_eq!(cat.playing(), Some(PERK_CLIP));
        let pose = cat.step(1.1, true, false);
        assert_eq!(CAT_FRAMES[pose.frame], "perk");
        assert!(!pose.asleep);
        cat.poke(1.5);
        cat.step(1.6, true, false);
        assert_eq!(cat.playing(), Some(PERK_CLIP));
        // Reduced motion: asleep, still, no routine.
        let pose = cat.step(2., true, true);
        assert_eq!(cat.playing(), None);
        assert_eq!(CAT_FRAMES[pose.frame], "sleep-0");
    }

    #[test]
    fn the_sleeping_cat_breathes_through_its_frames() {
        let mut seen = std::collections::BTreeSet::new();
        let mut t = 0.;
        while t < 4. {
            seen.insert(sleeping_frame(t));
            t += 1. / 30.;
        }
        assert_eq!(seen.len(), SLEEP_FRAMES);
    }

    #[test]
    fn rain_stays_on_the_glass_keeps_its_count_and_never_pops() {
        let mut rain = Rain::new(1);
        let mut t = 0.;
        for frame in 0..3000 {
            let energy = if (frame / 15) % 2 == 0 { 1. } else { 0. };
            let before: Vec<(f32, f32, f32)> = rain.drops.iter().map(|d| (d.x, d.y, d.len)).collect();
            rain.step(1. / 30., t, energy);
            t += 1. / 30.;
            for (d, (x, y, len)) in rain.drops.iter().zip(before) {
                // A drop either moves on smoothly, or leaves out of sight
                // (below the sill) and comes back out of sight (above the top).
                let moved = d.y - y;
                if moved < 0. {
                    assert!(y + 45. - len > 606. && d.y + 0.1 < 0., "a drop popped at {y} -> {}", d.y);
                } else {
                    assert!(moved < 45. && (d.x - x).abs() < 20.);
                }
            }
            for b in &rain.beads {
                if b.state > 0 {
                    assert!(on_glass(b.x, b.y), "a bead off the glass at {} {}", b.x, b.y);
                }
            }
        }
        assert_eq!(rain.drops.len(), DROPS);
        // The bass only leans on the rain slowly.
        let mut rain = Rain::new(2);
        rain.step(1. / 30., 0., 1.);
        assert!(rain.weight < 0.1);
        assert!(on_glass(470., 300.) && on_glass(800., 10.));
        assert!(!on_glass(700., 300.), "the mullion is not glass");
        assert!(!on_glass(1115., 300.) && !on_glass(460., 300.) && !on_glass(800., 610.));
    }

    #[test]
    fn lightning_flashes_twice_and_fades() {
        let mut rain = Rain::new(3);
        rain.strike(10.);
        assert!(rain.flash(10.01) > 0.8);
        assert!(rain.flash(10.12) < 0.3);
        assert!(rain.flash(10.18) > 0.6);
        assert_eq!(rain.flash(12.5), 0.);
        rain.storm(11., false);
        assert!(rain.flash(9.) == 0.);
    }

    #[test]
    fn city_windows_twinkle_and_only_a_few_go_dark() {
        let mut lights = Lights::new(4, 20);
        let mut t = 0.;
        let mut darkest = 0;
        let mut switched = false;
        while t < 600. {
            lights.step(1. / 30., t);
            let dark = lights.windows[..20].iter().filter(|w| w.on < 0.5).count();
            darkest = darkest.max(dark);
            switched |= dark > 0;
            for i in 0..20 {
                let glow = lights.glow(i, t, 0.);
                assert!((0. ..=0.35).contains(&glow));
            }
            let lamp = lights.lamp(t);
            assert!((0.8..1.1).contains(&lamp), "{lamp}");
            t += 1. / 30.;
        }
        assert!(switched && darkest <= 3, "{darkest}");
    }

    #[test]
    fn the_sign_flickers_on_and_goes_dark_off_air() {
        let mut sign = Sign::default();
        assert_eq!(sign.step(0., false, false), 0.);
        let levels: Vec<f32> = (0..60).map(|i| sign.step(1. + i as f64 / 30., true, false)).collect();
        assert!(levels.iter().any(|l| *l < 0.3) && levels.iter().any(|l| *l > 0.8), "no flicker");
        assert_eq!(*levels.last().unwrap(), 1.);
        let dips = levels.windows(2).filter(|w| w[1] < w[0] - 0.3).count();
        assert!(dips >= 2, "{dips} flickers");
        assert_eq!(sign.step(4., false, false), 1.);
        assert_eq!(sign.step(4.6, false, false), 0.);
        // Reduced motion: a plain fade, no flicker, no pulse.
        let mut calm = Sign::default();
        let levels: Vec<f32> = (0..20).map(|i| calm.step(i as f64 / 30., true, true)).collect();
        assert!(levels.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(calm.pulse(3., true), 1.);
    }

    #[test]
    fn steam_rises_and_fades_out() {
        let mut steam = Steam::new(8);
        for _ in 0..600 {
            steam.step(1. / 30.);
            for mug in &steam.puffs {
                for puff in mug {
                    let (_, y, r, a) = puff_at(puff);
                    assert!(y <= 0. && r >= 4. && (0. ..=0.12).contains(&a));
                }
            }
        }
    }
}
