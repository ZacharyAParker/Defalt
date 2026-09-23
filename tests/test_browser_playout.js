// The playout engine's bookkeeping: decoded-audio eviction, snapshot ordering,
// clock drift, audio interruptions, the events stream and the transcript.
// Real radio.js code; Web Audio, the network and the DOM are stand-ins.
const vm = require('node:vm');
const assert = require('node:assert/strict');
const {section, element} = require('./browser_harness');

const FILE = 'web/static/radio.js';
const code = [
  section(FILE, 'const clamp =', 'function playbackCurve('),
  section(FILE, 'async function bufferFor(url)', '/* Gain at a point'),
  section(FILE, '/* ── Schedule ──', '/* ── Now playing + reporting'),
].join('\n');

function harness() {
  const calls = {api: [], stops: 0, pumps: 0, queue: [], vibe: [], ads: 0, fetches: []};
  const pending = [];                       // api() promises we settle by hand
  const sources = [];
  class FakeEventSource {
    constructor(url) { this.url = url; this.listeners = {}; this.closed = false; sources.push(this); }
    addEventListener(name, fn) { this.listeners[name] = fn; }
    close() { this.closed = true; }
    emit(name, data) { this.listeners[name]({data: JSON.stringify(data)}); }
  }
  const timers = [];
  const context = vm.createContext({
    console, performance: {now: () => context.__ms}, __ms: 0,
    EventSource: FakeEventSource,
    setTimeout: (fn, ms) => { timers.push({fn, ms}); return timers.length; },
    clearTimeout() {},
    document: {createElement: (tag) => element(tag)},
    fetch: async (url) => { calls.fetches.push(url); return {ok: true, arrayBuffer: async () => new ArrayBuffer(8)}; },
    api: (path) => new Promise((resolve, reject) => { calls.api.push(path); pending.push({path, resolve, reject}); }),
    ui: {state: element(), timelineWrap: element(), transcript: element(), transcriptEmpty: null},
    setText: (node, text) => { node.textContent = String(text); },
    updateAdControls: () => { calls.ads++; },
    refreshScheduledGains() {},
    pumpAudio: () => { calls.pumps++; },
    stopAll: () => { calls.stops++; context.scheduled.clear(); },
    loadQueue: (data) => calls.queue.push(data),
    showVibe: (vibe) => calls.vibe.push(vibe),
  });
  vm.runInContext(`
    let ctx = null, clockOffset = null, items = [], epoch = null, running = true;
    const scheduled = new Map(), buffers = new Map(), seenLines = new Set(), reported = new Set();
    const MEMORY_CAP = 400;
    const stationNow = () => (clockOffset === null ? 0 : ctx.currentTime - clockOffset);
    const streamMode = false, clockNow = () => ctx.currentTime;
    this.scheduled = scheduled;
  `, context);
  vm.runInContext(code, context);
  const run = (source) => vm.runInContext(source, context);
  return {context, calls, pending, sources, timers, run};
}

const item = (id, start, duration, extra = {}) =>
  ({id, kind: 'music', url: `/media/audio/${id}.opus`, start_at: start, duration, envelope: [[0, 1]], meta: {}, ...extra});
const same = (actual, expected, message) => assert.equal(JSON.stringify(actual), JSON.stringify(expected), message);
const tick = () => new Promise((resolve) => setImmediate(resolve));

(async () => {
  // ── Decoded buffers are evicted once nothing needs them ──
  {
    const {run, context} = harness();
    run(`ctx = {currentTime: 100, decodeAudioData: async () => ({decoded: true})}; clockOffset = 0;`);
    run(`items = [${JSON.stringify(item('old', 10, 60))}, ${JSON.stringify(item('now', 80, 60))}, ${JSON.stringify(item('next', 140, 60))}];`);
    await run(`Promise.all(items.map((i) => bufferFor(i.url)))`);
    run(`buffers.set('/media/audio/gone.opus', {});`);
    run('evictBuffers()');
    same(run('[...buffers.keys()].sort()'), ['/media/audio/next.opus', '/media/audio/now.opus'],
      'finished and unscheduled records are released');
    // A record that ended moments ago is kept for a few seconds...
    run(`items = [${JSON.stringify(item('recent', 40, 57))}]; buffers.set('/media/audio/recent.opus', {});`);
    run('evictBuffers()');
    assert.ok(run(`buffers.has('/media/audio/recent.opus')`));
    // ...and anything still scheduled stays whatever the schedule says.
    run(`scheduled.set('x', {item: ${JSON.stringify(item('held', 0, 1))}}); buffers.set('/media/audio/held.opus', {}); items = [];`);
    run('evictBuffers()');
    same(run('[...buffers.keys()]'), ['/media/audio/held.opus']);
    // A decode that finishes after being evicted is returned but not re-cached.
    let finish;
    run(`ctx.decodeAudioData = () => new Promise((resolve) => { this.__finish = resolve; });`);
    const late = run(`bufferFor('/media/audio/slow.opus')`);
    await tick();
    run(`scheduled.clear(); evictBuffers();`);
    finish = context.__finish; finish({late: true});
    same(await late, {late: true});
    assert.equal(run(`buffers.has('/media/audio/slow.opus')`), false, 'an evicted decode does not come back');
  }

  // ── Snapshots: stale replies and old epochs are ignored ──
  {
    const {run, pending, calls} = harness();
    run(`ctx = {currentTime: 50, state: 'running'};`);
    const first = run('poll()'), second = run('poll()');
    pending[1].resolve({now: 20, epoch: 3, items: [item('b', 10, 30)], status: 'newer'});
    await second;
    pending[0].resolve({now: 19, epoch: 3, items: [item('a', 10, 30)], status: 'older'});
    await first;
    assert.equal(run('items[0].id'), 'b', 'a reply that lands late never overwrites a newer one');
    assert.equal(calls.pumps, 1);
    // An older epoch is a straggler, unless it keeps arriving (a restarted server).
    assert.equal(run(`applySnapshot({now: 21, epoch: 2, items: [], status: ''}, ++snapshotSeq, 0)`), false);
    assert.equal(run(`applySnapshot({now: 21, epoch: 2, items: [], status: ''}, ++snapshotSeq, 0)`), false);
    assert.equal(run(`applySnapshot({now: 21, epoch: 0, items: [], status: 'restarted'}, ++snapshotSeq, 0)`), true);
    assert.equal(run('epoch'), 0);
    assert.equal(calls.stops, 1, 'a new epoch drops what was scheduled');
  }

  // ── Clock drift is measured and corrected, but never under a playing item ──
  {
    const {run} = harness();
    run(`ctx = {currentTime: 1000, state: 'running'};`);
    run(`applySnapshot({now: 100, epoch: 1, items: [], status: ''}, ++snapshotSeq, 0.01)`);
    assert.ok(Math.abs(run('clockOffset') - 899.99) < 1e-9, 'first snapshot anchors, allowing for latency');
    run(`scheduled.set('playing', {item: ${JSON.stringify(item('p', 90, 200))}, offset: clockOffset});`);
    // 0.4 s of drift, measured three times, moves the shared clock...
    for (const t of [10, 20, 30]) {
      run(`ctx.currentTime = ${1000 + t}; applySnapshot({now: ${100 + t - 0.4}, epoch: 1, items: [], status: ''}, ++snapshotSeq, 0.01)`);
    }
    assert.ok(Math.abs(run('clockOffset') - 900.39) < 1e-6, `re-anchored, got ${run('clockOffset')}`);
    // ...but the item already handed to Web Audio keeps its own offset.
    assert.ok(Math.abs(run(`scheduled.get('playing').offset`) - 899.99) < 1e-9);
    // Small jitter is left alone.
    const before = run('clockOffset');
    for (const [t, j] of [[40, 0.05], [50, -0.08], [60, 0.1]]) {
      run(`ctx.currentTime = ${1000 + t}; applySnapshot({now: ${100 + t - 0.4 + j}, epoch: 1, items: [], status: ''}, ++snapshotSeq, 0.01)`);
    }
    assert.equal(run('clockOffset'), before);
    // One wild sample does not move a playing clock (median of the last few).
    run(`ctx.currentTime = 1070; applySnapshot({now: ${170 - 0.4 + 3}, epoch: 1, items: [], status: ''}, ++snapshotSeq, 0.01)`);
    assert.equal(run('clockOffset'), before);
    // A suspended context's clock is frozen: never measured.
    run(`driftSamples.length = 0; ctx.state = 'suspended';`);
    for (let i = 0; i < 4; i++) run(`applySnapshot({now: 500, epoch: 1, items: [], status: ''}, ++snapshotSeq, 0.01)`);
    assert.equal(run('clockOffset'), before);
  }

  // ── Audio interruptions resync from the station's clock ──
  {
    const {run, calls, pending} = harness();
    run(`ctx = {currentTime: 5, state: 'suspended'}; clockOffset = 1; watchAudioState();`);
    run(`ctx.state = 'running'; ctx.onstatechange();`);
    assert.equal(calls.stops, 1); assert.equal(run('clockOffset'), null);
    assert.equal(pending.at(-1).path, '/api/schedule', 'a fresh schedule is fetched');
    run(`ctx.state = 'interrupted'; ctx.onstatechange(); ctx.state = 'running'; ctx.onstatechange();`);
    assert.equal(calls.stops, 2);
    run(`running = false; ctx.state = 'suspended'; ctx.onstatechange(); ctx.state = 'running'; ctx.onstatechange();`);
    assert.equal(calls.stops, 2, 'a stopped station is left alone');
  }

  // ── Events stream: used when it works, polling when it does not ──
  {
    const {run, sources, calls, timers, pending} = harness();
    run(`ctx = {currentTime: 10, state: 'running'};`);
    run('connectEvents()');
    assert.equal(sources.length, 1);
    assert.equal(sources[0].url, '/api/events?topics=schedule,queue,vibe');
    sources[0].onopen();
    assert.equal(run('eventsOpen'), true);
    sources[0].emit('schedule', {now: 4, epoch: 1, items: [item('s', 0, 30)], status: 'streamed'});
    assert.equal(run('items[0].id'), 's');
    sources[0].emit('queue', {items: [{id: 'q'}]});
    sources[0].emit('vibe', {vibe: {description: 'calm'}});
    same(calls.queue.at(-1), {items: [{id: 'q'}]});
    same(calls.vibe.at(-1), {description: 'calm'});
    // The stream fails: poll straight away, retry with growing waits.
    sources[0].onerror();
    assert.equal(sources[0].closed, true);
    assert.equal(run('eventsOpen'), false);
    assert.equal(pending.at(-1).path, '/api/schedule');
    assert.equal(timers.at(-1).ms, 2000);
    timers.at(-1).fn();
    assert.equal(sources.length, 2, 'retried');
    sources[1].onerror();
    assert.equal(timers.at(-1).ms, 4000, 'backs off');
    timers.at(-1).fn(); sources[2].onopen();
    assert.equal(run('eventsRetry'), 2000, 'a good connection resets the backoff');
    run('disconnectEvents()');
    assert.equal(sources[2].closed, true);
    assert.equal(run('events'), null);
    // Nothing connects while stopped (an open stream counts as a listener).
    run('running = false; connectEvents()');
    assert.equal(sources.length, 3);
  }

  // ── Transcript: lines that never aired are dropped; memory is bounded ──
  {
    const {run, context} = harness();
    const voice = (id, start) => item(id, start, 4, {kind: 'voice', meta: {text: `line ${id}`, host: 'Mav'}});
    run(`ctx = {currentTime: 100, state: 'running'};`);
    run(`applySnapshot(${JSON.stringify({now: 50, epoch: 1, status: '', items: [voice('aired', 49), voice('later', 70), voice('cancelled', 80)]})}, ++snapshotSeq, 0)`);
    assert.equal(run('pendingLines.length'), 3);
    // A skip: new epoch, the cancelled break is gone, the clock jumps.
    run(`ctx.currentTime = 101;`);
    run(`applySnapshot(${JSON.stringify({now: 120, epoch: 2, status: '', items: [voice('later', 70)]})}, ++snapshotSeq, 0)`);
    same(run('pendingLines.map((l) => l.id)'), ['aired', 'later'],
      'the line that already played stays; the one that never will is dropped');
    // The same epoch: a future line removed from the plan is dropped too.
    run(`applySnapshot(${JSON.stringify({now: 121, epoch: 2, status: '', items: [voice('soon', 200)]})}, ++snapshotSeq, 0)`);
    run(`applySnapshot(${JSON.stringify({now: 122, epoch: 2, status: '', items: []})}, ++snapshotSeq, 0)`);
    assert.equal(run(`pendingLines.some((l) => l.id === 'soon')`), false);
    run(`for (let i = 0; i < 1000; i++) remember(seenLines, 'line-' + i);`);
    assert.equal(run('seenLines.size'), 400);
    assert.equal(run(`seenLines.has('line-999') && !seenLines.has('line-0')`), true, 'the oldest are forgotten first');
    assert.ok(context);
  }

  console.log('Browser playout: buffer eviction, snapshot ordering, drift correction, audio resync, events fallback and transcript pruning passed.');
})().catch((error) => { console.error(error); process.exitCode = 1; });
