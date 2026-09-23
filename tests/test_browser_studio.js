// The browser studio: optimized sprites that match the scene layout, partial
// loading, bitmap sizing, region repaints, pacing and the music-driven room.
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

function scene({fail = () => false, running = false} = {}) {
  let now = 0, resize = null;
  const requested = [];
  const main = recorder();
  const canvas = {width: 1728, height: 1152, dataset: {}, getContext: () => main};
  const elements = {
    'studio-scene': canvas, 'studio-status': element(), 'cat-life': element(),
    reduced: element(), 'rain-enabled': element(), 'lights-enabled': element(), 'cat-enabled': element(),
  };
  elements['cat-life'].dataset.pose = 'sleep';
  for (const id of ['rain-enabled', 'lights-enabled', 'cat-enabled']) elements[id].checked = true;
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
    ResizeObserver: class { constructor(fn) { resize = fn; } observe() {} },
    document: {
      hidden: false, currentScript: {src: 'http://127.0.0.1:8090/static/studio-scene.js?v=9.9.9'},
      getElementById: (id) => elements[id],
      createElement: () => ({width: 0, height: 0, getContext: () => recorder()}),
    },
  });
  vm.runInContext(read('web/static/studio-scene.js'), context);
  const settle = () => new Promise((resolve) => setImmediate(resolve));
  const frame = (ms, activity = {mav: false, rue: false}, energy = 0) => {
    main.calls.length = 0; now += ms;
    window.StudioScene.update(ms / 1000, activity, energy, running);
    return [...main.calls];
  };
  return {window, canvas, elements, requested, settle, frame, main,
    resize: (width) => resize([{contentRect: {width}}]), setRunning: (value) => { running = value; }};
}

(async () => {
  // ── The optimized art exists, is small, and matches the scene's layout ──
  const dir = path.join(root, 'web/static/studio-v2/web');
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'sprites.json'), 'utf8'));
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
  for (const host of layout.hosts) {
    assert.deepEqual(manifest[`${host.name}-mouth`].rect, host.mouth, `${host.name} mouth crop matches the scene`);
    host.eyes.forEach((eye, i) => assert.deepEqual(manifest[`${host.name}-eye-${i}`].rect, eye));
    for (const [sprite, rect] of [[host.name, host.at], [`${host.name}-phones`, host.phones]]) {
      assert.deepEqual(manifest[sprite].size, rect.slice(2), `${sprite} is drawn at its own size`);
    }
    const [w, h] = manifest[`${host.name}-mouth`].size;
    assert.ok(w >= host.mouth[2] && h >= host.mouth[3], 'face patches are at least scene-size (about 2x drawn)');
  }
  for (const pose of ['sleep', 'awake', 'yawn', 'groom']) {
    assert.deepEqual(manifest[`cat-${pose}`].rect, layout.cat);
    assert.deepEqual(manifest[`cat-${pose}`].size, [layout.cat[2] * 2, layout.cat[3] * 2]);
  }

  // ── Versioned URLs; one missing layer does not blank the studio ──
  const s = scene({fail: (name) => name === 'black-mug'});
  await s.settle();
  assert.equal(s.requested.length, layout.sprites.length);
  assert.ok(s.requested.every((url) => /^\/static\/studio-v2\/web\/[\w-]+\.webp\?v=9\.9\.9$/.test(url)), s.requested[0]);
  assert.equal(s.canvas.dataset.ready, 'true', 'drawn with the layers that loaded');
  assert.equal(s.elements['studio-status'].textContent, '');

  // ── The bitmap follows the element instead of the art's full size ──
  s.main.calls.length = 0;
  s.resize(600);
  assert.equal(s.canvas.width, 600); assert.equal(s.canvas.height, 400);
  const scaled = s.main.calls.find((call) => call[0] === 'setTransform');
  assert.ok(Math.abs(scaled[1] - 600 / 1728) < 1e-9, 'drawn in scene space, scaled to the bitmap');

  // ── Reduced motion: only what changes is repainted ──
  s.elements.reduced.checked = true;
  s.frame(200);                                                  // settles after the toggle
  assert.equal(s.frame(200).length, 0, 'an unchanged studio draws nothing');
  const talk = s.frame(10, {mav: true, rue: false});
  const regions = talk.filter((call) => call[0] === 'rect');
  assert.equal(regions.length, 1, 'one region repainted when Mav starts talking');
  const [, x, y, w, h] = regions[0];
  assert.ok(x <= 585 && y <= 463 && x + w >= 674 && y + h >= 514, 'the region covers his mouth');
  assert.ok(w * h < 1728 * 1152 * 0.5, 'and is not the whole scene');
  assert.ok(!talk.some((call) => call[0] === 'stroke'), 'no rain with reduced motion');

  // ── Pacing, rain that holds still off air, and a room that hears the bass ──
  s.elements.reduced.checked = false;
  s.frame(200);
  const rainAt = (calls) => JSON.stringify([...new Set(calls.filter((call) => call[0] === 'moveTo').map(String))]);
  const stopped = [0, 1, 2, 3, 4, 5].map(() => rainAt(s.frame(200))).filter((rain) => rain !== '[]');
  assert.ok(stopped.length >= 2, 'the city lights keep the window repainting');
  assert.ok(stopped.every((rain) => rain === stopped[0]), 'rain holds still while the station is off');
  assert.equal(s.frame(50).length, 0, 'about 8 fps while stopped');
  s.setRunning(true);
  s.frame(200);
  const moving1 = rainAt(s.frame(100)), moving2 = rainAt(s.frame(100));
  assert.notEqual(moving1, moving2, 'rain falls while on air');
  assert.equal(s.frame(30).length, 0, 'about 15 fps on air');
  const quiet = s.frame(100, undefined, 0), loud = s.frame(100, undefined, 1);
  const alpha = (calls) => Number(/,([\d.]+)\)$/.exec(calls.find((call) => call[0] === '=strokeStyle')[1])[1]);
  const drops = (calls) => calls.filter((call) => call[0] === 'stroke').length;
  assert.ok(alpha(loud) > alpha(quiet), `rain is a little heavier with the bass (${alpha(quiet)} -> ${alpha(loud)})`);
  assert.ok(drops(loud) > drops(quiet));
  assert.ok(alpha(loud) <= 0.3, 'but only a little');

  // ── Nothing loads: say so, never throw ──
  const none = scene({fail: () => true});
  await none.settle();
  assert.match(none.elements['studio-status'].textContent, /Studio artwork unavailable/);
  assert.equal(none.window.StudioScene.ready, false);
  none.frame(200);
  console.log('Browser studio: sprites match the layout, partial loading, bitmap sizing, region repaints, pacing and music response passed.');
})().catch((error) => { console.error(error); process.exitCode = 1; });
