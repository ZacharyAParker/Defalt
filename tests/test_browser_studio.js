// The browser studio: optimized sprites and one frame atlas that match the
// scene layout, partial loading, bitmap sizing, region repaints, pacing, rain
// that keeps falling off air, lip sync from the voice, and the music-driven room.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const {root, read, element} = require('./browser_harness');

/* Width and height of a .webp, from its header. */
function webpSize(file) {
  const b = fs.readFileSync(file);
  assert.equal(b.toString('ascii', 0, 4), 'RIFF'); assert.equal(b.toString('ascii', 8, 12), 'WEBP');
  const chunk = b.toString('ascii', 12, 16);
  if (chunk === 'VP8X') return [1 + b.readUIntLE(24, 3), 1 + b.readUIntLE(27, 3)];
  if (chunk === 'VP8L') { const bits = b.readUInt32LE(21); return [1 + (bits & 0x3fff), 1 + ((bits >> 14) & 0x3fff)]; }
  if (chunk === 'VP8 ') return [b.readUInt16LE(26) & 0x3fff, b.readUInt16LE(28) & 0x3fff];
  throw new Error(`${file}: unknown WebP chunk ${chunk}`);
}

function recorder() {
  const calls = [];
  const ctx = new Proxy({calls}, {
    get(target, name) {
      if (name in target) return target[name];
      return (...args) => { calls.push([name, ...args]); };
    },
    set(target, name, value) { target[name] = value; calls.push(['=' + String(name), value]); return true; },
  });
  return ctx;
}

const dir = path.join(root, 'web/static/studio-v2/web');
const framesJson = fs.readFileSync(path.join(dir, 'frames.json'), 'utf8');

function scene({fail = () => false, running = false, noFrames = false} = {}) {
  let now = 0, resize = null;
  const requested = [];
  const main = recorder();
  const canvas = {width: 1728, height: 1152, dataset: {}, getContext: () => main};
  const elements = {
    'studio-scene': canvas, 'studio-status': element(), 'cat-life': element(),
    reduced: element(), 'rain-enabled': element(), 'lights-enabled': element(), 'cat-enabled': element(),
    'lightning-enabled': element(),
  };
  for (const id of ['rain-enabled', 'lights-enabled', 'cat-enabled', 'lightning-enabled']) elements[id].checked = true;
  class Image {
    set src(url) {
      requested.push(url);
      this.url = url;
      const name = /web\/([\w-]+)\.webp/.exec(url)[1];
      queueMicrotask(() => (fail(name) ? this.onerror() : this.onload()));
    }
  }
  const window = {devicePixelRatio: 1, addEventListener() {}};
  const context = vm.createContext({
    window, Image, Promise, console: {warn() {}, error() {}},
    performance: {now: () => now},
    fetch: async (url) => { requested.push(url); return noFrames ? {ok: false} : {ok: true, json: async () => JSON.parse(framesJson)}; },
    ResizeObserver: class { constructor(fn) { resize = fn; } observe() {} },
    document: {
      hidden: false, currentScript: {src: 'http://127.0.0.1:8090/static/studio-scene.js?v=9.9.9'},
      getElementById: (id) => elements[id],
      createElement: () => ({width: 0, height: 0, getContext: () => recorder()}),
    },
  });
  vm.runInContext(read('web/static/studio-motion.js'), context);
  vm.runInContext(read('web/static/studio-scene.js'), context);
  const settle = () => new Promise((resolve) => setImmediate(resolve));
  const quiet = {levels: [0, 0], tones: [0, 0]};
  const frame = (ms, voice = quiet, energy = 0) => {
    main.calls.length = 0; now += ms;
    window.StudioScene.update(ms / 1000, {mav: false, rue: false}, energy, running, voice);
    return [...main.calls];
  };
  return {window, canvas, elements, requested, settle, frame, main,
    resize: (width) => resize([{contentRect: {width}}]), setRunning: (value) => { running = value; }};
}

(async () => {
  // ── The optimized art exists, is small, and matches the scene's layout ──
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'sprites.json'), 'utf8'));
  const frames = JSON.parse(framesJson);
  const probe = scene();
  const layout = JSON.parse(JSON.stringify(probe.window.StudioScene.layout));
  let total = 0;
  for (const name of layout.sprites) {
    const entry = manifest[name];
    assert.ok(entry, `sprites.json lists ${name}`);
    const file = path.join(dir, `${name}.webp`);
    assert.deepEqual(webpSize(file), entry.size, `${name}.webp is the size the manifest says`);
    total += fs.statSync(file).size;
  }
  assert.ok(total < 1.5e6, `studio art is ${(total / 1e6).toFixed(2)} MB`);
  assert.deepEqual(frames.atlas, manifest.atlas.size, 'frames.json is about this atlas');
  for (const host of layout.hosts) {
    for (const [sprite, rect] of [[host.name, host.at], [`${host.name}-phones`, host.phones]]) {
      assert.deepEqual(manifest[sprite].size, rect.slice(2), `${sprite} is drawn at its own size`);
    }
    const faces = frames.faces[host.name];
    assert.equal(faces.mouths.length, 5); assert.equal(faces.lids.length, 2); assert.equal(faces.looks.length, 2);
    for (const item of [...faces.mouths, ...faces.lids, ...faces.looks]) {
      assert.ok(item, `${host.name} has every face frame`);
      const [x, y, w, h] = item.at;
      assert.ok(x > host.at[0] && y + h < host.neck, `${host.name}'s face frame sits on the face, above the neck`);
      assert.ok(w < 200 && h < 130, 'a face frame is a patch, not a head');
    }
  }
  const [aw, ah] = frames.atlas;
  const within = (item) => item.src[0] >= 0 && item.src[1] >= 0 && item.src[0] + item.src[2] <= aw && item.src[1] + item.src[3] <= ah;
  assert.deepEqual(frames.cat.map((c) => c.name), [...probe.window.StudioMotion.CAT_FRAMES], 'the atlas has every cat frame, in order');
  assert.ok(frames.cat.every(within) && frames.lights.every((l) => within(l.lit) && within(l.dim)));

  // ── Versioned URLs; one missing layer does not blank the studio ──
  const s = scene({fail: (name) => name === 'black-mug'});
  await s.settle(); await s.settle();
  assert.equal(s.requested.length, layout.sprites.length + 1);
  assert.ok(s.requested.every((url) => /^\/static\/studio-v2\/web\/[\w-]+\.(webp|json)\?v=9\.9\.9$/.test(url)), s.requested[0]);
  assert.equal(s.canvas.dataset.ready, 'true', 'drawn with the layers that loaded');
  assert.equal(s.elements['studio-status'].textContent, '');

  // ── The bitmap follows the element instead of the art's full size ──
  s.main.calls.length = 0;
  s.resize(600);
  assert.equal(s.canvas.width, 600); assert.equal(s.canvas.height, 400);
  const scaled = s.main.calls.find((call) => call[0] === 'setTransform');
  assert.ok(Math.abs(scaled[1] - 600 / 1728) < 1e-9, 'drawn in scene space, scaled to the bitmap');
  s.resize(1728);

  // ── Reduced motion: only what changes is repainted ──
  s.elements.reduced.checked = true;
  s.frame(200);
  assert.equal(s.frame(200).length, 0, 'an unchanged studio draws nothing');
  const talk = s.frame(300, {levels: [0.2, 0], tones: [0.05, 0]});
  const regions = talk.filter((call) => call[0] === 'rect');
  assert.equal(regions.length, 1, 'one region repainted when Mav starts talking');
  const [, x, y, w, h] = regions[0];
  assert.ok(x <= 585 && y <= 463 && x + w >= 674 && y + h >= 514, 'the region covers his mouth');
  assert.ok(w * h < 1728 * 1152 * 0.5, 'and is not the whole scene');
  assert.ok(!talk.some((call) => call[0] === 'stroke'), 'no rain with reduced motion');
  assert.equal(s.canvas.dataset.speaking, 'mav');
  s.frame(400);

  // ── Rain that falls off air too, at a lower rate; about 30 fps on air ──
  s.elements.reduced.checked = false;
  s.frame(200);
  const rainAt = (calls) => JSON.stringify(calls.filter((call) => call[0] === 'moveTo').slice(0, 40));
  const off1 = rainAt(s.frame(100)), off2 = rainAt(s.frame(100));
  assert.notEqual(off1, '[]'); assert.notEqual(off1, off2, 'rain keeps falling while the station is off');
  assert.equal(s.frame(40).length, 0, 'about 12 fps while stopped');
  s.setRunning(true);
  s.frame(200);
  assert.ok(s.frame(34).length > 0, 'about 30 fps on air');
  assert.equal(s.frame(10).length, 0, 'but no faster');
  // The far layer's heads: the second stroke style the rain sets.
  const alpha = (calls) => calls.filter((call) => call[0] === '=strokeStyle' && /173,192,220/.test(call[1]))
    .map((call) => Number(/,([\d.]+)\)$/.exec(call[1])[1]))[1];
  const quiet = alpha(s.frame(100, undefined, 0));
  for (let i = 0; i < 40; i++) s.frame(50, undefined, 1);
  const loud = alpha(s.frame(100, undefined, 1));
  assert.ok(loud > quiet, `rain is a little heavier with the bass (${quiet} -> ${loud})`);
  assert.ok(loud <= 0.45, 'but only a little');
  const rain = s.window.StudioScene.life.rain, count = rain.drops.length;
  s.frame(100, undefined, 0); s.frame(100, undefined, 1);
  assert.equal(rain.drops.length, count, 'the bass never adds or removes drops');

  // ── The sign lights with the station, and is dark without it ──
  const signs = s.window.StudioScene.life.sign;
  assert.equal(signs.level, 1, 'lit on air');
  s.setRunning(false);
  for (let i = 0; i < 10; i++) s.frame(100);
  assert.equal(signs.level, 0, 'dark off air');

  // ── The cat answers the buttons ──
  s.window.StudioScene.catPoke();
  assert.equal(s.window.StudioScene.catClip(), 'perk');
  s.frame(100);
  assert.equal(s.canvas.dataset.pose, 'perk');

  // ── No frames manifest: the room still draws, nothing throws ──
  const bare = scene({noFrames: true});
  await bare.settle(); await bare.settle();
  assert.equal(bare.canvas.dataset.ready, 'true');
  bare.frame(200, {levels: [0.2, 0.2], tones: [0.05, 0.05]});

  // ── Nothing loads: say so, never throw ──
  const none = scene({fail: () => true, noFrames: true});
  await none.settle(); await none.settle();
  assert.match(none.elements['studio-status'].textContent, /Studio artwork unavailable/);
  assert.equal(none.window.StudioScene.ready, false);
  none.frame(200);
  console.log('Browser studio: sprites and atlas match the layout, partial loading, bitmap sizing, region repaints, pacing, rain, sign, cat and music response passed.');
})().catch((error) => { console.error(error); process.exitCode = 1; });
