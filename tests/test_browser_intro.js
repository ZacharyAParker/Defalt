// The startup intro: plays muted over everything, skips on a tap or key
// without the key pressing anything underneath, and follows the setting
// (every visit / once a day / off) and reduced motion.
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { read, element } = require('./browser_harness');

const html = read('web/index.html');
assert.match(html, /<script src="\/static\/legal\.js\?v=\{\{APP_VERSION\}\}" defer><\/script>\s*\n<script src="\/static\/intro\.js\?v=\{\{APP_VERSION\}\}" defer><\/script>/,
  'the intro opens after the notice, so it sits on top of it');
const markup = html.split('id="intro"')[1].split('</dialog>')[0];
assert.match(markup, /data-video="\/static\/splash\/intro\.mp4\?v=\{\{APP_VERSION\}\}"/);
assert.match(markup, /data-poster="\/static\/splash\/intro-poster\.webp\?v=\{\{APP_VERSION\}\}"/);
assert.match(markup, /<video[^>]* muted playsinline preload="none"/, 'muted, inline, and nothing fetched unless it plays');
assert.match(html, /<select id="intro-mode">/);

function page({stored = {}, reducedPreference = false, boothReduced = false, playRejects = false} = {}) {
  const ids = ['intro', 'intro-video', 'intro-skip', 'intro-mode', 'reduced'];
  const elements = Object.fromEntries(ids.map(id => [id, element()]));
  const dialog = elements.intro;
  dialog.dataset.video = '/static/splash/intro.mp4?v=1';
  dialog.dataset.poster = '/static/splash/intro-poster.webp?v=1';
  dialog.open = false;
  dialog.showModal = () => { dialog.open = true; dialog.shown = (dialog.shown || 0) + 1; };
  dialog.close = () => { dialog.open = false; };
  const video = elements['intro-video'];
  video.played = 0;
  video.play = () => { video.played++; return playRejects ? Promise.reject(new Error('no')) : Promise.resolve(); };
  video.pause = () => { video.paused = true; };
  video.removeAttribute = name => { if (name === 'src') video.src = ''; };
  elements.reduced.checked = boothReduced;

  const saved = new Map(Object.entries(stored));
  const localStorage = {
    getItem: key => saved.has(key) ? saved.get(key) : null,
    setItem: (key, value) => saved.set(key, String(value)),
  };
  const timers = [];
  const listeners = [];
  const body = new Set();
  const context = {
    document: {
      getElementById: id => elements[id],
      body: {classList: {add: c => body.add(c), remove: c => body.delete(c)}},
    },
    localStorage,
    matchMedia: () => ({matches: reducedPreference}),
    setTimeout: (fn, ms) => { timers.push({fn, ms}); return timers.length; },
    clearTimeout: id => { if (timers[id - 1]) timers[id - 1].cleared = true; },
    addEventListener: (type, fn, capture) => listeners.push({type, fn, capture}),
    removeEventListener: (type, fn) => {
      const at = listeners.findIndex(l => l.type === type && l.fn === fn);
      if (at >= 0) listeners.splice(at, 1);
    },
    Date, String, Promise,
  };
  context.globalThis = context;
  vm.runInNewContext(read('web/static/intro.js'), context);
  const run = () => { for (const timer of timers.splice(0)) if (!timer.cleared) timer.fn(); };
  const key = (type, name) => {
    let prevented = false, stopped = false;
    const event = {type, key: name, preventDefault() { prevented = true; }, stopPropagation() { stopped = true; }};
    for (const listener of [...listeners]) if (listener.type === type) listener.fn(event);
    return {prevented, stopped};
  };
  return {context, dialog, video, elements, saved, timers, run, body, key, listeners};
}

(async () => {
  // First visit: the ident, muted, over everything, and it goes when it ends.
  const first = page();
  assert.equal(first.context.DefaltIntro.started, 'full');
  assert.equal(first.dialog.open, true);
  assert.equal(first.video.muted, true);
  assert.equal(first.video.playsInline, true);
  assert.equal(first.video.src, '/static/splash/intro.mp4?v=1');
  assert.equal(first.video.poster, '/static/splash/intro-poster.webp?v=1');
  assert.equal(first.video.played, 1);
  assert.ok(first.body.has('intro-playing'));
  first.video.handlers.ended();
  assert.ok(first.dialog.classList.contains('intro--out'), 'it fades');
  first.run();
  assert.equal(first.dialog.open, false);
  assert.equal(first.video.src, '', 'the video lets go of its download');
  assert.ok(!first.body.has('intro-playing'));

  // Skipped with a tap.
  const tapped = page();
  tapped.dialog.handlers.click();
  tapped.run();
  assert.equal(tapped.dialog.open, false);

  // Skipped with Space: the key is swallowed until it's let go, so it can't
  // go on to press the first-run notice's "I agree" underneath.
  const spaced = page();
  let prevented = false, stopped = false;
  spaced.dialog.handlers.keydown({key: ' ', preventDefault() { prevented = true; }, stopPropagation() { stopped = true; }});
  assert.ok(prevented && stopped);
  spaced.run();
  assert.equal(spaced.dialog.open, false);
  assert.ok(spaced.key('keydown', ' ').prevented, 'a held key repeats into nothing');
  assert.ok(spaced.key('keyup', ' ').prevented, 'the release presses nothing');
  assert.equal(spaced.listeners.length, 0, 'and then keys are the page\'s again');
  // Escape goes through cancel: skipped, not merely closed.
  const escaped = page();
  let cancelled = false;
  escaped.dialog.handlers.cancel({preventDefault() { cancelled = true; }});
  assert.ok(cancelled);
  escaped.run();
  assert.equal(escaped.dialog.open, false);
  // Other keys stay with the intro and do nothing.
  const other = page();
  other.dialog.handlers.keydown({key: 'm', preventDefault() { throw new Error('m is not a skip'); }, stopPropagation() {}});
  assert.equal(other.dialog.open, true);

  // Once a day: the ident the first time, and it remembers the day...
  const daily = page({stored: {'defalt.intro.mode': 'daily'}});
  assert.equal(daily.context.DefaltIntro.started, 'full');
  const day = daily.saved.get('defalt.intro.day');
  assert.equal(day, daily.context.DefaltIntro.today());
  // ...then the final logo for a second on later visits that day.
  const later = page({stored: {'defalt.intro.mode': 'daily', 'defalt.intro.day': day}});
  assert.equal(later.context.DefaltIntro.started, 'still');
  assert.equal(later.dialog.open, true);
  assert.equal(later.video.played, 0);
  assert.equal(later.video.poster, '/static/splash/intro-poster.webp?v=1');
  assert.equal(later.timers[0].ms, 1000);
  later.run(); later.run();
  assert.equal(later.dialog.open, false);
  // A new day: the ident again.
  assert.equal(page({stored: {'defalt.intro.mode': 'daily', 'defalt.intro.day': '2001-01-01'}}).context.DefaltIntro.started, 'full');

  // Off: nothing at all.
  const off = page({stored: {'defalt.intro.mode': 'off'}});
  assert.equal(off.context.DefaltIntro.started, 'skip');
  assert.equal(off.dialog.shown, undefined);
  // Nonsense in storage reads as the default.
  assert.equal(page({stored: {'defalt.intro.mode': 'sideways'}}).context.DefaltIntro.mode(), 'every');

  // Reduced motion, from the system or the booth's own switch: the still, briefly.
  for (const options of [{reducedPreference: true}, {boothReduced: true}]) {
    const still = page(options);
    assert.equal(still.context.DefaltIntro.started, 'still');
    assert.equal(still.video.played, 0, 'no motion');
    assert.equal(still.timers[0].ms, 800);
  }

  // Autoplay refused: the still instead of a stuck black screen.
  const refused = page({playRejects: true});
  await new Promise(resolve => setImmediate(resolve));
  const pending = refused.timers.filter(t => !t.cleared);
  assert.equal(pending.length, 1);
  assert.equal(pending[0].ms, 800);

  // The setting.
  const setting = page();
  assert.equal(setting.elements['intro-mode'].value, 'every');
  setting.elements['intro-mode'].value = 'daily';
  setting.elements['intro-mode'].handlers.change();
  assert.equal(setting.saved.get('defalt.intro.mode'), 'daily');
  setting.elements['intro-mode'].value = 'bogus';
  setting.elements['intro-mode'].handlers.change();
  assert.equal(setting.saved.get('defalt.intro.mode'), 'daily');

  console.log('intro ok');
})().catch(error => { console.error(error); process.exit(1); });
