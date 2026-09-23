// The Up next list: pushed queues from the events stream, overlapping loads,
// and controls that stay usable.
const vm = require('node:vm');
const assert = require('node:assert/strict');
const {section, element} = require('./browser_harness');

const calls = [], later = [];
const replies = {'/api/queue': {items: [{id: 'fetched', stage: 'queued', title: 'Fetched'}]}, '/api/requests': {wishes: []}};
let gate = null;
const ui = Object.fromEntries(['queue', 'lineupCount', 'lineupEmpty', 'queueClear'].map((key) => [key, element()]));
const context = vm.createContext({
  ui, KIND_LABEL: {}, mmss: (s) => `${s}s`, toast() {},
  document: {createElement: (tag) => element(tag)},
  setTimeout: (fn) => later.push(fn),
  api: async (path) => { calls.push(path); if (gate) await gate; return replies[path]; },
});
vm.runInContext(section('web/static/radio.js', 'const STAGE_LABEL =', '/* ── Board'), context);

const labels = () => ui.queue.children.map((li) => li.children[1].textContent);

(async () => {
  await context.loadQueue();
  assert.deepEqual(calls, ['/api/queue', '/api/requests']);
  assert.deepEqual(labels(), ['Fetched']);

  // A queue delivered by the events stream is rendered without fetching it again.
  calls.length = 0;
  await context.loadQueue({items: [{id: 'pushed', stage: 'on_deck', title: 'Pushed', artist: 'Band'}]});
  assert.deepEqual(calls, ['/api/requests']);
  assert.deepEqual(labels(), ['Band — Pushed']);

  // A push that lands mid-load is not lost: it runs once the first finishes.
  calls.length = 0;
  let open; gate = new Promise((resolve) => { open = resolve; });
  const first = context.loadQueue();
  await context.loadQueue({items: [{id: 'a', stage: 'queued', title: 'Older push'}]});
  await context.loadQueue({items: [{id: 'b', stage: 'queued', title: 'Newest push'}]});
  gate = null; open(); await first;
  assert.equal(later.length, 1, 'one follow-up, not one per call');
  await later.shift()();
  assert.deepEqual(labels(), ['Newest push']);

  // Row controls are real buttons with names.
  await context.loadQueue({items: [
    {id: 'x', stage: 'queued', title: 'One', can_move: true, can_remove: true},
    {id: 'y', stage: 'queued', title: 'Two', can_move: true, can_remove: true},
  ]});
  const actions = ui.queue.children[1].children.at(-1).children;
  assert.deepEqual(actions.map((b) => b.attributes['aria-label']),
    ['Play next: Two', 'Move up: Two', 'Remove: Two']);
  assert.ok(actions.every((b) => b.className === 'queue__act'));

  // Up next says how each planned record comes in; the one on air does not.
  await context.loadQueue({items: [
    {id: 'p', stage: 'on_deck', title: 'Now', playing: true, transition: {technique: 'brake'}},
    {id: 'n', stage: 'on_deck', title: 'Next', transition: {technique: 'loop_roll', preset: 'loop_roll'}},
    {id: 'o', stage: 'on_deck', title: 'Old', transition: {preset: 'blend'}},
  ]});
  const notes = (li) => li.children.filter((c) => c.className === 'queue__note tnum').map((c) => c.textContent);
  assert.ok(!notes(ui.queue.children[0]).some((t) => t.startsWith('into:')));
  assert.ok(notes(ui.queue.children[1]).includes('into: loop roll'));
  assert.ok(notes(ui.queue.children[2]).includes('into: blend'), 'an older server without techniques');
  console.log('Browser queue: pushed queues, overlapping loads, row controls and planned techniques passed.');
})().catch((error) => { console.error(error); process.exitCode = 1; });
