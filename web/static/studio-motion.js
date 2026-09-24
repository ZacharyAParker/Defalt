/* What moves in the booth, and when. Plain state stepped by the clock; nothing
   here draws. The desktop booth runs the same machinery (src/ui/studio/motion.rs),
   so the two behave alike. Storage is made once; stepping allocates nothing. */
(function (root) {
  'use strict';
  const TAU = Math.PI * 2;

  /* A small repeatable random source (xorshift32). */
  class Dice {
    constructor(seed) { this.s = (seed >>> 0) || 1; }
    next() { let x = this.s; x ^= x << 13; x >>>= 0; x ^= x >>> 17; x ^= x << 5; x >>>= 0; this.s = x; return (x >>> 8) / 16777216; }
    range(lo, hi) { return lo + (hi - lo) * this.next(); }
  }
  const approach = (value, target, dt, seconds) => value + (target - value) * (1 - Math.exp(-dt / Math.max(seconds, 1e-4)));
  const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

  /* ── Mouths ── */
  const REST = 0, SLIGHT = 1, AH = 2, WIDE = 3, OO = 4, EE = 5;
  const VISEMES = ['slight', 'ah', 'wide', 'oo', 'ee'];
  const ATTACK = 0.015, RELEASE = 0.080, MIN_HOLD = 0.075, REDUCED_HOLD = 0.25, TEETH = 0.22, ROUND = 0.018;
  const openness = level => clamp((20 * Math.log10(Math.max(level, 1e-5)) + 42) / 26, 0, 1);
  function visemeFor(open, tone, current) {
    const sticky = shape => current === shape ? 0.05 : 0;
    if (open < 0.10 - (current === REST ? 0 : 0.03)) return REST;
    if (tone > TEETH - sticky(EE) * 0.6 && open < 0.75) return EE;
    if (open < 0.34 + sticky(SLIGHT)) return SLIGHT;
    if (tone < ROUND + sticky(OO) * 0.1) return OO;
    return open < 0.68 + sticky(AH) - sticky(WIDE) ? AH : WIDE;
  }
  class Lips {
    constructor() { this.level = 0; this.slow = 0; this.tone = 0; this.viseme = REST; this.held = 9; this.nodAge = 9; this.quiet = 9; }
    step(dt, rms, tone, reduced) {
      dt = clamp(dt, 0, 0.25);
      rms = Number.isFinite(rms) ? Math.max(rms, 0) : 0;
      this.level = approach(this.level, rms, dt, rms > this.level ? ATTACK : RELEASE);
      this.slow = approach(this.slow, this.level, dt, 0.6);
      const open = openness(this.level);
      if (open > 0.10) { this.tone = approach(this.tone, Number.isFinite(tone) ? tone : 0, dt, 0.04); this.quiet = 0; }
      else this.quiet += dt;
      this.held += dt; this.nodAge += dt;
      const target = reduced ? (open > 0.10 ? SLIGHT : REST) : visemeFor(open, this.tone, this.viseme);
      if (target !== this.viseme && this.held >= (reduced ? REDUCED_HOLD : MIN_HOLD)) { this.viseme = target; this.held = 0; }
      if (!reduced && open > 0.45 && this.level > this.slow * 1.8 && this.nodAge > 0.7) this.nodAge = 0;
      return this.viseme;
    }
    talking() { return this.quiet < 0.5; }
    nod() { return this.nodAge < 0.42 ? 1.6 * Math.sin(Math.PI * this.nodAge / 0.42) : 0; }
  }

  /* ── Eyes ── */
  const OPEN = 0, HALF = 1, CLOSED = 2, BLINK = 0.18, DOUBLE_GAP = 0.26;
  const lidAt = age => age < 0 ? OPEN : age < 0.05 ? HALF : age < 0.12 ? CLOSED : age < BLINK ? HALF : OPEN;
  class Eyes {
    constructor(seed, facesOther) {
      this.dice = new Dice(seed); this.next = this.dice.range(0.8, 3); this.start = -10; this.double = false;
      this.glance = 0; this.until = 0; this.nextLook = 2; this.facesOther = facesOther; this.look = {lid: OPEN, glance: 0};
    }
    length() { return this.double ? DOUBLE_GAP + BLINK : BLINK; }
    step(t, talking, otherTalking, reduced) {
      if (reduced) { this.glance = 0; this.start = -10; this.look.lid = OPEN; this.look.glance = 0; return this.look; }
      const idle = !talking && !otherTalking;
      if (this.glance !== 0 && t >= this.until) { this.glance = 0; this.nextLook = t + this.dice.range(1.5, 4); }
      if (this.glance === 0 && t >= this.nextLook) {
        const roll = this.dice.next();
        let wanted;
        if (otherTalking) wanted = this.facesOther ? (roll < 0.12 ? 2 : 0) : (roll < 0.65 ? 1 : 0);
        else if (talking) wanted = roll < 0.35 && this.facesOther ? 1 : roll < 0.5 ? 2 : 0;
        else wanted = roll < 0.3 ? 2 : roll < 0.5 ? 1 : 0;
        if (wanted !== 0) {
          this.glance = wanted; this.until = t + (otherTalking && wanted === 1 ? this.dice.range(1.5, 3.5) : this.dice.range(0.8, 2.2));
          if (this.dice.next() < 0.5 && t - this.start > this.length()) this.next = t;
        } else this.nextLook = t + this.dice.range(1.5, 4);
      }
      if (t - this.start >= this.length() && t >= this.next) {
        this.start = t; this.double = this.dice.next() < 0.18;
        this.next = t + this.length() + this.dice.range(2, 6) * (idle ? 1.4 : 1);
      }
      const age = t - this.start;
      this.look.lid = this.double && age >= DOUBLE_GAP ? lidAt(age - DOUBLE_GAP) : lidAt(age);
      this.look.glance = this.glance;
      return this.look;
    }
  }
  function breath(t, host) {
    const [period, offset] = host === 0 ? [4.1, 0] : [4.7, 0.37];
    const phase = ((t / period + offset) % 1 + 1) % 1;
    return 2 * (0.5 - 0.5 * Math.cos(TAU * phase));
  }

  /* ── The cat ── */
  const CAT_FRAMES = ['sleep-0', 'sleep-1', 'sleep-2', 'sleep-3', 'sleep-4', 'sleep-5', 'sleep-6', 'sleep-7',
    'ear-flick', 'ear-back', 'tail-up', 'tail-flick', 'tail-down', 'drowsy', 'awake', 'look', 'yawn-a', 'yawn-b', 'squint',
    'groom-paw', 'groom-lick', 'groom-wipe', 'stretch-a', 'stretch-b', 'perk'];
  const F = name => { const i = CAT_FRAMES.indexOf(name); if (i < 0) throw Error('unknown cat frame ' + name); return i; };
  const BREATHING = -1, SLEEP_ORDER = [0, 1, 2, 3, 4, 5, 6, 7].map(i => F('sleep-' + i));
  const clip = (...pairs) => { const out = []; for (let i = 0; i < pairs.length; i += 2) out.push([F(pairs[i]), pairs[i + 1]]); return out; };
  const CLIPS = [
    ['ear', clip('ear-flick', 0.12, 'sleep-0', 0.10, 'ear-flick', 0.10, 'sleep-0', 0.35, 'ear-flick', 0.09), 3],
    ['ear-back', clip('ear-back', 1.1, 'sleep-0', 0.25, 'ear-back', 0.6), 2],
    ['tail', clip('tail-up', 0.20, 'tail-flick', 0.26, 'tail-up', 0.16, 'tail-down', 0.55, 'tail-up', 0.22, 'tail-flick', 0.24, 'tail-up', 0.2), 3],
    ['wake', clip('drowsy', 0.6, 'awake', 1.3, 'yawn-a', 0.28, 'yawn-b', 1.15, 'yawn-a', 0.22, 'squint', 0.55, 'awake', 1.6, 'look', 1.2, 'awake', 0.6, 'drowsy', 0.7), 2],
    ['groom', clip('drowsy', 0.35, 'awake', 0.7, 'groom-paw', 0.35, 'groom-lick', 0.3, 'groom-paw', 0.22, 'groom-lick', 0.3, 'groom-paw', 0.22,
      'groom-lick', 0.3, 'groom-wipe', 0.5, 'groom-paw', 0.3, 'groom-wipe', 0.45, 'awake', 0.9, 'drowsy', 0.6), 2],
    ['stretch', clip('drowsy', 0.35, 'awake', 0.6, 'stretch-a', 0.45, 'stretch-b', 1.4, 'stretch-a', 0.4, 'awake', 0.9, 'squint', 0.4, 'drowsy', 0.6), 1.5],
    ['perk', clip('perk', 0.5, 'tail-up', 0.14, 'tail-flick', 0.2, 'perk', 0.25, 'look', 0.9, 'perk', 0.4, 'drowsy', 0.6), 0],
  ];
  const PERK_CLIP = 6, FIRST = [20, 40], BETWEEN = [45, 120];
  const clipLength = c => CLIPS[c][1].reduce((sum, [, hold]) => sum + hold, 0);
  function clipFrame(c, age) {
    let at = 0;
    for (const [frame, hold] of CLIPS[c][1]) { at += hold; if (age < at) return frame; }
    return BREATHING;
  }
  function sleepingFrame(t) {
    const phase = ((t / 3.8) % 1 + 1) % 1, depth = 0.5 - 0.5 * Math.cos(TAU * phase);
    return SLEEP_ORDER[Math.min(SLEEP_ORDER.length - 1, Math.round(depth * (SLEEP_ORDER.length - 1)))];
  }
  class Cat {
    constructor(seed, t) { this.dice = new Dice(seed); this.clip = null; this.start = 0; this.next = t + this.dice.range(FIRST[0], FIRST[1]); this.last = -1; this.reacted = -1e9; this.pose = {frame: 0, asleep: true}; }
    playing() { return this.clip; }
    play(c, t) { this.clip = c; this.start = t; this.last = c; }
    poke(t) { if (this.clip === null || this.clip <= 2) { this.play(PERK_CLIP, t); this.next = Math.max(this.next, t + 30); } }
    /* Both hosts at once: the cat looks up, now and then. */
    crowd(t) { if (this.clip === null && t - this.reacted > 120) { this.reacted = t; this.play(PERK_CLIP, t); } }
    choose() {
      let total = 0; CLIPS.forEach((c, i) => { if (i !== this.last) total += c[2]; });
      let roll = this.dice.next() * total;
      for (let i = 0; i < CLIPS.length; i++) {
        if (i === this.last || CLIPS[i][2] <= 0) continue;
        if (roll < CLIPS[i][2]) return i;
        roll -= CLIPS[i][2];
      }
      return 0;
    }
    step(t, enabled, reduced) {
      const pose = this.pose;
      if (!enabled || reduced) {
        this.clip = null;
        if (t + FIRST[0] > this.next) this.next = t + this.dice.range(FIRST[0], FIRST[1]);
        pose.frame = reduced ? SLEEP_ORDER[0] : sleepingFrame(t); pose.asleep = true; return pose;
      }
      if (this.clip !== null && t - this.start >= clipLength(this.clip)) { this.clip = null; this.next = t + this.dice.range(BETWEEN[0], BETWEEN[1]); }
      if (this.clip === null && t >= this.next) this.play(this.choose(), t);
      if (this.clip !== null) {
        const frame = clipFrame(this.clip, t - this.start);
        pose.frame = frame === BREATHING || frame === SLEEP_ORDER[0] ? sleepingFrame(t) : frame;
        pose.asleep = this.clip <= 2;
      } else { pose.frame = sleepingFrame(t); pose.asleep = true; }
      return pose;
    }
  }
  function zAt(k, out) {
    out[0] = 12 * k + 4 * Math.sin(TAU * (k * 1.1 + 0.15)) * k; out[1] = -40 * k;
    out[2] = 0.65 + 0.55 * k; out[3] = Math.pow(Math.sin(Math.PI * k), 0.8) * 0.8; return out;
  }

  /* ── Rain ── */
  const PANES = [[463, 0, 218, 606], [722, 0, 387, 606]], WINDOW = [455, 0, 663, 614], SILL = 606;
  const onGlass = (x, y) => PANES.some(p => x >= p[0] && x < p[0] + p[2] && y >= p[1] && y < p[1] + p[3]);
  // count, speed, length, opacity, width, lean
  const LAYERS = [[90, [380, 470], [9, 15], [0.2, 0.32], 0.8, 0.7], [48, [560, 700], [17, 26], [0.3, 0.44], 1.1, 1.0], [16, [820, 1000], [30, 44], [0.26, 0.38], 1.6, 1.35]];
  const BEADS = 12, SPLASHES = 8, SPLASH_LIFE = 0.3;
  class Rain {
    constructor(seed) {
      this.dice = new Dice(seed); this.drops = []; this.beads = []; this.splashes = [];
      this.wind = 0.12; this.gust = 0; this.gustAt = 6; this.weight = 0; this.flashAt = -100; this.nextFlash = 0;
      LAYERS.forEach((spec, layer) => { for (let i = 0; i < spec[0]; i++) { const d = this.fresh(layer, {}); d.y = this.dice.range(-40, SILL); this.drops.push(d); } });
      for (let b = 0; b < BEADS; b++) { const bead = this.bead({}); bead.delay = this.dice.range(0, 5); this.beads.push(bead); }
      for (let s = 0; s < SPLASHES; s++) this.splashes.push({x: 0, age: SPLASH_LIFE, size: 1});
      this.nextFlash = this.dice.range(35, 110);
    }
    fresh(layer, d) {
      const [, speed, len, alpha] = LAYERS[layer];
      d.len = this.dice.range(len[0], len[1]);
      d.x = this.dice.range(WINDOW[0] - 90, WINDOW[0] + WINDOW[2]);
      d.y = -d.len - this.dice.range(0, 60);
      d.speed = this.dice.range(speed[0], speed[1]);
      d.alpha = this.dice.range(alpha[0], alpha[1]); d.layer = layer;
      return d;
    }
    bead(b) {
      const pane = this.dice.next() < 0.36 ? PANES[0] : PANES[1];
      const r = this.dice.range(1.2, 2.6), y = this.dice.range(20, pane[3] - 60);
      b.x = this.dice.range(pane[0] + 6, pane[0] + pane[2] - 6); b.y = y; b.r = 0; b.grow = r; b.top = y; b.speed = 0; b.age = 0;
      b.delay = this.dice.range(0.5, 4); b.state = 0; b.run = this.dice.range(80, 260);
      return b;
    }
    lean(layer) { return this.wind * LAYERS[layer][5]; }
    step(dt, t, energy) {
      dt = clamp(dt, 0, 0.1);
      this.weight = approach(this.weight, clamp(energy, 0, 1), dt, 0.5);
      if (t >= this.gustAt) { this.gust = this.dice.range(0.08, 0.2); this.gustAt = t + this.dice.range(8, 20); }
      this.gust = approach(this.gust, 0, dt, 2.5);
      const tf = t % 10000, wander = 0.10 + 0.05 * Math.sin(tf * 0.23) + 0.03 * Math.sin(tf * 0.61 + 1.3);
      this.wind = approach(this.wind, wander + this.gust, dt, 0.8);
      for (const d of this.drops) {
        const fall = d.speed * (1 + 0.12 * this.weight) * dt;
        d.y += fall; d.x += fall * this.lean(d.layer);
        if (d.y - d.len > SILL) {
          if (d.layer > 0 && this.dice.next() < 0.3 && onGlass(d.x, SILL - 2)) {
            const s = this.splashes.find(s => s.age >= SPLASH_LIFE);
            if (s) { s.x = d.x; s.age = 0; s.size = d.layer === 2 ? 1.4 : 1; }
          }
          this.fresh(d.layer, d);
        }
      }
      for (const s of this.splashes) s.age = Math.min(SPLASH_LIFE, s.age + dt);
      for (const b of this.beads) {
        b.age += dt;
        if (b.state === 0 && b.age >= b.delay) { b.state = 1; b.age = 0; }
        else if (b.state === 1) {
          b.r = b.grow * Math.sqrt(Math.min(1, b.age / 2.5));
          if (b.age > 2.5 + b.grow) { b.state = 2; b.age = 0; b.top = b.y; }
        } else if (b.state === 2) {
          b.speed = Math.min(150, b.speed + dt * 160); b.y += b.speed * dt;
          b.x += Math.sin(b.age * 9) * 4 * dt; b.r = Math.max(0.8, b.r - dt * 0.35);
          if (b.y > SILL - 2 || b.y - b.top > b.run) this.bead(b);
        }
      }
    }
    flash(t) {
      const age = t - this.flashAt;
      if (!(age >= 0 && age < 2)) return 0;
      const pulse = (at, fade, peak) => age >= at ? peak * Math.exp(-(age - at) / fade) : 0;
      return Math.min(1, pulse(0, 0.07, 1) + pulse(0.17, 0.11, 0.75) + pulse(0.42, 0.35, 0.22));
    }
    storm(t, allowed) { if (!allowed) { this.nextFlash = Math.max(this.nextFlash, t + 20); return; } if (t >= this.nextFlash) this.strike(t); }
    strike(t) { this.flashAt = t; this.nextFlash = t + this.dice.range(35, 110); }
  }

  /* ── The city, the lamp and the mugs ── */
  const MAX_WINDOWS = 64;
  class Lights {
    constructor(seed, count) {
      this.dice = new Dice(seed); this.windows = [];
      for (let i = 0; i < MAX_WINDOWS; i++) this.windows.push({on: 1, target: 1, phase: this.dice.range(0, TAU), rate: this.dice.range(0.2, 0.7), screen: this.dice.next() < 0.12});
      this.count = Math.min(count, MAX_WINDOWS); this.next = this.dice.range(5, 12); this.dipAt = this.dice.range(12, 30); this.dip = 0;
    }
    step(dt, t) {
      if (this.count > 0 && t >= this.next) {
        this.next = t + this.dice.range(5, 16);
        const pick = Math.min(this.count - 1, Math.floor(this.dice.next() * this.count));
        let dark = 0; for (let i = 0; i < this.count; i++) if (this.windows[i].target < 0.5) dark++;
        const w = this.windows[pick]; w.target = w.target > 0.5 && dark < 3 ? 0 : 1;
      }
      for (let i = 0; i < this.count; i++) {
        const w = this.windows[i], step = dt / 0.3;
        w.on = w.on < w.target ? Math.min(w.target, w.on + step) : Math.max(w.target, w.on - step);
      }
      if (t >= this.dipAt) { this.dip = 1; this.dipAt = t + this.dice.range(15, 40); }
      this.dip = Math.max(0, this.dip - dt / 0.18);
    }
    glow(i, t, energy) {
      const w = this.windows[i], tf = t % 10000;
      const twinkle = w.screen ? 0.12 + 0.1 * Math.sin(tf * 7.1 + w.phase) * Math.sin(tf * 2.3 + w.phase) + 0.06 * Math.sin(tf * 13.7)
        : 0.08 + 0.07 * Math.sin(tf * w.rate + w.phase);
      return Math.max(0, twinkle + 0.12 * energy) * w.on;
    }
    lamp(t) { const tf = t % 10000; return 1 + 0.035 * Math.sin(tf * 7.3) + 0.02 * Math.sin(tf * 13.1 + 1) - 0.14 * this.dip * Math.sin(Math.PI * this.dip); }
  }
  const PUFFS = 7;
  class Steam {
    constructor(seed) {
      this.dice = new Dice(seed); this.puffs = [[], []];
      for (const mug of this.puffs) for (let i = 0; i < PUFFS; i++) { const p = this.puff({}); p.age = p.life * i / PUFFS; mug.push(p); }
    }
    puff(p) { p.age = 0; p.life = this.dice.range(3, 4.4); p.drift = this.dice.range(-6, 10); p.phase = this.dice.range(0, 1); p.spread = this.dice.range(-7, 7); return p; }
    step(dt) { for (const mug of this.puffs) for (const p of mug) { p.age += clamp(dt, 0, 0.1); if (p.age >= p.life) this.puff(p); } }
  }
  function puffAt(p, out) {
    const k = clamp(p.age / p.life, 0, 1), rise = 1 - (1 - k) * (1 - k);
    out[0] = p.spread * 0.4 + p.drift * k + 6 * k * Math.sin(TAU * (k * 0.9 + p.phase)); out[1] = -70 * rise;
    out[2] = 4 + 12 * k; out[3] = 0.11 * Math.pow(Math.sin(Math.PI * k), 1.2); return out;
  }

  /* ── The sign ── */
  const IGNITION = [[0, 0], [0.08, 0.85], [0.13, 0.05], [0.3, 0], [0.38, 0.7], [0.43, 0.2], [0.55, 0.95], [0.6, 0.45], [0.72, 1], [1.05, 0.8], [1.12, 1], [1.6, 1]];
  const IGNITION_LENGTH = 1.6;
  class Sign {
    constructor() { this.on = false; this.since = -100; this.from = 0; this.level = 0; }
    step(t, onAir, reduced) {
      if (onAir !== this.on) { this.on = onAir; this.since = t; this.from = this.level; }
      const age = t - this.since;
      if (this.on) {
        if (reduced) this.level = Math.min(1, this.from + age / 0.3);
        else if (age < IGNITION_LENGTH && this.from < 0.5) { let level = 0; for (const [at, l] of IGNITION) if (age >= at) level = l; this.level = level; }
        else this.level = 1;
      } else if (reduced) this.level = Math.max(0, this.from - age / 0.3);
      else { const fade = Math.max(0, this.from * (1 - age / 0.35)); this.level = age >= 0.1 && age < 0.16 ? fade * 0.3 : fade; }
      return this.level;
    }
    pulse(t, reduced) { if (reduced) return 1; const tf = t % 10000; return 1 + 0.06 * Math.sin(TAU * 0.3 * tf) + 0.015 * Math.sin(TAU * 7.7 * tf); }
  }

  const api = {Dice, Lips, Eyes, Cat, Rain, Lights, Steam, Sign, openness, visemeFor, lidAt, breath, sleepingFrame, clipLength, clipFrame,
    zAt, puffAt, onGlass, REST, SLIGHT, AH, WIDE, OO, EE, VISEMES, MIN_HOLD, OPEN, HALF, CLOSED, CAT_FRAMES, CLIPS, PERK_CLIP, BREATHING,
    FIRST, BETWEEN, PANES, WINDOW, LAYERS, SPLASH_LIFE, IGNITION_LENGTH};
  root.StudioMotion = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
