// The browser booth's motion: mouth shapes and their hold, blinks and
// glances, the cat's routine and first antic, rain that stays on the glass
// and never pops, the sign's flicker, and the reduced-motion paths. The same
// rules the desktop booth's motion.rs is tested against.
const assert = require('node:assert/strict');
const M = require('../web/static/studio-motion.js');

function speak(lips, seconds, rms, tone, reduced = false) {
  const out = [];
  for (let i = 0; i < seconds * 1000; i++) out.push(lips.step(0.001, rms, tone, reduced));
  return out;
}

// ── Dice are repeatable ──
const a = new M.Dice(7), b = new M.Dice(7);
for (let i = 0; i < 1000; i++) { const x = a.next(); assert.equal(x, b.next()); assert.ok(x >= 0 && x < 1); }

// ── Mouth shapes from loudness and tone, sticky at the edges ──
assert.equal(M.visemeFor(0.02, 0.05, M.REST), M.REST);
assert.equal(M.visemeFor(0.25, 0.05, M.REST), M.SLIGHT);
assert.equal(M.visemeFor(0.5, 0.05, M.REST), M.AH);
assert.equal(M.visemeFor(0.9, 0.05, M.REST), M.WIDE);
assert.equal(M.visemeFor(0.5, 0.005, M.REST), M.OO);
assert.equal(M.visemeFor(0.4, 0.4, M.REST), M.EE);
assert.equal(M.visemeFor(0.70, 0.05, M.AH), M.AH);
assert.equal(M.visemeFor(0.70, 0.05, M.REST), M.WIDE);

// ── Opens fast, closes at a pause ──
let lips = new M.Lips();
const opening = speak(lips, 0.06, 0.12, 0.05);
const opened = opening.findIndex((v) => v !== M.REST);
assert.ok(opened >= 0 && opened < 30, `opens within 30 ms (${opened})`);
speak(lips, 0.3, 0.12, 0.05);
assert.ok([M.AH, M.WIDE].includes(lips.viseme));
const pause = speak(lips, 0.4, 0, 0);
assert.equal(pause[pause.length - 1], M.REST, 'the mouth closes at a pause');

// ── Shapes hold long enough to read ──
lips = new M.Lips();
const shapes = [];
for (let i = 0; i < 2000; i++) shapes.push(lips.step(0.001, (i / 20 | 0) % 2 ? 0.004 : 0.2, (i / 55 | 0) % 3 ? 0.03 : 0.4, false));
let run = 0, shortest = Infinity;
for (let i = 1; i < shapes.length; i++) { run++; if (shapes[i] !== shapes[i - 1]) { shortest = Math.min(shortest, run); run = 0; } }
assert.ok(shortest >= M.MIN_HOLD * 1000 - 1, `a shape lasted only ${shortest} ms`);

// ── Reduced motion: open or closed, never a flicker, no nod ──
lips = new M.Lips();
const calm = [];
for (let i = 0; i < 3000; i++) calm.push(lips.step(0.001, (i / 30 | 0) % 2 ? 0 : 0.3, 0.5, true));
assert.ok(calm.every((v) => v === M.REST || v === M.SLIGHT));
assert.ok(calm.filter((v, i) => i && v !== calm[i - 1]).length <= 13);
assert.equal(lips.nod(), 0);

// ── Blinks every two to six seconds, some double, half-closed on the way ──
const eyes = new M.Eyes(3, false);
const starts = [];
let last = M.OPEN;
for (let t = 0; t < 600; t += 0.01) {
  const look = eyes.step(t, true, false, false);
  if (look.lid !== M.OPEN && last === M.OPEN) starts.push(t);
  last = look.lid;
}
const gaps = starts.slice(1).map((t, i) => t - starts[i]);
assert.ok(gaps.filter((g) => g < 0.5).length > 3, 'double blinks happen');
const singles = gaps.filter((g) => g > 0.5), mean = singles.reduce((s, g) => s + g, 0) / singles.length;
assert.ok(mean > 2.5 && mean < 5.5, `mean gap ${mean}`);
assert.equal(M.lidAt(0.02), M.HALF); assert.equal(M.lidAt(0.08), M.CLOSED); assert.equal(M.lidAt(0.3), M.OPEN);

// ── Rue looks over at Mav while he talks; Mav already faces her ──
let toward = 0;
const rue = new M.Eyes(11, false);
for (let t = 0; t < 120; t += 0.05) if (rue.step(t, false, true, false).glance === 1) toward++;
assert.ok(toward > 600, `Rue looked over for ${toward} of 2400 frames`);
const mav = new M.Eyes(12, true);
for (let t = 0; t < 120; t += 0.05) assert.notEqual(mav.step(t, false, true, false).glance, 1);
assert.deepEqual({...new M.Eyes(13, false).step(50, true, true, true)}, {lid: M.OPEN, glance: 0});

// ── The cat: up within 20-40 s, then every 45-120 s, every clip ends asleep ──
for (let c = 0; c < M.CLIPS.length; c++) assert.equal(M.clipFrame(c, M.clipLength(c) + 0.01), M.BREATHING);
const cat = new M.Cat(5, 0);
const antics = [];
let was = null;
for (let t = 0; t < 1800; t += 1 / 30) {
  cat.step(t, true, false);
  if (cat.playing() !== null && was === null) antics.push(t);
  was = cat.playing();
}
assert.ok(antics[0] >= 20 && antics[0] <= 40, `first antic at ${antics[0]}`);
for (let i = 1; i < antics.length; i++) { const g = antics[i] - antics[i - 1]; assert.ok(g >= 45 && g <= 135, `gap ${g}`); }
const poked = new M.Cat(9, 0);
poked.step(1, true, false); poked.poke(1);
assert.equal(poked.playing(), M.PERK_CLIP);
assert.equal(M.CAT_FRAMES[poked.step(1.1, true, false).frame], 'perk');
assert.equal(M.CAT_FRAMES[poked.step(2, true, true).frame], 'sleep-0', 'reduced motion: asleep and still');
assert.equal(poked.playing(), null);
const seen = new Set();
for (let t = 0; t < 4; t += 1 / 30) seen.add(M.sleepingFrame(t));
assert.equal(seen.size, 8, 'the sleeping cat breathes through all its frames');

// ── Rain: on the glass, a fixed count, never popping ──
const rain = new M.Rain(1);
const count = rain.drops.length;
for (let frame = 0, t = 0; frame < 3000; frame++, t += 1 / 30) {
  const before = rain.drops.map((d) => [d.x, d.y, d.len]);
  rain.step(1 / 30, t, (frame / 15 | 0) % 2 ? 0 : 1);
  rain.drops.forEach((d, i) => {
    // A drop leaves only once all of it has gone under the sill (at most one
    // frame's fall past where it was), and comes back above the top.
    const [x, y, len] = before[i], moved = d.y - y;
    if (moved < 0) assert.ok(y + 45 - len > 606 && d.y + 0.1 < 0, `a drop popped at ${y} -> ${d.y}`);
    else assert.ok(moved < 45 && Math.abs(d.x - x) < 20);
  });
  for (const bead of rain.beads) if (bead.state > 0) assert.ok(M.onGlass(bead.x, bead.y), 'a bead off the glass');
}
assert.equal(rain.drops.length, count);
assert.ok(!M.onGlass(700, 300), 'the mullion is not glass');
assert.ok(!M.onGlass(1115, 300) && !M.onGlass(460, 300) && !M.onGlass(800, 610));
rain.strike(10);
assert.ok(rain.flash(10.01) > 0.8 && rain.flash(10.12) < 0.3 && rain.flash(10.18) > 0.6 && rain.flash(12.5) === 0);

// ── City windows twinkle; only a few go dark at once ──
const lights = new M.Lights(4, 20);
let darkest = 0;
for (let t = 0; t < 600; t += 1 / 30) {
  lights.step(1 / 30, t);
  darkest = Math.max(darkest, lights.windows.slice(0, 20).filter((w) => w.on < 0.5).length);
  const lamp = lights.lamp(t); assert.ok(lamp > 0.8 && lamp < 1.1);
}
assert.ok(darkest > 0 && darkest <= 3);

// ── The sign flickers on, goes dark off air; reduced motion just fades ──
const sign = new M.Sign();
assert.equal(sign.step(0, false, false), 0);
const levels = [];
for (let i = 0; i < 60; i++) levels.push(sign.step(1 + i / 30, true, false));
assert.ok(levels.some((l) => l < 0.3) && levels.some((l) => l > 0.8));
assert.ok(levels.filter((l, i) => i && l < levels[i - 1] - 0.3).length >= 2, 'it flickers');
assert.equal(levels[levels.length - 1], 1);
assert.equal(sign.step(4, false, false), 1); assert.equal(sign.step(4.6, false, false), 0);
const gentle = new M.Sign(), fade = [];
for (let i = 0; i < 20; i++) fade.push(gentle.step(i / 30, true, true));
assert.ok(fade.every((l, i) => !i || l >= fade[i - 1]));
assert.equal(gentle.pulse(3, true), 1);

// ── Steam rises and fades ──
const steam = new M.Steam(8), out = [0, 0, 0, 0];
for (let i = 0; i < 600; i++) {
  steam.step(1 / 30);
  for (const mug of steam.puffs) for (const p of mug) { M.puffAt(p, out); assert.ok(out[1] <= 0 && out[2] >= 4 && out[3] >= 0 && out[3] <= 0.12); }
}
console.log('Browser studio motion: visemes and hold, blinks and glances, cat timing, rain bounds, lights, sign and reduced motion passed.');
