// The first-run notice: shown once per device and terms version, can't be
// dismissed without agreeing, and holds playback until it is accepted.
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { read, section, element } = require('./browser_harness');

const html = read('web/index.html');
assert.match(html, /<dialog class="info-window legal-window" id="legal-window"[^>]* data-terms-version="\{\{TERMS_VERSION\}\}"/);
assert.match(html, /<script src="\/static\/legal\.js\?v=\{\{APP_VERSION\}\}" defer><\/script>\s*\n(?:.*\n)*?<script src="\/static\/radio\.js/,
  'the notice is ready before the player can start');
for (const page of ['terms', 'license', 'privacy']) {
  const notice = html.split('id="legal-window"')[1].split('</dialog>')[0];
  assert.ok(notice.includes(`data-info-page="${page}"`), `the notice links the ${page} page`);
}

function page({stored = null, version = '2026-09-25', storage = true, reducedBooth = false} = {}) {
  const ids = ['legal-window', 'legal-agree', 'legal-reduced', 'reduced'];
  const elements = Object.fromEntries(ids.map(id => [id, element()]));
  const dialog = elements['legal-window'];
  dialog.dataset.termsVersion = version;
  dialog.open = false;
  dialog.showModal = () => { dialog.open = true; dialog.shown = (dialog.shown || 0) + 1; };
  dialog.close = () => { dialog.open = false; dialog.handlers.close?.(); };
  elements.reduced.checked = reducedBooth;
  const booth = [];
  elements.reduced.dispatchEvent = event => booth.push(event.type);
  const saved = new Map(stored === null ? [] : [['defalt.legal.accepted', stored]]);
  const localStorage = {
    getItem: key => { if (!storage) throw new Error('blocked'); return saved.has(key) ? saved.get(key) : null; },
    setItem: (key, value) => { if (!storage) throw new Error('blocked'); saved.set(key, value); },
  };
  const classes = new Set();
  const context = {
    document: {
      getElementById: id => elements[id],
      body: {classList: {add: c => classes.add(c), remove: c => classes.delete(c)}},
    },
    localStorage, Event: class { constructor(type) { this.type = type; } },
  };
  context.globalThis = context;
  vm.runInNewContext(read('web/static/legal.js'), context);
  return {context, dialog, elements, saved, classes, booth};
}

// First visit: the notice is up, Escape doesn't dismiss it.
let first = page();
assert.equal(first.dialog.open, true);
assert.equal(first.context.DefaltLegal.accepted(), false);
let prevented = false;
first.dialog.handlers.cancel({preventDefault() { prevented = true; }});
assert.equal(prevented, true, 'Escape must not count as agreeing');
let stopped = false;
first.dialog.handlers.keydown({stopPropagation() { stopped = true; }});
assert.equal(stopped, true, 'keys typed at the notice must not reach playback shortcuts');

// Reduced motion from the notice flips the booth's own switch.
first.elements['legal-reduced'].checked = true;
first.elements['legal-reduced'].handlers.change();
assert.equal(first.elements.reduced.checked, true);
assert.deepEqual(first.booth, ['change'], 'the booth saves and applies its setting');

// Agreeing remembers this terms version and closes it.
first.elements['legal-agree'].handlers.click();
assert.equal(first.dialog.open, false);
assert.equal(first.saved.get('defalt.legal.accepted'), '2026-09-25');
assert.equal(first.context.DefaltLegal.accepted(), true);
assert.equal(first.classes.size, 0);

// Same version next time: no notice.
const again = page({stored: '2026-09-25'});
assert.equal(again.dialog.open, false);
assert.equal(again.context.DefaltLegal.accepted(), true);

// New terms: asked again, and the notice picks up the booth's current setting.
const newer = page({stored: '2026-01-01', reducedBooth: true});
assert.equal(newer.dialog.open, true);
assert.equal(newer.elements['legal-reduced'].checked, true);

// Blocked storage still lets you in for this visit.
const blocked = page({storage: false});
assert.equal(blocked.dialog.open, true);
blocked.elements['legal-agree'].handlers.click();
assert.equal(blocked.context.DefaltLegal.accepted(), true);
assert.equal(blocked.dialog.open, false);

// The player's start() stops at the notice until it is accepted.
const start = section('web/static/radio.js', 'async function start() {', '  if (streamMode) {');
const gate = page();
let reached = false;
const run = vm.runInNewContext(`${start}\n  reached(); }\n start;`, Object.assign(gate.context, {reached: () => { reached = true; }}));
gate.dialog.open = false;
run();
assert.equal(reached, false, 'playback started before the terms were accepted');
assert.equal(gate.dialog.open, true, 'pressing play brings the notice back');
gate.elements['legal-agree'].handlers.click();
run();
assert.equal(reached, true);

console.log('Browser first-run notice: once per terms version, no Escape, reduced motion, blocked storage and the playback gate passed.');
