// Technique lanes in the browser: the isolator, sweep, level, echo and reverb
// sends, the rate (spinback degraded to a brake), rolls, stem stand-ins, and a
// technique that arrives after its record was already handed to Web Audio.
// Real radio.js code; the AudioContext is a recording stand-in.
const vm = require('node:vm');
const assert = require('node:assert/strict');
const {section} = require('./browser_harness');

const FILE = 'web/static/radio.js';
const code = [
  section(FILE, 'const clamp =', 'function mmss('),
  section(FILE, '/* Gain at a point', 'async function pumpAudio() {'),
  section(FILE, 'function stopAll() {', '/* ── Schedule ──'),
].join('\n');

// Scheduled values only; a cancel carries a time, not a value.
const values = (p) => p.calls.filter(([m]) => m !== 'cancel').map(([, v]) => v);

function param(name, value = 0) {
  const p = {name, value, calls: [],
    setValueAtTime(v, t) { p.calls.push(['set', v, t]); return p; },
    linearRampToValueAtTime(v, t) { p.calls.push(['ramp', v, t]); return p; },
    cancelScheduledValues(t) { p.calls.push(['cancel', t]); return p; }};
  return p;
}

function harness() {
  const created = [];
  const node = (kind, extra = {}) => {
    const n = {kind, outputs: [], disconnected: false,
      connect(other) { n.outputs.push(other); return other; },
      disconnect() { n.outputs = []; n.disconnected = true; }, ...extra};
    created.push(n);
    return n;
  };
  const ctx = {
    currentTime: 100, sampleRate: 8000,
    createGain: () => node('gain', {gain: param('gain', 1)}),
    createBiquadFilter: () => node('biquad', {type: '', frequency: param('frequency'), gain: param('gain'), Q: param('Q')}),
    createDelay: () => node('delay', {delayTime: param('delayTime')}),
    createConvolver: () => node('convolver', {buffer: null}),
    createAnalyser: () => node('analyser', {fftSize: 0}),
    createBuffer: (channels, length, rate) => {
      const data = Array.from({length: channels}, () => new Float32Array(length));
      return {sampleRate: rate, length, getChannelData: (i) => data[i]};
    },
    createBufferSource: () => node('source', {playbackRate: param('playbackRate', 1), loop: false,
      start(...args) { this.started = args; },
      stop(t) { this.stops = (this.stops || 0) + 1; if (t !== undefined) this.stopped = t; }}),
  };
  const context = vm.createContext({console, Math, JSON, Number, Object, Array, Float32Array, Infinity});
  vm.runInContext(`
    var ctx = null, master = null, clockOffset = 0, items = [];
    const scheduled = new Map();
  `, context);
  vm.runInContext(code, context);
  context.ctx = ctx;
  vm.runInContext('ctx = this.ctx; master = ctx.createGain();', context);
  const run = (source) => vm.runInContext(source, context);
  return {run, created, ctx, context};
}

const music = (id, start, duration, transition) => ({id, kind: 'music', url: `/a/${id}`, start_at: start,
  duration, offset: 0, envelope: [[0, 1], [duration, 1]],
  meta: {playback_rate: 1, beat_period: 0.5, bpm: 120, ...(transition ? {transition} : {})}});
const near = (a, b, eps = 1e-6) => Math.abs(a - b) <= eps;
// Values from inside the vm are another realm's arrays: compare their JSON.
const same = (actual, expected, message) => assert.equal(JSON.stringify(actual), JSON.stringify(expected), message);

// The outgoing record plays 90..190; the incoming one starts at 185 and its
// transition carries both decks' lanes.
const transition = {
  technique: 'echo_out', preset: 'echo_out', base: 'blend', overlap: 5,
  lanes: {
    out: {
      level: [[184.9, 1], [185.1, 0], [189.99, 0], [190, 1]],
      low: [[183, 0.5], [185, 0]],
      sweep: [[185, 0], [187, 0.55], [190, 0]],
      echo_send: [[184, 0], [184.8, 0.6], [185, 0]],
      echo_feedback: [[184, 0.3], [185, 0.97], [187, 0], [190, 0.3]],
      echo_beats: [[183, 0.75], [190, 0.75]],
      reverb_send: [[184, 0], [185, 0.5], [185.1, 0]],
      rate: [[183, 1], [183.5, -3], [184.5, -1], [185, 0], [190, 1]],
      stem_vocals: [[184, 1], [185, 0]],
    },
    in: {sweep: [[185, -0.6], [187, 0]]},
  },
  events: [{type: 'roll', deck: 'out', at: 181, length_seconds: 1, until: 183},
           {type: 'roll', deck: 'out', at: 183, length_seconds: 0.5, until: 184},
           {type: 'roll', deck: 'in', at: 186, length_seconds: 0.5, until: 187}],
};

(() => {
  // ── The console's knob and fader maps ──
  const {run} = harness();
  assert.equal(run('knobDb(0.5)'), 0);
  assert.equal(run('knobDb(1)'), 6);
  assert.equal(run('knobDb(0)'), -40, 'a kill is a kill');
  assert.ok(run('knobDb(0.25)') < -5 && run('knobDb(0.25)') > -30);
  assert.equal(run('sweepHz(0).lpf'), 20000);
  assert.equal(run('sweepHz(0).hpf'), 20);
  assert.ok(near(run('sweepHz(-1).lpf'), 120), 'fully closed low-pass');
  assert.ok(near(run('sweepHz(1).hpf'), 9020), 'fully open high-pass');
  assert.ok(run('sweepHz(-0.5).lpf') < 20000 && run('sweepHz(0.5).hpf') > 20);

  // ── A spinback becomes a brake: nothing negative reaches Web Audio ──
  const braked = run(`forwardRate(${JSON.stringify(transition.lanes.out.rate)})`);
  assert.ok(braked.every(([, v]) => v > 0), JSON.stringify(braked));
  same(braked[0], [183, 1]);
  assert.ok(braked.some(([t, v]) => t === 185 && v < 0.001), 'stopped where the spin ended');
  same(run('forwardRate([[0, 1], [1, 0]])'), [[0, 1], [1, 0]], 'a brake is left alone');
})();

(() => {
  // ── Each deck gets its own half of the technique, relative to itself ──
  const {run} = harness();
  run(`items = [${JSON.stringify(music('a', 90, 100))}, ${JSON.stringify(music('b', 185, 200, transition))}]`);
  const out = run('laneSet(items[0])'), incoming = run('laneSet(items[1])');
  same(out.lanes.level[0], [184.9 - 90, 1]);
  assert.equal(out.events.length, 2, 'only the outgoing rolls');
  assert.ok(near(out.events[0].at, 91) && near(out.events[0].until, 93));
  same(Object.keys(incoming.lanes), ['sweep']);
  assert.ok(near(incoming.lanes.sweep[0][0], 0));
  assert.equal(incoming.events.length, 1);
  assert.equal(run(`laneSet(${JSON.stringify(music('c', 0, 10))}).key`), '', 'no technique, no lanes');
})();

(() => {
  // ── The outgoing chain: bands, sweep, level, then post-fader sends ──
  const {run, created} = harness();
  run(`items = [${JSON.stringify(music('a', 90, 100))}, ${JSON.stringify(music('b', 185, 200, transition))}]`);
  run('this.chain = buildChain(items[0], 100, 10)');
  const chain = run('this.chain');
  const biquads = created.filter((n) => n.kind === 'biquad');
  const types = biquads.map((n) => n.type);
  for (const type of ['lowshelf', 'lowpass', 'highpass', 'peaking']) assert.ok(types.includes(type), type);
  // The isolator kill reaches -40 dB at 185 (95 s into the record).
  const low = biquads.find((n) => n.type === 'lowshelf');
  assert.ok(low.gain.calls.some(([m, v, t]) => m === 'ramp' && v === -40 && near(t, 100 + 95 - 10)));
  // The vocal stem stands in as a mid cut, capped at -12 dB.
  const mid = biquads.find((n) => n.type === 'peaking');
  assert.ok(mid.gain.calls.some(([, v]) => v === -12));
  // The high-pass climbs as the sweep goes positive.
  const hpf = biquads.find((n) => n.type === 'highpass');
  assert.ok(Math.max(...values(hpf.frequency)) > 1000);
  // Level closes to (near) zero right after the one.
  const gains = created.filter((n) => n.kind === 'gain');
  const level = gains.find((n) => n.gain.calls.some(([m, v, t]) => m === 'ramp' && v <= 0.0001 && near(t, 100 + 95.1 - 10)));
  assert.ok(level, 'the level lane drives a gain node');
  // Echo: one delay line, its time from the beat, feedback clamped under a
  // perfect freeze, and its return joining at the envelope gain.
  const delays = created.filter((n) => n.kind === 'delay');
  assert.equal(delays.length, 1);
  assert.ok(near(delays[0].delayTime.value, 0.375), 'three quarters of a half-second beat');
  const feedback = gains.find((n) => n.outputs.includes(delays[0]) && delays[0].outputs.includes(n));
  assert.ok(feedback, 'a feedback loop');
  assert.ok(Math.max(...values(feedback.gain)) <= 0.98);
  const back = gains.find((n) => delays[0].outputs.includes(n) && n !== feedback);
  assert.ok(back.outputs.includes(chain.gain), 'the echo return joins before the envelope');
  // Reverb: a convolver with a generated impulse, also into the envelope gain.
  const room = created.find((n) => n.kind === 'convolver');
  assert.ok(room.buffer && room.buffer.length > 0);
  assert.ok(room.outputs.includes(chain.gain));
  // Signal order: the chain input leads through the filters to the level.
  let cursor = chain.input, hops = 0;
  while (cursor !== level && hops++ < 20) cursor = cursor.outputs[0];
  assert.equal(cursor, level, 'filters feed the level');
  const post = level.outputs[0];
  assert.ok(post.outputs.length >= 3, 'post-fader point feeds the dry path and both sends');
})();

(() => {
  // ── Scheduling: the rate lane rides playbackRate, rolls loop the slice ──
  const {run, created, ctx} = harness();
  run(`items = [${JSON.stringify(music('a', 90, 100))}, ${JSON.stringify(music('b', 185, 200, transition))}]`);
  // Joining a record already playing starts 20 ms from now: times shift by that.
  const buffer = {duration: 300}, late = 0.021;
  run(`scheduleItem(items[0], ${JSON.stringify(buffer)})`);
  const sources = created.filter((n) => n.kind === 'source');
  assert.equal(sources.length, 3, 'the record and two rolls');
  const [main, ...rolls] = sources;
  assert.ok(main.playbackRate.calls.every(([m, v]) => m === 'cancel' || v > 0), 'never backwards');
  assert.ok(main.playbackRate.calls.some(([m, v, t]) => v < 0.001 && near(t, 185, late)), 'braked to a stop');
  assert.ok(rolls.every((roll) => roll.loop), 'rolls loop');
  const [first, second] = rolls;
  assert.ok(near(first.loopStart, 91) && near(first.loopEnd - first.loopStart, 1));
  assert.ok(near(first.started[0], 181, late) && near(first.stopped, 183.01, late));
  assert.ok(near(second.loopEnd - second.loopStart, 0.5));
  // The record underneath is gated for the whole run, not flashed open at
  // the joint between two rolls.
  const entry = run(`scheduled.get('a')`);
  const gate = entry.gate.gain.calls.filter(([m]) => m !== 'cancel');
  same(gate.map(([, v]) => v), [1, 0, 0, 1]);
  assert.ok(near(gate[0][2], 181, late) && near(gate.at(-1)[2], 184, late));
  assert.ok(ctx);
  // Stopping the station stops the rolls too.
  run('stopAll()');
  assert.ok(rolls.every((roll) => roll.stops === 2), 'scheduled stop, then the stop now');
})();

(() => {
  // ── A technique planned after its record was scheduled is wired in ──
  const {run, created} = harness();
  run(`items = [${JSON.stringify(music('a', 90, 100))}]`);
  run(`scheduleItem(items[0], {duration: 300})`);
  const entry = run(`scheduled.get('a')`);
  const before = entry.chain;
  assert.equal(entry.lanes, '');
  run('refreshLanes()');
  assert.equal(run(`scheduled.get('a').chain`), before, 'nothing changed, nothing rebuilt');
  run(`items = [items[0], ${JSON.stringify(music('b', 185, 200, transition))}]`);
  run('refreshLanes()');
  const after = run(`scheduled.get('a')`);
  assert.notEqual(after.chain, before);
  assert.ok(after.gate.outputs.includes(after.chain.input), 'the gate feeds the new chain');
  assert.ok(before.gain.disconnected, 'the old chain is released');
  assert.equal(created.filter((n) => n.kind === 'source').length, 3, 'the rolls were scheduled too');
  // Too close to call: a technique whose lanes start almost now is left alone.
  run(`items = [${JSON.stringify(music('x', 90, 100))}]; scheduleItem(items[0], {duration: 300});`);
  const late = run(`scheduled.get('x')`).chain;
  const soon = JSON.parse(JSON.stringify(transition));
  soon.lanes.out = {level: [[100.1, 1], [100.2, 0]]};
  soon.events = [];
  run(`items = [items[0], ${JSON.stringify(music('y', 100.2, 50, soon))}]; refreshLanes();`);
  assert.equal(run(`scheduled.get('x')`).chain, late);
})();

console.log('Browser lanes: knob and sweep maps, spinback as brake, per-deck lanes, post-fader sends, rolls and late wiring passed.');
