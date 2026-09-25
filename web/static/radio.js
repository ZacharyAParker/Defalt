/* =========================================================================
   Side Room — playout engine.

   The server decides everything: what plays, when it starts, and the exact
   gain envelope for every item. This file schedules those items on the Web
   Audio clock and draws what is happening. It makes no programming decisions
   of its own, which is why crossfades and ducking are sample-accurate rather
   than approximated with setTimeout.
   ========================================================================= */
"use strict";

const POLL_MS = 4000;
const DECODE_AHEAD = 70;   // seconds of schedule to fetch and decode ahead
const SCHEDULE_AHEAD = 45; // seconds of schedule to hand to the audio clock

const el = (id) => document.getElementById(id);

const ui = {
  root: document.documentElement,
  lamp: el("lamp"), state: el("state-value"), uptime: el("uptime"),
  wallclock: el("wallclock"),
  source: el("now-source"), title: el("now-title"), artist: el("now-artist"),
  lyric: { box: el("now-lyric"), line: el("lyric-line"), next: el("lyric-next") },
  scope: el("scope"), meterL: el("meter-l"), meterR: el("meter-r"),
  progress: el("progress"), fill: el("progress-fill"),
  pos: el("pos"), dur: el("dur"),
  power: el("power"), powerLabel: el("power-label"), skip: el("skip"),
  volume: el("volume"), mute: el("mute"), volIcon: el("vol-icon"),
  up: el("rate-up"), down: el("rate-down"), hint: el("hint"),
  transcript: el("transcript"), transcriptEmpty: el("transcript-empty"),
  hostPill: el("host-pill"),
  form: el("request-form"), input: el("request-input"), note: el("request-note"),
  requestMode: el("request-mode"), requestPrompt: el("request-prompt"), articleInput: el("article-input"),
  spotifyNote: el("spotify-note"), spotifyResults: el("spotify-results"),
  vibePanel: el("vibe-panel"), vibeDescription: el("vibe-description"), vibeClear: el("vibe-clear"),
  queue: el("queue"), lineupCount: el("lineup-count"),
  lineupEmpty: el("lineup-empty"), queueClear: el("queue-clear"),
  timeline: el("timeline"), timelineWrap: document.querySelector(".timeline"),
  faders: el("faders"), panelToggle: el("panel-toggle"), panelBody: el("panel-body"),
  toast: el("toast"),
  adNext: el("ad-next"), adNow: el("ad-now"), adStatus: el("ad-status"),
};

/* ── State ─────────────────────────────────────────────────────────── */
let ctx = null;
let master = null;
let analyser = null;
let running = false;

let clockOffset = null;      // audioCtx.currentTime - stationTime
let items = [];              // everything the server has told us about
const scheduled = new Map(); // item.id -> { source, gain, item, offset }
const buffers = new Map();   // url -> AudioBuffer | Promise
const seenLines = new Set();
const reported = new Set();
const MEMORY_CAP = 400;      // ids remembered for de-duplication, per set
let currentKey = null;
let startedAt = 0;
let hostNames = "";
let epoch = null;          // server bumps this when the clock jumps
let volume = 0.9;
let muted = false;
/* Stream mode (stream.js): the console at home mixes, /listen carries it, and
   this page only follows the station -- no Web Audio graph at all. */
let streamMode = false;
let remote = null;

/* ── Small helpers ─────────────────────────────────────────────────── */
const clamp = (n, lo, hi) => Math.min(Math.max(n, lo), hi);

/* Remember an id, forgetting the oldest once the set is full. A session left
   on for days would otherwise keep every line and report it ever made. */
function remember(set, value, cap = MEMORY_CAP) {
  set.add(value);
  while (set.size > cap) set.delete(set.values().next().value);
}

/* `trim_db` is loudness correction applied on top of the
   envelope. Clamped, because a bad measurement must never blow a speaker. */
function trimGain(item) {
  if (item.kind !== "music") return 1;
  const db = Number(item.meta?.trim_db);
  return Number.isFinite(db) && db !== 0 ? 10 ** (clamp(db, -24, 12) / 20) : 1;
}

function itemEnvelope(item) {
  const trim = trimGain(item);
  const envelope = item.envelope || [];
  return trim === 1 ? envelope : envelope.map(([time, gain]) => [time, gain * trim]);
}

function playbackCurve(item) {
  const initial = item.kind === "music" ? clamp(Number(item.meta?.playback_rate) || 1, 0.92, 1.08) : 1;
  const points = item.kind === "music" ? item.meta?.rate_curve : null;
  if (!Array.isArray(points) || !points.length || points.length > 16) return [[0, initial]];
  let last = -1;
  for (const point of points) {
    if (!Array.isArray(point) || !Number.isFinite(point[0]) || !Number.isFinite(point[1])
      || point[0] <= last || point[0] < 0 || point[1] < 0.92 || point[1] > 1.08) return [[0, initial]];
    last = point[0];
  }
  return points[0][0] === 0 ? points : [[0, initial]];
}

function playbackAt(points, elapsed) {
  elapsed = Math.max(0, elapsed);
  let previous = points[0], source = 0;
  for (let i = 1; i < points.length; ++i) {
    const point = points[i], span = Math.min(elapsed, point[0]) - previous[0];
    const rate = previous[1] + (point[1] - previous[1]) * span / (point[0] - previous[0]);
    source += span * (previous[1] + rate) / 2;
    if (elapsed <= point[0]) return { source, rate };
    previous = point;
  }
  return { source: source + (elapsed - previous[0]) * previous[1], rate: previous[1] };
}

function mmss(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${String(s).padStart(2, "0")}`;
}

function hhmmss(seconds) {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

let toastTimer = null;
function toast(message, tone = "") {
  ui.toast.textContent = message;
  ui.toast.dataset.tone = tone;
  ui.toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { ui.toast.hidden = true; }, 4200);
}

/* Writing the same text, attribute or custom property every frame still
   dirties style and layout. These only write when the value changes. */
function setText(node, text) {
  text = String(text);
  if (node.textContent !== text) node.textContent = text;
}
function setVar(node, name, value) {
  if (node.style.getPropertyValue(name) !== value) node.style.setProperty(name, value);
}
function setData(node, key, value) {
  if (node.dataset[key] !== value) node.dataset[key] = value;
}

async function api(path, options) {
  const response = await fetch(path, {
    headers: { "Content-Type": "application/json" }, ...options,
  });
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    // Refusals arrive with a reason already written for a person. Show that,
    // not "request failed (400)".
    const error = new Error(data.error || data.message
                            || `request failed (${response.status})`);
    error.payload = data;
    throw error;
  }
  return data;
}

/* Station time, as the server counts it. In stream mode it is the time of
   what you are hearing: the stream runs a few seconds behind the clock. */
const clockNow = () => (streamMode ? performance.now() / 1000 : ctx.currentTime);
const stationNow = () => (clockOffset === null ? 0
  : clockNow() - clockOffset - (streamMode && remote ? remote.delay() : 0));

/* ── Audio graph ───────────────────────────────────────────────────── */
function buildGraph() {
  ctx = new (window.AudioContext || window.webkitAudioContext)();
  master = ctx.createGain();
  master.gain.value = muted ? 0.0001 : Math.max(volume, 0.0001);
  analyser = ctx.createAnalyser();
  analyser.fftSize = 2048;
  analyser.smoothingTimeConstant = 0.72;
  master.connect(analyser);
  analyser.connect(ctx.destination);
}

async function bufferFor(url) {
  if (buffers.has(url)) return buffers.get(url);
  const pending = (async () => {
    const response = await fetch(url);
    if (!response.ok) throw new Error(`could not load ${url}`);
    return ctx.decodeAudioData(await response.arrayBuffer());
  })();
  buffers.set(url, pending);
  try {
    const buffer = await pending;
    // Evicted while decoding: hand it over, but do not cache it again.
    if (buffers.get(url) === pending) buffers.set(url, buffer);
    return buffer;
  } catch (error) {
    if (buffers.get(url) === pending) buffers.delete(url);
    throw error;
  }
}

/* A decoded record is around 80 MB of float PCM. Keep only what the schedule
   still needs -- anything scheduled, or any item that has not been over for
   more than a few seconds -- and let the rest go. */
const BUFFER_GRACE = 5;
function evictBuffers() {
  const now = stationNow();
  const keep = new Set();
  for (const item of items) {
    if (clockOffset === null || item.start_at + item.duration > now - BUFFER_GRACE) keep.add(item.url);
  }
  for (const entry of scheduled.values()) keep.add(entry.item.url);
  for (const url of buffers.keys()) {
    if (!keep.has(url)) buffers.delete(url);
  }
}

/* Gain at a point inside an envelope, by linear interpolation between the
   two surrounding breakpoints. Needed when we join an item already playing. */
function gainAt(envelope, time) {
  if (!envelope.length) return 1;
  if (time <= envelope[0][0]) return envelope[0][1];
  for (let i = 0; i < envelope.length - 1; i++) {
    const [t0, g0] = envelope[i];
    const [t1, g1] = envelope[i + 1];
    if (time >= t0 && time <= t1) {
      if (t1 === t0) return g1;
      return g0 + (g1 - g0) * ((time - t0) / (t1 - t0));
    }
  }
  return envelope[envelope.length - 1][1];
}

/* `minimum` differs per parameter: a gain must never reach zero, a filter
   frequency must stay inside the audible range, and an EQ band in dB is
   legitimately negative and must not be clamped at all. */
function applyEnvelope(param, envelope, when, offset, minimum = 0.0001) {
  const start = Math.max(when, ctx.currentTime);
  param.cancelScheduledValues(start);
  param.setValueAtTime(Math.max(gainAt(envelope, offset), minimum), start);
  for (const [time, value] of envelope) {
    if (time <= offset) continue;
    param.linearRampToValueAtTime(Math.max(value, minimum),
                                  when + (time - offset));
  }
}

/* Crossover points for the three-band EQ. A low shelf under the bass, a
   peaking band across the body of the mix, a high shelf over the top. */
const BAND_LOW_HZ = 220;
const BAND_MID_HZ = 1400;
const BAND_HIGH_HZ = 5000;

/* ── Technique lanes ─────────────────────────────────────────────────
   A technique transition (docs/TRANSITIONS.md, "Automation lanes") carries
   curves for both decks on the incoming record's `transition`: `lanes.out`
   drive the record before it, `lanes.in` the record itself. Times are
   station seconds. Web Audio runs what it can -- level, the isolator bands,
   the filter sweep, the echo and reverb sends, the rate -- and degrades the
   rest: a negative rate (spinback) becomes a brake, and stem lanes nudge the
   bands a separated deck would have moved (vocals the mids, bass the low
   shelf). Drums and harmony have no honest approximation and are ignored. */
const ECHO_RETURN = 0.8;
const LANE_STEPS = 6;              // breakpoints per segment for curved maps

/* The isolator knob (0 kill, 0.5 flat, 1 boost) in dB, as the console maps it. */
function knobDb(knob) {
  const k = clamp(Number(knob), 0, 1);
  if (k >= 0.5) return (k - 0.5) * 12;
  if (k <= 0.005) return -40;
  const t = 1 - k / 0.5;
  return -30 * t * t;
}

/* The sweep fader (-1 low-pass .. 0 off .. 1 high-pass) as two cutoffs. */
function sweepHz(value) {
  const s = clamp(Number(value), -1, 1);
  if (s < 0) {
    const t = -s;
    return { lpf: 20000 * (1 - t) ** 2.4 + 120 * t, hpf: 20 };
  }
  return { lpf: 20000, hpf: 20 + 9000 * s ** 2.4 };
}

/* A stem level (0..2) as the band cut that stands in for it. */
const stemDb = (limit) => (level) => clamp(20 * Math.log10(Math.max(Number(level), 1e-4)), limit, 6);

/* Resample a lane through a nonlinear map so linear ramps between the
   results still trace the curve. */
function mapLane(points, fn) {
  const out = [];
  for (let i = 0; i < points.length; i++) {
    const [t, v] = points[i];
    if (i > 0) {
      const [t0, v0] = points[i - 1];
      if (t > t0 && v !== v0) {
        for (let s = 1; s < LANE_STEPS; s++) {
          const x = s / LANE_STEPS;
          out.push([t0 + (t - t0) * x, fn(v0 + (v - v0) * x)]);
        }
      }
    }
    out.push([t, fn(v)]);
  }
  return out;
}

/* A spinback plays backwards; Web Audio will not. Brake instead: from the
   last forward point, down to a stop where the backwards burst ended. */
function forwardRate(points) {
  const first = points.findIndex((p) => p[1] < 0);
  if (first < 0) return points;
  let last = first;
  for (let i = first; i < points.length; i++) if (points[i][1] < 0) last = i;
  return [...points.slice(0, first), [points[last][0], 0.0001],
          ...points.slice(last + 1).map(([t, v]) => [t, Math.max(v, 0.0001)])];
}

/* The music item after this one: its transition holds this item's tail. */
function nextMusic(item) {
  let best = null;
  for (const other of items) {
    if (other.kind !== "music" || other.id === item.id || other.start_at <= item.start_at) continue;
    if (!best || other.start_at < best.start_at) best = other;
  }
  return best;
}

/* Every lane and event that touches this item, relative to its own start. */
function laneSet(item) {
  const found = { lanes: {}, events: [], key: "" };
  if (item.kind !== "music") return found;
  const take = (transition, deck) => {
    const lanes = transition?.lanes?.[deck];
    if (lanes && typeof lanes === "object") {
      for (const [name, points] of Object.entries(lanes)) {
        if (!Array.isArray(points) || !points.length) continue;
        const moved = points.filter((p) => Array.isArray(p) && Number.isFinite(p[0]) && Number.isFinite(p[1]))
          .map(([t, v]) => [t - item.start_at, v]);
        if (moved.length) found.lanes[name] = moved;
      }
    }
    for (const event of transition?.events || []) {
      if (event?.deck !== deck || !["roll", "loop"].includes(event.type)) continue;
      const at = Number(event.at) - item.start_at, until = Number(event.until) - item.start_at;
      const length = Number(event.length_seconds);
      if (Number.isFinite(at) && Number.isFinite(until) && until > at && length > 0.01) {
        found.events.push({ at, until, length });
      }
    }
  };
  take(item.meta?.transition, "in");
  const next = nextMusic(item);
  if (next) take(next.meta?.transition, "out");
  if (Object.keys(found.lanes).length || found.events.length) {
    found.key = JSON.stringify([found.lanes, found.events]);
  }
  return found;
}

/* One seconds-per-beat for the echo, at the rate the record is playing. */
function beatSeconds(item) {
  const rate = Number(item.meta?.playback_rate) || 1;
  const period = Number(item.meta?.beat_period);
  if (period > 0) return period / rate;
  const bpm = Number(item.meta?.bpm);
  return bpm > 0 ? 60 / (bpm * rate) : 0.5;
}

/* A short, dark room: stereo noise under an exponential decay. Built once. */
let reverbImpulse = null;
function impulse() {
  if (reverbImpulse && reverbImpulse.sampleRate === ctx.sampleRate) return reverbImpulse;
  const rate = ctx.sampleRate || 48000, length = Math.floor(rate * 2.4);
  const buffer = ctx.createBuffer(2, length, rate);
  for (let channel = 0; channel < 2; channel++) {
    const data = buffer.getChannelData(channel);
    let seed = 1 + channel * 7919;
    for (let i = 0; i < length; i++) {
      seed = (seed * 16807) % 2147483647;
      data[i] = ((seed / 2147483647) * 2 - 1) * Math.exp(-3.2 * i / length) * 0.5;
    }
  }
  reverbImpulse = buffer;
  return buffer;
}

/* Build only the nodes a transition actually needs. Most records get a plain
   gain node; a filtered transition adds shelves and sweeps for that item
   alone, so the graph stays as small as the programming allows. */
function buildChain(item, startAt, offset) {
  const automation = (item.meta && item.meta.automation) || {};
  const { lanes } = laneSet(item);
  const nodes = [];
  const series = [];               // processing in order, before the fader
  const gain = ctx.createGain();

  const shelf = (type, frequency, points) => {
    if (!points || !points.length) return null;
    const node = ctx.createBiquadFilter();
    node.type = type;
    node.frequency.value = frequency;
    if (type === "peaking") node.Q.value = 0.7;
    node.gain.value = 0;
    applyEnvelope(node.gain, points, startAt, offset, -60);
    return node;
  };

  const sweep = (type, resting, points) => {
    if (!points || !points.length) return null;
    const node = ctx.createBiquadFilter();
    node.type = type;
    node.Q.value = 0.4;              // gentle, so a sweep reads as a filter
    node.frequency.value = resting;  // not a resonant whistle
    applyEnvelope(node.frequency, points, startAt, offset, 20);
    return node;
  };

  const stemLow = lanes.stem_bass ? mapLane(lanes.stem_bass, stemDb(-24)) : null;
  const stemMid = lanes.stem_vocals ? mapLane(lanes.stem_vocals, stemDb(-12)) : null;
  const swept = lanes.sweep ? mapLane(lanes.sweep, sweepHz) : null;
  for (const node of [
    shelf("lowshelf", BAND_LOW_HZ, automation.low),
    shelf("peaking", BAND_MID_HZ, automation.mid),
    shelf("highshelf", BAND_HIGH_HZ, automation.high),
    sweep("lowpass", 20000, automation.lpf),
    sweep("highpass", 20, automation.hpf),
    shelf("lowshelf", BAND_LOW_HZ, lanes.low && mapLane(lanes.low, knobDb)),
    shelf("peaking", BAND_MID_HZ, lanes.mid && mapLane(lanes.mid, knobDb)),
    shelf("highshelf", BAND_HIGH_HZ, lanes.high && mapLane(lanes.high, knobDb)),
    shelf("lowshelf", BAND_LOW_HZ, stemLow),
    shelf("peaking", BAND_MID_HZ, stemMid),
    sweep("lowpass", 20000, swept && swept.map(([t, hz]) => [t, hz.lpf])),
    sweep("highpass", 20, swept && swept.map(([t, hz]) => [t, hz.hpf])),
  ]) {
    if (node) series.push(node);
  }
  if (lanes.level) {
    const level = ctx.createGain();
    applyEnvelope(level.gain, lanes.level.map(([t, v]) => [t, clamp(v, 0, 1)]), startAt, offset);
    series.push(level);
  }

  // source -> [filters...] -> [level] -> (sends) -> gain -> master. The sends
  // are post-fader and their returns join before the envelope: closing the
  // level stops feeding the echo and leaves what is in it to ring out, and
  // ducking still turns the tail down with everything else.
  gain.connect(master);
  let tail = gain;
  const echo = item.meta?.echo;
  const legacyEcho = echo && Number(echo.mix) > 0;
  if (legacyEcho) {
    const input = ctx.createGain(), dry = ctx.createGain(), wet = ctx.createGain();
    const delay = ctx.createDelay(2), feedback = ctx.createGain();
    delay.delayTime.value = clamp(Number(echo.seconds) || 0.25, 0.03, 1.8);
    feedback.gain.value = clamp(Number(echo.feedback) || 0, 0, 0.65);
    const begin = Number(echo.start) || 0, end = Number(echo.end) || item.duration;
    const level = clamp(Number(echo.mix), 0, 0.5);
    const rampEnd = begin + Math.max(0.05, (end - begin) * 0.25);
    applyEnvelope(wet.gain, [[0,0],[begin,0],[rampEnd,level],[end,level]], startAt, offset, 0);
    applyEnvelope(dry.gain, [[0,1],[begin,1],[rampEnd,1-level],[end,1-level]], startAt, offset, 0);
    input.connect(dry); dry.connect(gain);
    input.connect(delay); delay.connect(feedback); feedback.connect(delay);
    delay.connect(wet); wet.connect(gain);
    nodes.push(input, dry, wet, delay, feedback);
    tail = input;
  }
  if (lanes.echo_send || lanes.reverb_send) {
    const post = ctx.createGain();
    post.connect(tail);
    nodes.push(post);
    if (lanes.echo_send) {
      const send = ctx.createGain(), delay = ctx.createDelay(2), feedback = ctx.createGain();
      const back = ctx.createGain();
      send.gain.value = 0;
      applyEnvelope(send.gain, lanes.echo_send.map(([t, v]) => [t, clamp(v, 0, 1)]), startAt, offset, 0);
      const beat = beatSeconds(item);
      const beats = lanes.echo_beats || [[0, 0.5]];
      delay.delayTime.value = clamp(beats[0][1] * beat, 0.03, 1.9);
      applyEnvelope(delay.delayTime, beats.map(([t, v]) => [t, clamp(v * beat, 0.03, 1.9)]), startAt, offset, 0.03);
      feedback.gain.value = 0.3;
      // 1.0 would be a perfect freeze; a hair under keeps a float loop from
      // ever creeping upward.
      applyEnvelope(feedback.gain, (lanes.echo_feedback || [[0, 0.3]]).map(([t, v]) => [t, clamp(v, 0, 0.98)]),
                    startAt, offset, 0);
      back.gain.value = ECHO_RETURN;
      post.connect(send); send.connect(delay); delay.connect(feedback); feedback.connect(delay);
      delay.connect(back); back.connect(gain);
      nodes.push(send, delay, feedback, back);
    }
    if (lanes.reverb_send) {
      const send = ctx.createGain(), room = ctx.createConvolver();
      send.gain.value = 0;
      applyEnvelope(send.gain, lanes.reverb_send.map(([t, v]) => [t, clamp(v, 0, 1)]), startAt, offset, 0);
      room.buffer = impulse();
      post.connect(send); send.connect(room); room.connect(gain);
      nodes.push(send, room);
    }
    tail = post;
  }
  for (let i = series.length - 1; i >= 0; i--) {
    series[i].connect(tail);
    tail = series[i];
  }
  nodes.unshift(...series);
  return { input: tail, gain, nodes };
}

/* Rate automation from the technique, on top of the item's rate curve. The
   lane is the deck's absolute speed, as on the console. */
function applyRateLane(param, points, startAt, offset) {
  if (!points || !points.length) return;
  const forward = forwardRate(points);
  let started = false;
  for (const [time, rate] of forward) {
    const at = startAt + time - offset, value = clamp(rate, 0.0001, 4);
    if (time <= offset) continue;
    if (!started) {
      param.setValueAtTime(value, at);
      started = true;
    } else param.linearRampToValueAtTime(value, at);
  }
}

/* Rolls and loops: the record keeps playing silently underneath (slip), and
   a short looping copy of the slice at `at` plays over it until `until`.
   Tiny ramps at each edge, because a gate that steps clicks. */
function scheduleRolls(entry, buffer, events, startAt, offset) {
  const EDGE = 0.004;
  const rates = playbackCurve(entry.item);
  const sorted = [...events].sort((a, b) => a.at - b.at);
  // Back-to-back rolls hand over to each other; the record underneath stays
  // gated through the whole run rather than flashing open at every joint.
  const joined = (time) => sorted.some((other) => Math.abs(other.at - time) < 0.002);
  const joining = (time) => sorted.some((other) => Math.abs(other.until - time) < 0.002);
  for (const event of sorted) {
    if (event.at < offset + 0.02) continue;
    const at = startAt + event.at - offset, until = startAt + event.until - offset;
    const { source: position, rate } = playbackAt(rates, event.at);
    const from = (entry.item.offset || 0) + position;
    const roll = ctx.createBufferSource(), level = ctx.createGain();
    roll.buffer = buffer;
    roll.loop = true;
    roll.loopStart = from;
    roll.loopEnd = from + Math.max(0.01, event.length * rate);
    roll.playbackRate.value = rate;
    level.gain.value = 0;
    level.gain.setValueAtTime(0, at);
    level.gain.linearRampToValueAtTime(1, at + EDGE);
    level.gain.setValueAtTime(1, until - EDGE);
    level.gain.linearRampToValueAtTime(0, until);
    if (!joining(event.at)) {
      entry.gate.gain.setValueAtTime(1, at);
      entry.gate.gain.linearRampToValueAtTime(0, at + EDGE);
    }
    if (!joined(event.until)) {
      entry.gate.gain.setValueAtTime(0, until - EDGE);
      entry.gate.gain.linearRampToValueAtTime(1, until);
    }
    roll.connect(level); level.connect(entry.chain.input);
    roll.start(at, from);
    roll.stop(until + 0.01);
    roll.onended = () => { try { roll.disconnect(); level.disconnect(); } catch { /* gone */ } };
    entry.rolls.push(roll);
  }
}

function scheduleItem(item, buffer) {
  const when = clockOffset + item.start_at;
  const now = ctx.currentTime;
  let offset = 0;
  let startAt = when;

  if (when < now) {
    offset = now - when;                     // already in progress: join it
    if (offset >= item.duration - 0.15) return;
    startAt = now + 0.02;
  }

  const chain = buildChain(item, startAt, offset);
  const source = ctx.createBufferSource();
  source.buffer = buffer;
  const rates = playbackCurve(item), playback = playbackAt(rates, offset);
  source.playbackRate.setValueAtTime(playback.rate, startAt);
  for (const [time, rate] of rates) {
    if (time > offset) source.playbackRate.linearRampToValueAtTime(rate, startAt + time - offset);
  }
  const set = laneSet(item);
  applyRateLane(source.playbackRate, set.lanes.rate, startAt, offset);
  let hostAnalyser = null, gate = null;
  if (item.kind === "voice") {
    hostAnalyser = ctx.createAnalyser();
    hostAnalyser.fftSize = 512;
    source.connect(hostAnalyser); hostAnalyser.connect(chain.input);
  } else {
    // Music goes through a gate, so a roll can take over and a technique that
    // arrives after scheduling can be wired in without touching the source.
    gate = ctx.createGain();
    source.connect(gate); gate.connect(chain.input);
  }

  applyEnvelope(chain.gain.gain, itemEnvelope(item), startAt, offset);
  source.start(startAt, (item.offset || 0) + playback.source);
  source.stop(startAt + Math.max(0.01, item.duration - offset));

  // The offset it was placed with. A later drift correction moves the clock
  // for new items only; this one stays where the audio thread already has it.
  const entry = { source, gain: chain.gain, item, offset: clockOffset, hostAnalyser,
    hostSamples: hostAnalyser ? new Float32Array(hostAnalyser.fftSize) : null,
    chain, gate, buffer, lanes: set.key, rolls: [] };
  if (gate) scheduleRolls(entry, buffer, set.events, startAt, offset);

  source.onended = () => {
    const current = scheduled.get(item.id);
    if (current && current.source === source) scheduled.delete(item.id);
    try {
      entry.chain.gain.disconnect();
      hostAnalyser?.disconnect();
      gate?.disconnect();
      for (const node of entry.chain.nodes) node.disconnect();
    } catch { /* already torn down */ }
  };

  scheduled.set(item.id, entry);
}

/* The next record's technique is often planned after this one was handed
   to Web Audio. When an item's lanes change and none of them has started,
   swap in a fresh chain behind the gate: the source never stops. */
function refreshLanes() {
  const now = ctx.currentTime;
  for (const item of items) {
    const entry = scheduled.get(item.id);
    if (!entry || !entry.gate || item.kind !== "music") continue;
    const set = laneSet(item);
    if (set.key === entry.lanes) continue;
    const anchor = entry.offset ?? clockOffset;
    const offset = Math.max(0, now + 0.05 - anchor - item.start_at);
    const first = Math.min(...Object.values(set.lanes).map((p) => p[0][0]),
                           ...set.events.map((e) => e.at), Infinity);
    if (first < offset + 0.25) { entry.lanes = set.key; continue; }  // too late to rewire cleanly
    const startAt = anchor + item.start_at + offset;
    const chain = buildChain(item, startAt, offset);
    applyEnvelope(chain.gain.gain, itemEnvelope(item), startAt, offset);
    applyRateLane(entry.source.playbackRate, set.lanes.rate, startAt, offset);
    const old = entry.chain;
    entry.gate.disconnect();
    entry.gate.connect(chain.input);
    entry.chain = chain;
    entry.gain = chain.gain;
    entry.item = item;
    entry.lanes = set.key;
    scheduleRolls(entry, entry.buffer, set.events, startAt, offset);
    try {
      old.gain.disconnect();
      for (const node of old.nodes) node.disconnect();
    } catch { /* already torn down */ }
  }
}

async function pumpAudio() {
  if (!running || clockOffset === null || streamMode) return;
  const now = stationNow();
  try { refreshLanes(); } catch (error) { console.warn(error); }

  for (const item of items) {
    if (scheduled.has(item.id)) continue;
    if (item.start_at > now + SCHEDULE_AHEAD) continue;
    if (item.start_at + item.duration <= now + 0.15) continue;

    let buffer = buffers.get(item.url);
    if (!buffer || typeof buffer.then === "function") {
      // Kick off the decode; a later pump will schedule it.
      bufferFor(item.url).catch(() => {});
      continue;
    }
    try { scheduleItem(item, buffer); } catch (error) { console.warn(error); }
  }

  // Warm the decode cache further out than we schedule.
  for (const item of items) {
    if (item.start_at > now + DECODE_AHEAD) continue;
    if (!buffers.has(item.url)) bufferFor(item.url).catch(() => {});
  }
}

function refreshScheduledGains() {
  // Late-added speech must duck music already handed to Web Audio.
  for (const item of items) {
    const entry = scheduled.get(item.id);
    if (entry && JSON.stringify(itemEnvelope(entry.item)) !== JSON.stringify(itemEnvelope(item))) {
      const anchor = entry.offset ?? clockOffset;
      const offset = Math.max(0, ctx.currentTime - anchor - item.start_at);
      applyEnvelope(entry.gain.gain, itemEnvelope(item),
                    Math.max(ctx.currentTime, anchor + item.start_at), offset);
      entry.item = item;
    }
  }
}

let adRequestBusy = false;
function updateAdControls(ad = {}) {
  ui.adNext.disabled = ui.adNow.disabled = !running || adRequestBusy || !!ad.busy || ad.enabled === false;
  ui.adStatus.textContent = ad.message || (ad.enabled === false ? "Ads are disabled in games.yaml." :
    running ? "Unsponsored comedy. Play now airs after preparation and any host speech already in progress." :
    "Start playback to queue a comedy ad.");
}
async function requestAd(timing) {
  if (!running || adRequestBusy) return;
  adRequestBusy = true;
  updateAdControls({message: "Writing and voicing an ad..."});
  try {
    const result = await api("/api/ads", {method: "POST", body: JSON.stringify({timing})});
    adRequestBusy = false;
    updateAdControls(result.ad);
  } catch (error) {
    adRequestBusy = false;
    updateAdControls({message: error.message});
  }
}
ui.adNext.addEventListener("click", () => requestAd("next_break"));
ui.adNow.addEventListener("click", () => requestAd("now"));

function stopAll() {
  for (const { source, rolls } of scheduled.values()) {
    try { source.stop(); } catch { /* not started */ }
    for (const roll of rolls || []) {
      try { roll.stop(); } catch { /* not started */ }
    }
  }
  scheduled.clear();
}

/* ── Schedule ──────────────────────────────────────────────────────── */
/* Snapshots arrive two ways: the events stream when the server has one, and
   a poll otherwise (or after a skip, when we want the new timeline now). Both
   go through applySnapshot, which numbers them so a slow reply can never
   overwrite a newer one. */
let snapshotSeq = 0;         // handed out when a request is sent or an event lands
let appliedSeq = 0;          // the newest one applied
let staleEpochs = 0;
let oneWay = 0.005;          // latency estimate, seconds: half the last good RTT
const DRIFT_LIMIT = 0.25;    // seconds of clock error worth correcting
const driftSamples = [];

async function poll() {
  const seq = ++snapshotSeq;
  const sent = performance.now();
  try {
    const snapshot = await api("/api/schedule");
    const rtt = (performance.now() - sent) / 1000;
    // A slow round trip says more about a busy server than about the wire,
    // so it is too vague to measure drift with. Use it, but do not learn from it.
    const measured = rtt < 0.5 ? rtt / 2 : null;
    if (measured !== null) oneWay = measured;
    applySnapshot(snapshot, seq, measured);
  } catch (error) {
    if (seq >= appliedSeq) ui.state.textContent = "server unreachable";
    console.warn(error);
  }
}

/* `latency` is how old `snapshot.now` already is on arrival, or null when it
   is unknown -- then the snapshot can anchor a clock but not correct one. */
function applySnapshot(snapshot, seq, latency) {
  if (seq < appliedSeq) return false;
  // Epochs only go up while the server lives. An older one is a straggler --
  // unless it keeps coming, which means the server restarted and counts afresh.
  if (epoch !== null && snapshot.epoch < epoch && ++staleEpochs < 3) return false;
  staleEpochs = 0;
  appliedSeq = seq;

  // The station clock can move discontinuously -- a skip winds it forward
  // into the next transition. Without noticing, we would keep playing to
  // the old timeline and never hear the skip at all.
  const aired = stationNow();
  if (epoch !== null && snapshot.epoch !== epoch) {
    stopAll();
    clockOffset = null;
    driftSamples.length = 0;
  }
  epoch = snapshot.epoch;
  pruneLines(snapshot.items, aired);

  if (ctx || streamMode) {
    const serverNow = snapshot.now + (latency ?? oneWay);
    if (clockOffset === null) clockOffset = clockNow() - serverNow;
    else if (latency !== null && (streamMode || ctx.state === "running")) correctDrift(clockNow() - serverNow);
  }

  const known = new Set(items.map((i) => i.id));
  items = snapshot.items;
  updateAdControls(snapshot.ad);
  refreshScheduledGains();
  for (const item of items) {
    if (!known.has(item.id) && item.kind === "voice") queueLine(item);
  }
  evictBuffers();

  setText(ui.state, snapshot.status || "—");
  ui.timelineWrap.dataset.ready = items.length ? "1" : "0";
  pumpAudio();
  return true;
}

/* The Web Audio clock and the station's monotonic clock tick at slightly
   different rates, so they drift apart over a long session. Each snapshot
   measures the gap; once the median of the last few passes DRIFT_LIMIT the
   clock is re-anchored. Items already handed to Web Audio keep the offset
   they were placed with (see scheduleItem), so nothing audible jumps: the
   correction lands on the next item that has not been scheduled yet. */
function correctDrift(measuredOffset) {
  driftSamples.push(measuredOffset - clockOffset);
  if (driftSamples.length > 5) driftSamples.shift();
  const sorted = [...driftSamples].sort((a, b) => a - b);
  const drift = sorted[Math.floor(sorted.length / 2)];
  const idle = scheduled.size === 0;
  if ((sorted.length >= 3 && Math.abs(drift) > DRIFT_LIMIT) || (idle && Math.abs(driftSamples.at(-1)) > DRIFT_LIMIT)) {
    clockOffset += idle ? driftSamples.at(-1) : drift;
    driftSamples.length = 0;
  }
}

/* The audio clock stops while the context is suspended or interrupted (a
   locked phone, a Bluetooth handover, an autoplay hold) and the station's
   does not. Whatever was scheduled is now late by the length of the gap, so
   start again from where the station actually is. */
let audioState = null;
function watchAudioState() {
  audioState = ctx.state;
  ctx.onstatechange = () => {
    const was = audioState;
    audioState = ctx.state;
    if (running && audioState === "running" && (was === "suspended" || was === "interrupted")) resync();
  };
}

function resync() {
  stopAll();
  clockOffset = null;
  driftSamples.length = 0;
  poll();
}

/* ── Live updates ───────────────────────────────────────────────────── */
/* One stream instead of four polls. It is only open while playing: an open
   stream counts as a listener, and the station should be able to stop when
   nobody is listening. Any failure drops back to polling and retries later. */
const EVENTS_URL = "/api/events?topics=schedule,queue,vibe";
let events = null;
let eventsOpen = false;
let eventsRetry = 2000;
let eventsTimer = null;

function connectEvents() {
  if (!running || events || eventsTimer || typeof EventSource === "undefined") return;
  const source = new EventSource(EVENTS_URL);
  events = source;
  source.onopen = () => { if (events === source) { eventsOpen = true; eventsRetry = 2000; } };
  source.onerror = () => {
    if (events !== source) return;
    source.close();
    events = null;
    eventsOpen = false;
    if (!running) return;
    poll();                      // do not leave a gap while we wait
    eventsTimer = setTimeout(() => { eventsTimer = null; connectEvents(); }, eventsRetry);
    eventsRetry = Math.min(eventsRetry * 2, 60000);
  };
  const on = (name, handler) => source.addEventListener(name, (event) => {
    if (events !== source || !running) return;
    let data;
    try { data = JSON.parse(event.data); } catch { return; }
    handler(data);
  });
  on("schedule", (snapshot) => applySnapshot(snapshot, ++snapshotSeq, oneWay));
  on("queue", (queue) => loadQueue(queue));
  on("vibe", (data) => showVibe(data.vibe));
}

function disconnectEvents() {
  clearTimeout(eventsTimer);
  eventsTimer = null;
  events?.close();
  events = null;
  eventsOpen = false;
  eventsRetry = 2000;
}

/* ── Transcript ────────────────────────────────────────────────────── */
const pendingLines = [];

function queueLine(item) {
  if (!item.meta || !item.meta.text || seenLines.has(item.id)) return;
  remember(seenLines, item.id);
  pendingLines.push(item);
}

/* A line waiting for its moment can be cancelled before it airs -- a skip
   over a break, speech replaced by a new plan. Keep only lines the schedule
   still has, or that have already played here (`aired` is our clock before
   any jump; a hidden tab stops drawing but not playing). */
function pruneLines(scheduleItems, aired) {
  const live = new Set(scheduleItems.map((i) => i.id));
  for (let i = pendingLines.length - 1; i >= 0; i--) {
    const line = pendingLines[i];
    if (!live.has(line.id) && !(line.start_at <= aired)) pendingLines.splice(i, 1);
  }
}

let hostOrder = [];
function renderLines() {
  const now = stationNow();
  while (pendingLines.length && pendingLines[0].start_at <= now + 0.35) {
    const item = pendingLines.shift();
    if (ui.transcriptEmpty) { ui.transcriptEmpty.remove(); ui.transcriptEmpty = null; }

    const host = item.meta.host || "";
    if (!hostOrder.includes(host)) hostOrder.push(host);

    const li = document.createElement("li");
    li.className = "line" + (hostOrder.indexOf(host) % 2 ? " line--alt" : "");
    const who = document.createElement("p");
    who.className = "line__host";
    who.textContent = host;
    const what = document.createElement("p");
    what.className = "line__text";
    what.textContent = item.meta.text;
    li.append(who, what);
    const reference = item.meta.reference;
    if (reference?.kind === "article") {
      const linked = /^https?:\/\//i.test(reference.url || "");
      const source = document.createElement(linked ? "a" : "small");
      source.textContent = `Source: ${reference.source || "Pasted article"}`;
      if (linked) { source.href = reference.url; source.target = "_blank"; source.rel = "noopener noreferrer"; }
      li.append(source);
    }
    ui.transcript.append(li);

    while (ui.transcript.children.length > 60) ui.transcript.firstElementChild.remove();
    ui.transcript.scrollTop = ui.transcript.scrollHeight;
  }
}

/* ── Now playing + reporting ───────────────────────────────────────── */
function currentMusic(now) {
  let best = null;
  for (const item of items) {
    if (item.kind !== "music") continue;
    if (item.start_at <= now && now < item.start_at + item.duration) {
      if (!best || item.start_at > best.start_at) best = item;
    }
  }
  return best;
}

function talkingNow(now) {
  return items.some((i) => i.kind === "voice"
    && i.start_at - 0.2 <= now && now < i.start_at + i.duration);
}

/* The line being sung, from the station's synced lyrics, when it has them.
   stationNow() is what you hear, stream delay included. */
function updateLyric(music, now) {
  if (!window.RadioLyrics || !ui.lyric.box) return;
  window.RadioLyrics.update(ui.lyric, music, now, {
    fetcher: (path) => api(path),
    playbackAt, curve: music ? playbackCurve(music) : null,
  });
}

function updateNowPlaying() {
  const now = stationNow();
  const music = currentMusic(now);
  const talking = talkingNow(now);
  setData(ui.root, "talking", talking ? "1" : "0");
  updateLyric(music, now);

  // The autoplay warning outlives the problem otherwise: the watcher gives up
  // after a few tries, and the browser often releases audio later anyway.
  if (ctx && ctx.state === "running" && ui.hint.textContent.startsWith("Your browser")) {
    ui.hint.textContent = "";
  }

  if (!music) {
    // A dry break has no record under it. Say what is actually happening
    // rather than leaving the last title -- or worse, the cold placeholder --
    // sitting there while the hosts are mid-sentence.
    if (talking) {
      setText(ui.source, "mic open");
      setText(ui.title, hostNames || "On the mic");
      setText(ui.artist, "talking");
    } else if (running) {
      setText(ui.source, "standing by");
      setText(ui.title, "Lining up the next record");
      setText(ui.artist, "");
    }
    setVar(ui.fill, "--p", "0");
    return;
  }

  const position = now - music.start_at;
  const key = music.meta.key;

  if (key !== currentKey) {
    currentKey = key;
    ui.title.textContent = music.meta.title || "Unknown";
    ui.artist.textContent = music.meta.artist || "";
    ui.up.disabled = ui.down.disabled = false;
    remote?.nowPlaying(music.meta);
    // In stream mode the console is the player, and it does its own reporting.
    if (!streamMode && !reported.has(`start:${music.id}`)) {
      remember(reported, `start:${music.id}`);
      api("/api/report", {
        method: "POST",
        body: JSON.stringify({ kind: "started", key, item_id: music.id }),
      }).catch(() => {});
    }
  }

  setText(ui.source, talking ? "mic open" : "on air");
  setText(ui.pos, mmss(position));
  setText(ui.dur, mmss(music.duration));
  const fraction = clamp(position / music.duration, 0, 1);
  setVar(ui.fill, "--p", fraction.toFixed(3));
  const percent = String(Math.round(fraction * 100));
  if (ui.progress.getAttribute("aria-valuenow") !== percent) ui.progress.setAttribute("aria-valuenow", percent);

  // Report completion once, near the end.
  if (!streamMode && position > music.duration - 1.2 && !reported.has(`end:${music.id}`)) {
    remember(reported, `end:${music.id}`);
    api("/api/report", {
      method: "POST",
      body: JSON.stringify({
        kind: "played", key, position, duration: music.duration,
      }),
    }).catch(() => {});
  }
}

/* Match a canvas bitmap to its laid-out box.

   The box comes from a ResizeObserver rather than getBoundingClientRect, which
   forces a synchronous layout when called every frame. Rounded, and the bitmap
   only written when it actually changes: setting canvas.width/height alters
   the element's intrinsic size, so a canvas whose CSS size depends on its own
   content would grow every frame -- the CSS keeps these out of flow to prevent
   exactly that, and the rounding stops sub-pixel jitter from rewriting the
   bitmap forever. */
const canvasBoxes = new WeakMap();
const canvasObserver = typeof ResizeObserver === "function"
  ? new ResizeObserver((entries) => {
    for (const entry of entries) {
      canvasBoxes.set(entry.target, { width: entry.contentRect.width, height: entry.contentRect.height });
    }
  }) : null;

function canvasBox(canvas) {
  let box = canvasBoxes.get(canvas);
  if (!box) {
    const rect = canvas.getBoundingClientRect();
    box = { width: rect.width, height: rect.height };
    if (canvasObserver) {
      canvasBoxes.set(canvas, box);
      canvasObserver.observe(canvas);
    }
  }
  return box;
}

function fitCanvas(canvas) {
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const box = canvasBox(canvas);
  const width = Math.round(box.width), height = Math.round(box.height);
  if (width < 2 || height < 2) return null;
  const wanted = Math.round(width * dpr), high = Math.round(height * dpr);
  if (canvas.width !== wanted || canvas.height !== high) {
    canvas.width = wanted;
    canvas.height = high;
  }
  return { width, height, dpr };
}

/* ── Scope ─────────────────────────────────────────────────────────── */
const wave = { data: null, peak: 0 };

/* Meters are logarithmic, because ears are. A record mastered to -14 LUFS
   peaks around -8 dBFS, which a linear amplitude meter renders as a
   quarter-full stub that looks broken. Mapping dBFS across the scale puts a
   healthy signal near the top, where a real meter would sit. */
const METER_FLOOR_DB = -48;
function meterScale(amplitude) {
  if (amplitude <= 0.0001) return 0;
  const db = 20 * Math.log10(amplitude);
  return clamp((db - METER_FLOOR_DB) / -METER_FLOOR_DB, 0, 1);
}

/* Draw only while visible; the spectrum reads the same master bus as playback. */
function drawScope() {
  window.RadioVisualizer?.draw(ui.scope, running ? analyser : null);
  if (!analyser || document.hidden || ui.scope.parentElement?.hidden) return;
  if (!wave.data || wave.data.length !== analyser.fftSize) wave.data = new Uint8Array(analyser.fftSize);
  analyser.getByteTimeDomainData(wave.data);
  let peak = 0;
  for (const sample of wave.data) peak = Math.max(peak, Math.abs(sample - 128) / 128);
  wave.peak = Math.max(peak, wave.peak * 0.92);
  const level = meterScale(wave.peak).toFixed(2);
  setVar(ui.meterL, "--h", level);
  setVar(ui.meterR, "--h", level);
}

/* ── Timeline ──────────────────────────────────────────────────────── */
function drawTimeline() {
  const canvas = ui.timeline;
  const size = fitCanvas(canvas);
  if (!size) return;
  const { width, height, dpr } = size;
  const g = canvas.getContext("2d");
  g.setTransform(dpr, 0, 0, dpr, 0, 0);
  g.clearRect(0, 0, width, height);
  if (!items.length) return;

  const now = stationNow();
  const spanBack = 25, spanFwd = 215;          // seconds visible
  const span = spanBack + spanFwd;
  const x = (t) => ((t - (now - spanBack)) / span) * width;

  const laneTop = 22, laneH = 44;
  const talkTop = laneTop + laneH + 10, talkH = 18;

  // Minute grid
  g.font = '11px "IBM Plex Mono", monospace';
  g.textBaseline = "top";
  for (let t = Math.ceil((now - spanBack) / 30) * 30; t < now + spanFwd; t += 30) {
    const px = Math.round(x(t)) + 0.5;
    g.strokeStyle = "#ffffff0a";
    g.beginPath(); g.moveTo(px, laneTop - 8); g.lineTo(px, height - 4); g.stroke();
    g.fillStyle = "#948a78";   // --text-mute
    g.fillText(`+${Math.round(t - now)}s`, px + 4, 4);
  }

  // Music blocks
  for (const item of items) {
    if (item.kind !== "music") continue;
    const x0 = x(item.start_at), x1 = x(item.start_at + item.duration);
    if (x1 < 0 || x0 > width) continue;
    const w = Math.max(2, x1 - x0);

    g.fillStyle = "#ffb0201f";
    g.strokeStyle = "#ffb02066";
    roundRect(g, x0, laneTop, w, laneH, 4);
    g.fill(); g.lineWidth = 1; g.stroke();

    // Envelope drawn on top of the block — this is where the crossfades and
    // the duck notches actually become visible.
    g.beginPath();
    for (let i = 0; i < item.envelope.length; i++) {
      const [t, gain] = item.envelope[i];
      const px = x(item.start_at + t);
      const py = laneTop + laneH - gain * (laneH - 4) - 2;
      if (i === 0) g.moveTo(px, py); else g.lineTo(px, py);
    }
    g.strokeStyle = "#ffb020";
    g.lineWidth = 1.5;
    g.stroke();

    // Label
    const label = `${item.meta.artist || ""} — ${item.meta.title || ""}`;
    const visible = Math.min(x1, width) - Math.max(x0, 0);
    if (visible > 70) {
      g.save();
      g.beginPath(); g.rect(x0 + 6, laneTop, w - 12, laneH); g.clip();
      g.fillStyle = "#ece5d8";
      g.font = '600 11px Archivo, sans-serif';
      // Pin the label into view for a record that started off-screen left --
      // the one already playing is the one you most want named.
      g.fillText(label, Math.max(x0 + 8, 8), laneTop + 6);
      g.restore();
    }
  }

  // Talk blocks
  for (const item of items) {
    if (item.kind !== "voice") continue;
    const x0 = x(item.start_at), x1 = x(item.start_at + item.duration);
    if (x1 < 0 || x0 > width) continue;
    g.fillStyle = "#6ee7a855";
    roundRect(g, x0, talkTop, Math.max(2, x1 - x0), talkH, 3);
    g.fill();
    g.strokeStyle = "#6ee7a8"; g.lineWidth = 1; g.stroke();
  }

  // Needle
  const nx = Math.round(x(now)) + 0.5;
  g.strokeStyle = "#ff4436";
  g.lineWidth = 1.5;
  g.beginPath(); g.moveTo(nx, laneTop - 10); g.lineTo(nx, height - 2); g.stroke();
  g.fillStyle = "#ff4436";
  g.beginPath();
  g.moveTo(nx - 4, laneTop - 12); g.lineTo(nx + 4, laneTop - 12);
  g.lineTo(nx, laneTop - 6); g.closePath(); g.fill();
}

function roundRect(g, x, y, w, h, r) {
  r = Math.min(r, w / 2, h / 2);
  g.beginPath();
  g.moveTo(x + r, y);
  g.arcTo(x + w, y, x + w, y + h, r);
  g.arcTo(x + w, y + h, x, y + h, r);
  g.arcTo(x, y + h, x, y, r);
  g.arcTo(x, y, x + w, y, r);
  g.closePath();
}

/* ── Frame loop ────────────────────────────────────────────────────── */
let frameErrors = 0;

/* One bad frame must not take the display down for the rest of the session.
   requestAnimationFrame is re-queued in `finally`, so a throw anywhere above
   costs a single frame instead of permanently freezing the scope, the
   timeline and the clock. */
/* The timeline moves a few pixels a second; redrawing it at the display rate
   is wasted work. The studio samples the host mics at about 30 Hz. */
const TIMELINE_MS = 1000 / 12;
const STUDIO_MS = 1000 / 30;
let timelineAt = -Infinity, studioAt = -Infinity;

function frame(time = performance.now()) {
  try {
    if (window.LiveStudio && !document.hidden && time - studioAt >= STUDIO_MS) {
      studioAt = time;
      const levels = {mav:0, rue:0}, tones = {mav:0, rue:0}, now = stationNow();
      if (running && ctx?.state === "running") for (const entry of scheduled.values()) {
        if (!entry.hostAnalyser || now < entry.item.start_at || now >= entry.item.start_at + entry.item.duration) continue;
        entry.hostAnalyser.getFloatTimeDomainData(entry.hostSamples);
        // Level (RMS), and brightness: how much of the energy is in the
        // sample-to-sample change -- a hiss high, a vowel low. The desktop
        // booth measures its voices the same way.
        let energy = 0, change = 0, previous = 0;
        for (const x of entry.hostSamples) { energy += x * x; change += (x - previous) ** 2; previous = x; }
        const level = Math.sqrt(energy / entry.hostSamples.length), tone = energy > 1e-9 ? change / energy : 0;
        const host = entry.item.meta?.host;
        if (host in levels) { levels[host] = Math.max(levels[host], level); tones[host] = Math.max(tones[host], tone); }
      }
      const live = running && ctx?.state === "running";
      // The low band drives the studio's rain and city lights, a little.
      const energy = window.RadioVisualizer?.lowBand?.(live ? analyser : null, time) ?? 0;
      window.LiveStudio.update(levels, running ? currentMusic(now) : null, energy, running, tones);
    }
    if (running) {
      updateNowPlaying();
      renderLines();
      if (time - timelineAt >= TIMELINE_MS) {
        timelineAt = time;
        drawTimeline();
      }
      setText(ui.uptime, hhmmss((performance.now() - startedAt) / 1000));
    }
    drawScope();
    const now = new Date();
    setText(ui.wallclock,
      `${String(now.getHours()).padStart(2, "0")}:${String(now.getMinutes()).padStart(2, "0")}`);
  } catch (error) {
    // Log the first few and then stay quiet, so a persistent fault does not
    // flood the console at sixty a second.
    if (frameErrors++ < 5) console.error("frame failed:", error);
  } finally {
    requestAnimationFrame(frame);
  }
}

/* ── Volume ────────────────────────────────────────────────────────── */
/* Kept in this browser rather than on the server. Turning it down here is a
   listening decision, not a station setting, and it should not follow you to
   another device or survive as the station's idea of "correct". */
const VOLUME_KEY = "sideroom.volume";
const MUTE_KEY = "sideroom.muted";

function readStored(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? fallback : JSON.parse(raw);
  } catch { return fallback; }
}

function store(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* private mode */ }
}

function applyVolume() {
  const level = muted ? 0 : volume;
  remote?.setVolume(volume, muted);
  if (master) {
    // A short ramp, not a jump -- stepping a gain produces a click.
    master.gain.cancelScheduledValues(ctx.currentTime);
    master.gain.setTargetAtTime(Math.max(level, 0.0001), ctx.currentTime, 0.015);
  }
  const percent = Math.round(volume * 100);
  ui.volume.value = String(percent);
  ui.volume.style.setProperty("--pct", `${percent}%`);
  ui.volume.setAttribute("aria-valuetext",
    muted ? "muted" : `${percent} percent`);
  ui.mute.setAttribute("aria-pressed", String(muted));
  ui.mute.setAttribute("aria-label", muted ? "Unmute" : "Mute");
  ui.volIcon.setAttribute("href", muted ? "#i-mute" : "#i-vol");
  ui.root.dataset.muted = muted ? "1" : "0";
}

function setVolume(next, { unmute = true } = {}) {
  volume = clamp(next, 0, 1);
  if (unmute && muted && volume > 0) muted = false;
  store(VOLUME_KEY, volume);
  store(MUTE_KEY, muted);
  applyVolume();
}

ui.volume.addEventListener("input", () => {
  setVolume(Number(ui.volume.value) / 100);
});

ui.mute.addEventListener("click", () => {
  muted = !muted;
  store(MUTE_KEY, muted);
  applyVolume();
});

/* Keyboard, but never while someone is typing a request, working a control
   that owns the arrow keys (a select, a fader, a menu), or reading a dialog. */
function ownsKeys(target) {
  if (!target || typeof target.closest !== "function") return false;
  return Boolean(target.closest(
    "input, textarea, select, [contenteditable]:not([contenteditable='false']), dialog, [role='listbox'], [role='menu'], [role='slider']"));
}

document.addEventListener("keydown", (event) => {
  if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
  if (ownsKeys(event.target)) return;

  if (event.key === "ArrowUp") { setVolume(volume + 0.05); event.preventDefault(); }
  else if (event.key === "ArrowDown") { setVolume(volume - 0.05); event.preventDefault(); }
  else if (event.key.toLowerCase() === "m") { ui.mute.click(); }
});

/* ── Controls ──────────────────────────────────────────────────────── */
async function start() {
  // Nothing plays until the first-run notice has been accepted here.
  if (globalThis.DefaltLegal && !globalThis.DefaltLegal.accepted()) {
    globalThis.DefaltLegal.show();
    return;
  }
  if (streamMode) {
    // The console mixes; this only plays what it sends. Called inside the
    // tap, which is what lets a phone start audio at all.
    remote?.play();
  } else {
    if (!ctx) { buildGraph(); watchAudioState(); }

    // Never await resume(). Under an autoplay policy the promise can stay
    // pending indefinitely, and awaiting it would strand the whole start path
    // -- the station would look dead while the server was already running.
    ctx.resume().catch(() => {});
  }

  running = true;
  startedAt = performance.now();
  clockOffset = null;
  ui.root.dataset.state = "live";
  ui.powerLabel.textContent = "Stop";
  ui.power.classList.remove("btn--primary");
  ui.skip.disabled = false;
  ui.hint.textContent = streamMode
    ? "Streaming from the console at home. It runs a few seconds behind; the words follow the sound."
    : "Warming up — the first record has to download before it can play.";

  await poll();
  connectEvents();
  if (!streamMode) watchAudioPermission();
}

/* Chrome will not let a page make noise until it trusts the gesture. If the
   context is still suspended shortly after starting, say so plainly rather
   than leaving a silent page that looks broken. */
function watchAudioPermission() {
  let tries = 0;
  const check = () => {
    if (!running || !ctx) return;
    if (ctx.state === "running") {
      ui.hint.textContent = "";
      return;
    }
    ctx.resume().catch(() => {});
    if (++tries > 3) {
      ui.hint.textContent =
        "Your browser is holding audio back. Click anywhere on the page to release it.";
      document.addEventListener("pointerdown", () => {
        ctx.resume().catch(() => {});
      }, { once: true });
      return;
    }
    setTimeout(check, 700);
  };
  setTimeout(check, 400);
}

function stop() {
  running = false;
  remote?.stop();
  updateAdControls();
  disconnectEvents();
  stopAll();
  buffers.clear();             // the decoded audio is most of this page's memory
  if (ctx) ctx.suspend();
  ui.root.dataset.state = "idle";
  ui.powerLabel.textContent = "Start the station";
  ui.power.classList.add("btn--primary");
  ui.skip.disabled = true;
  ui.up.disabled = ui.down.disabled = true;
  ui.source.textContent = "off air";
  ui.hint.textContent = "The station holds its place while you're away.";
  if (ui.lyric.box) ui.lyric.box.hidden = true;
}

ui.power.addEventListener("click", () => (running ? stop() : start()));

ui.skip.addEventListener("click", async () => {
  ui.skip.disabled = true;
  try {
    const result = await api("/api/skip", { method: "POST" });
    // Do not clear `items` -- the schedule is still valid, we have simply
    // moved along it. Dropping it would throw away the transition we are
    // skipping into.
    if (result.mode === "transition" && result.skipped > 0) {
      stopAll();
      clockOffset = null;
      currentKey = null;
    }
    await poll();
    pumpAudio();
    loadQueue();
    // The queue we just fetched was computed mid-transition, so the record we
    // skipped into still reads "on deck". Refresh once the mix has landed
    // rather than leaving a stale label until the next 15s tick.
    setTimeout(() => { if (running) loadQueue(); }, 6000);
    toast({
      speaking: "Letting the hosts finish, then moving to the transition.",
      transition: `Into the mix${result.into ? ` — ${result.into}` : ""}`,
      already_mixing: "Already mixing into the next one",
      preparing: "Preparing the next transition; keeping this song playing",
      empty: "Waiting for the next song",
      cut: "Skipped — they'll pick it up from here",
    }[result.mode] || "Skipped");
  } catch (error) {
    toast(error.message, "bad");
  } finally {
    ui.skip.disabled = !running;
  }
});

for (const [button, value] of [[ui.up, "up"], [ui.down, "down"]]) {
  button.addEventListener("click", async () => {
    if (!currentKey) return;
    try {
      await api("/api/rate", {
        method: "POST",
        body: JSON.stringify({ key: currentKey, value }),
      });
      toast(value === "up" ? "Noted — more like this." : "Noted — less like this.");
    } catch (error) {
      toast(error.message, "bad");
    }
  });
}

/* The box takes more than song titles, so the reply has to say what it
   understood. A misread request is otherwise invisible until something odd
   turns up on air twenty minutes later. */
const KIND_LABEL = {
  track: "song", artist: "artist", similar: "similar", genre: "genre",
  topic: "topic", article: "article", segment: "segment", directive: "play less", vibe: "vibe", clear_vibe: "vibe",
};

let spotifySelection = null, spotifyTimer = null, spotifyRevision = 0;
function clearSpotify() {
  ++spotifyRevision;
  clearTimeout(spotifyTimer);
  spotifySelection = null;
  ui.spotifyResults.replaceChildren();
  ui.spotifyResults.hidden = true;
  ui.spotifyNote.textContent = "";
}

ui.input.addEventListener("input", () => {
  clearSpotify();
  const query = ui.input.value.trim(), revision = spotifyRevision;
  if (ui.requestMode.value !== "request" || query.length < 2 || /https?:\/\/|youtube\.com\/|youtu\.be\//i.test(query)) return;
  spotifyTimer = setTimeout(async () => {
    ui.spotifyNote.textContent = "Searching Spotify…";
    try {
      const result = await api(`/api/spotify/search?q=${encodeURIComponent(query)}`);
      if (revision !== spotifyRevision) return;
      const tracks = result.tracks || [];
      ui.spotifyNote.textContent = tracks.length ? "Choose a Spotify result, then send your request." : "No Spotify matches. You can still send your request.";
      for (const track of tracks) {
        const row = document.createElement("li"), button = document.createElement("button");
        button.type = "button";
        const title = document.createElement("strong"), detail = document.createElement("span");
        title.textContent = track.title;
        detail.textContent = `${track.artist} · ${mmss(track.duration_ms / 1000)}${track.year ? " · " + track.year : ""}`;
        button.title = track.album || track.title;
        button.append(title, detail);
        button.addEventListener("click", () => {
          clearSpotify();
          ui.input.value = `${track.artist} - ${track.title}`;
          spotifySelection = track;
          ui.spotifyNote.textContent = `Spotify selection · ${mmss(track.duration_ms / 1000)}`;
          ui.input.focus();
        });
        row.append(button);
        ui.spotifyResults.append(row);
      }
      ui.spotifyResults.hidden = tracks.length === 0;
    } catch (error) {
      if (revision !== spotifyRevision) return;
      ui.spotifyNote.textContent = error.payload?.message || error.message || "Spotify search is unavailable. Typed requests still work.";
    }
  }, 280);
});

ui.form.addEventListener("submit", async (event) => {
  event.preventDefault();
  const mode = ui.requestMode.value;
  const input = mode === "article" ? ui.articleInput : ui.input;
  if (input.disabled) return;
  const query = input.value.trim();
  if (!query) return;

  ui.note.textContent = "Working out what you meant…";
  ui.note.dataset.tone = "";
  input.disabled = true;
  ui.requestMode.disabled = true;
  if (mode === "article") ui.note.textContent = "Sending article…";

  let payload;
  try {
    payload = await api("/api/request", {
      method: "POST", body: JSON.stringify({ query, mode,
        selection: mode === "request" && spotifySelection
          && query === `${spotifySelection.artist} - ${spotifySelection.title}` ? spotifySelection : null }),
    });
  } catch (error) {
    // A refusal comes back as a 400 with the reason already written for a
    // person, so show it as-is rather than wrapping it in our own wording.
    payload = error.payload && error.payload.message
      ? { ...error.payload, ok: false }
      : { ok: false, message: error.message };
  } finally {
    input.disabled = false;
    ui.requestMode.disabled = false;
  }

  const kind = payload.kind || (payload.intent && payload.intent.kind);
  ui.note.textContent = (payload.ok && kind && KIND_LABEL[kind])
    ? `${KIND_LABEL[kind]} — ${payload.message}`
    : payload.message || "something went wrong";
  ui.note.dataset.tone = payload.ok ? "good" : "bad";

  if (payload.ok) {
    clearSpotify();
    input.value = "";
    input.focus();
  }
  loadQueue();
  loadVibe();
});

ui.requestMode.addEventListener("change", () => {
  clearSpotify();
  const vibeMode = ui.requestMode.value === "vibe";
  const articleMode = ui.requestMode.value === "article";
  ui.input.hidden = articleMode;
  ui.articleInput.hidden = !articleMode;
  ui.requestPrompt.textContent = vibeMode ? "What is the mood, or what are you doing?" : "What would you like to hear?";
  ui.input.placeholder = vibeMode ? "Studying, calm and jazzy. No heavy metal." : "A song, YouTube link, genre, or topic…";
  ui.note.textContent = vibeMode ? "Stays on until changed or cleared. Planned mixes finish first; song requests keep priority." : "Ask for a song, artist, genre, or something for the hosts to discuss.";
  ui.note.dataset.tone = "";
  if (articleMode) {
    ui.requestPrompt.textContent = "Article link or pasted text";
    ui.note.textContent = "The director writes a sourced news break. Already planned breaks finish first. Up to 24,000 characters.";
  }
  ui.requestPrompt.htmlFor = articleMode ? "article-input" : "request-input";
});

function showVibe(vibe) {
  ui.vibePanel.hidden = !vibe?.description;
  ui.vibeDescription.textContent = vibe?.description || "";
}

async function loadVibe() {
  try { showVibe((await api("/api/vibe")).vibe); } catch { /* keep the last known brief */ }
}

ui.vibeClear.addEventListener("click", async () => {
  ui.vibeClear.disabled = true;
  try {
    const result = await api("/api/vibe/clear", { method: "POST" });
    showVibe(result.vibe);
    ui.note.textContent = result.message;
    ui.note.dataset.tone = "good";
  } catch (error) {
    ui.note.textContent = error.message;
    ui.note.dataset.tone = "bad";
  } finally { ui.vibeClear.disabled = false; }
});

/* ── The queue ─────────────────────────────────────────────────────── */
const STAGE_LABEL = { on_deck: "on deck", queued: "queued", finding: "finding", article: "article", failed: "failed" };

function iconButton(symbol, label, handler) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "queue__act";
  button.setAttribute("aria-label", label);
  button.title = label;
  button.innerHTML =
    `<svg class="icon icon--xs" viewBox="0 0 24 24"><use href="#${symbol}"/></svg>`;
  button.addEventListener("click", async () => {
    button.disabled = true;
    await handler();
    loadQueue();
  });
  return button;
}

async function queueAction(id, action) {
  try {
    await api(`/api/queue/${encodeURIComponent(id)}/${action}`, { method: "POST" });
  } catch (error) {
    toast(error.message, "bad");
  }
}

let queueBusy = false, queueAgain = null, articleFetching = false;

/* `pushed` is a queue the events stream already delivered; without it the
   queue is fetched. Either way the open wishes come from /api/requests. A
   call that lands while one is in flight runs once more afterwards, with the
   newest pushed queue, so a change is never lost behind a slow request. */
async function loadQueue(pushed) {
  if (queueBusy) {
    queueAgain = pushed || queueAgain || true;
    return;
  }
  queueBusy = true;
  let data, wishes = [];
  try {
    data = pushed && typeof pushed === "object" ? pushed : await api("/api/queue");
    wishes = (await api("/api/requests")).wishes || [];
  } catch {
    return;
  } finally {
    queueBusy = false;
    if (queueAgain) {
      const again = queueAgain;
      queueAgain = null;
      setTimeout(() => loadQueue(again === true ? undefined : again), 0);
    }
  }

  const rows = data.items || [];
  articleFetching = rows.some(row => row.stage === "article" && row.status === "preparing");
  const movable = rows.filter((r) => r.stage === "queued");

  ui.queue.replaceChildren();

  // Topics and forced segments sit above the music -- they change the next
  // break rather than the running order.
  for (const wish of wishes) {
    if (wish.kind === "article") continue; // Included in the shared queue, including fetch failures.
    if (!["pending", "active"].includes(wish.status)) continue;
    const li = document.createElement("li");
    li.className = "queue__item queue__item--wish";
    li.append(tagged(KIND_LABEL[wish.kind] || wish.kind, "wish"),
              labelled(wish.subject || wish.raw));
    if (wish.timing === "hour") li.append(noted("top of hour"));
    li.append(iconButton("i-cross", `Cancel: ${wish.subject || wish.raw}`,
      () => api(`/api/requests/${wish.id}/cancel`, { method: "POST" })
              .catch(() => {})));
    ui.queue.append(li);
  }

  for (const row of rows) {
    const name = row.title
      ? `${row.artist && row.artist !== "unknown" ? row.artist + " — " : ""}${row.title}`
      : "(unknown)";

    const li = document.createElement("li");
    li.className = "queue__item";
    li.dataset.stage = row.stage;
    if (row.playing) li.dataset.playing = "1";

    li.append(tagged(row.playing ? "on air" : STAGE_LABEL[row.stage], row.stage),
              labelled(name));
    if (row.selection_origin?.by === "listener") li.append(noted("you queued"));
    if (row.selection_origin?.by === "director") li.append(noted("auto pick"));
    // How it comes in: the technique the station planned for this mix.
    const technique = row.transition?.technique || row.transition?.preset;
    if (!row.playing && technique) li.append(noted(`into: ${String(technique).replaceAll("_", " ")}`));
    if (row.note) {
      li.title = row.note;
      li.append(noted(row.note));
    }

    if (row.playing) {
      li.append(noted("now"));
    } else if (row.eta !== null && row.eta !== undefined) {
      li.append(noted(`in ${mmss(row.eta)}`));
    } else if (row.stage === "finding") {
      li.append(noted("…"));
    }

    const actions = document.createElement("span");
    actions.className = "queue__actions";

    if (row.can_move) {
      const index = movable.indexOf(row);
      if (index > 0) {
        actions.append(iconButton("i-top", `Play next: ${name}`,
                                  () => queueAction(row.id, "next")));
        actions.append(iconButton("i-up-arrow", `Move up: ${name}`,
                                  () => queueAction(row.id, "up")));
      }
      if (index < movable.length - 1) {
        actions.append(iconButton("i-down-arrow", `Move down: ${name}`,
                                  () => queueAction(row.id, "down")));
      }
    }
    if (row.can_remove) {
      actions.append(iconButton("i-cross", `Remove: ${name}`,
                                () => queueAction(row.id, "remove")));
    }
    if (actions.childElementCount) li.append(actions);
    ui.queue.append(li);
  }

  const count = rows.filter((r) => !r.playing).length
    + wishes.filter(w => w.kind !== "article" && ["pending", "active"].includes(w.status)).length;
  ui.lineupCount.textContent = count ? String(count) : "";
  ui.lineupEmpty.hidden = count > 0;
  ui.queueClear.disabled = !movable.length;
}

function tagged(text, stage) {
  const span = document.createElement("span");
  span.className = "queue__kind";
  span.dataset.stage = stage || "";
  span.textContent = text;
  return span;
}

function labelled(text) {
  const span = document.createElement("span");
  span.className = "queue__label";
  span.textContent = text;
  return span;
}

function noted(text) {
  const span = document.createElement("span");
  span.className = "queue__note tnum";
  span.textContent = text;
  return span;
}

/* ── Board ─────────────────────────────────────────────────────────── */
const FADER_META = {
  "crossfade.duration":                ["Crossfade", "Overlap between two records, in seconds.", 1],
  "ducking.target_gain":               ["Duck depth", "How far music drops under a voice.", 2],
  "ducking.attack":                    ["Duck attack", "How fast it drops. Fast reads professional.", 2],
  "ducking.release":                   ["Duck release", "How slowly the music climbs back.", 2],
  "talk_placement.assumed_intro":      ["Assumed intro", "Fallback talk-over length when a track hasn't been measured.", 1],
  "talk_placement.post_safety_margin": ["Post margin", "Silence left before the vocal lands.", 2],
  "selection.exploration_rate":        ["Exploration", "Share of records outside your profile.", 2],
  "clock.segment_weights.banter":      ["Banter", "How often they just riff.", 0],
  "clock.segment_weights.track_intro": ["Intros", "How often they introduce the record.", 0],
  "clock.segment_weights.news":        ["News", "How often a news break airs.", 0],
  "clock.segment_weights.patch_notes": ["Patch notes", "How often they cover a game update.", 0],
  "clock.segment_weights.game_ad":     ["Ad reads", "How often they perform a fake advert.", 0],
};

async function buildBoard() {
  let config;
  try { config = await api("/api/config"); } catch { return; }

  // Walk FADER_META, not the response: Flask sorts JSON keys alphabetically,
  // which would scatter the ducking controls between News and Patch notes.
  // The declaration order above is the order these belong in on a board.
  for (const [key, meta] of Object.entries(FADER_META)) {
    const bounds = config.bounds[key];
    if (!bounds) continue;
    const value = config.values[key];
    const [name, hint, decimals] = meta;

    const wrap = document.createElement("div");
    wrap.className = "fader";

    const top = document.createElement("div");
    top.className = "fader__top";
    const label = document.createElement("label");
    label.className = "fader__name";
    label.textContent = name;
    label.htmlFor = `f-${key}`;
    const readout = document.createElement("span");
    readout.className = "fader__value tnum";
    top.append(label, readout);

    const input = document.createElement("input");
    input.type = "range";
    input.id = `f-${key}`;
    input.min = bounds.min;
    input.max = bounds.max;
    input.step = decimals === 0 ? 1 : (decimals === 1 ? 0.5 : 0.01);
    input.value = value ?? bounds.min;

    const note = document.createElement("p");
    note.className = "fader__hint";
    note.textContent = hint;

    const paint = () => {
      readout.textContent = Number(input.value).toFixed(decimals);
      const pct = ((input.value - bounds.min) / (bounds.max - bounds.min)) * 100;
      input.style.setProperty("--pct", `${pct}%`);
    };
    paint();

    let timer = null;
    input.addEventListener("input", () => {
      paint();
      clearTimeout(timer);
      timer = setTimeout(() => {
        api("/api/config", {
          method: "POST",
          body: JSON.stringify({ key, value: Number(input.value) }),
        }).catch((error) => toast(error.message, "bad"));
      }, 400);
    });

    wrap.append(top, input, note);
    ui.faders.append(wrap);
  }
}

const TRANSITION_HELP = {
  auto:  "Chosen per pair from tempo, key and energy.",
  fade:  "Equal-power crossfade, bass swaps at the midpoint.",
  rise:  "Overlap, bass swap at the end, low-pass in and high-pass out.",
  blend: "Three-band fade — highs hand over first, lows last.",
  wave:  "Overlap with a bass swap, low-passed through the middle.",
  melt:  "Fade across, high-passed both sides so it thins out.",
  slam:  "A hard centred swap. No blend at all.",
  echo_out: "The old record's last beat echoes away over the new drop.",
  loop_roll: "The old record stutters in shrinking loops into the drop.",
  brake: "The old record slows to a stop; the new one lands on the one.",
  spinback: "The old record spins backwards out (a brake in this browser).",
  echo_freeze: "The last beat freezes in the echo and is filtered away.",
  reverb_wash: "The old record dissolves into reverb as the new one emerges.",
  stem_swap: "New drums and bass under the old vocals, then the vocals hand over. Needs separated records.",
  acapella_intro: "The new vocal over the old instrumental. Needs separated records.",
  filter_ride: "A long filter ride with the bass swapped on the bar.",
  silence_punch: "A beat of silence right before the drop.",
  drop_swap: "A hard cut on the downbeat, the old bass already gone.",
};

async function buildTransitionPicker() {
  let current = "auto";
  try {
    current = (await api("/api/transition")).preset || "auto";
  } catch { return; }

  const wrap = document.createElement("div");
  wrap.className = "fader fader--wide";

  const top = document.createElement("div");
  top.className = "fader__top";
  const label = document.createElement("label");
  label.className = "fader__name";
  label.textContent = "Transition";
  label.htmlFor = "f-transition";
  top.append(label);

  const select = document.createElement("select");
  select.className = "picker";
  select.id = "f-transition";
  for (const name of Object.keys(TRANSITION_HELP)) {
    const option = document.createElement("option");
    option.value = name;
    option.textContent = name.replaceAll("_", " ");
    if (name === current) option.selected = true;
    select.append(option);
  }

  const note = document.createElement("p");
  note.className = "fader__hint";
  note.textContent = TRANSITION_HELP[current];

  select.addEventListener("change", async () => {
    note.textContent = TRANSITION_HELP[select.value] || "";
    try {
      // The mix settings schema accepts the techniques as well as the presets.
      await api("/api/mix/config", {
        method: "POST", body: JSON.stringify({ "transitions.preset": select.value }),
      });
      toast(`Transitions: ${select.value.replaceAll("_", " ")}`);
    } catch (error) {
      toast(error.message, "bad");
    }
  });

  wrap.append(top, select, note);
  ui.faders.prepend(wrap);
}

/* How often the station reaches for an effect instead of a plain blend.
   Read from the full status's mix settings schema, saved through it too. */
function buildCreativity(status) {
  const field = (status?.mix_config?.fields || []).find((f) => f.key === "transitions.creativity");
  if (!field || !Array.isArray(field.bounds)) return;
  const [min, max] = field.bounds;
  const wrap = document.createElement("div");
  wrap.className = "fader";
  const top = document.createElement("div");
  top.className = "fader__top";
  const label = document.createElement("label");
  label.className = "fader__name";
  label.textContent = "Creativity";
  label.htmlFor = "f-creativity";
  const readout = document.createElement("span");
  readout.className = "fader__value tnum";
  top.append(label, readout);
  const input = document.createElement("input");
  input.type = "range";
  input.id = "f-creativity";
  input.min = min;
  input.max = max;
  input.step = 0.05;
  input.value = Number(field.value ?? 0.5);
  const note = document.createElement("p");
  note.className = "fader__hint";
  note.textContent = "0 is smooth radio; 1 is a show-off DJ. Hosts talking always get a clean blend.";
  const paint = () => {
    readout.textContent = `${Math.round(Number(input.value) * 100)}%`;
    input.style.setProperty("--pct", `${((input.value - min) / (max - min)) * 100}%`);
  };
  paint();
  let timer = null;
  input.addEventListener("input", () => {
    paint();
    clearTimeout(timer);
    timer = setTimeout(() => {
      api("/api/mix/config", {
        method: "POST", body: JSON.stringify({ "transitions.creativity": Number(input.value) }),
      }).catch((error) => toast(error.message, "bad"));
    }, 400);
  });
  wrap.append(top, input, note);
  ui.faders.append(wrap);
}

ui.queueClear.addEventListener("click", async () => {
  ui.queueClear.disabled = true;
  try {
    const result = await api("/api/queue/clear", {
      method: "POST", body: JSON.stringify({}),
    });
    toast(result.dropped ? `Cleared ${result.dropped} from the queue`
                         : "Queue was already empty");
  } catch (error) {
    toast(error.message, "bad");
  }
  loadQueue();
});

ui.panelToggle.addEventListener("click", () => {
  const open = ui.panelToggle.getAttribute("aria-expanded") === "true";
  ui.panelToggle.setAttribute("aria-expanded", String(!open));
  ui.panelBody.hidden = open;
});

/* ── Boot ──────────────────────────────────────────────────────────── */
async function boot() {
  volume = clamp(Number(readStored(VOLUME_KEY, 0.9)) || 0, 0, 1);
  muted = Boolean(readStored(MUTE_KEY, false));
  setUpStream();
  applyVolume();

  // Asked for once: the hosts, the writing backend and the station's name all
  // come from the same full status (the lite one leaves the personas out).
  let status = null;
  try { status = await api("/api/status"); } catch { /* no station yet */ }
  if (status) {
    if (status.hosts && status.hosts.length) {
      const names = status.hosts.map((h) => h.name);
      ui.hostPill.textContent = names.join(" · ");
      hostNames = names.length === 2 ? names.join(" and ") : names.join(", ");
    }
    if (status.llm && !status.llm.configured) {
      toast("No writing backend configured — the hosts will use fallback lines.", "bad");
    }
  }

  buildBoard().then(buildTransitionPicker).then(() => buildCreativity(status));
  loadVibe();
  loadQueue();
  // Polling is the fallback for the events stream, not the main path: each
  // timer stands down while the stream is open, and nothing polls for a
  // hidden or stopped page that has no use for the answer.
  setInterval(() => { if (running && !eventsOpen && !document.hidden) loadVibe(); }, 4000);
  setInterval(() => { if (!eventsOpen && (running || articleFetching)) loadQueue(); }, 5000);
  setInterval(() => { if (running && !eventsOpen) poll(); }, POLL_MS);
  setInterval(() => { if (running) pumpAudio(); }, 900);

  // The station clock stops when it stops hearing from a listener. The
  // schedule poll normally does that, but it can be slow while tracks are
  // downloading -- so keep a cheap heartbeat on its own timer. Losing the
  // clock mid-record would put a hole in the broadcast. An open events
  // stream already counts as a listener.
  //
  // A hidden tab throttles this to about once a minute, which is why the
  // server allows a generous window before it decides nobody is there.
  setInterval(() => {
    if (running && !eventsOpen) fetch("/api/heartbeat", { method: "POST" }).catch(() => {});
  }, 8000);

  // Coming back to the tab should catch up immediately rather than waiting
  // for the next throttled tick.
  document.addEventListener("visibilitychange", () => {
    if (document.hidden || !running) return;
    if (!eventsOpen) {
      fetch("/api/heartbeat", { method: "POST" }).catch(() => {});
      poll();
    }
    loadVibe();
  });
  requestAnimationFrame(frame);
  nameTheStation(status);
}

/* ── Stream mode ───────────────────────────────────────────────────── */
/* Chosen once at boot: the stream away from home or as an installed app, the
   browser mixer at home. The toggle switches, and remembers, either way. */
function setUpStream() {
  const RS = window.RemoteStream;
  const audio = el("stream-audio");
  if (!RS || !audio) return;
  const standalone = window.matchMedia?.("(display-mode: standalone)")?.matches || navigator.standalone === true;
  const mode = RS.chooseMode({ hostname: location.hostname, standalone, search: location.search,
                               stored: readStored(RS.MODE_KEY, null) });
  const note = el("stream-note");
  remote = RS.create({
    audio,
    fetchStatus: () => api("/api/remote/status"),
    onNote: (text) => { if (note) note.textContent = text; },
    onState: (state) => setData(ui.root, "stream", state),
    mediaSession: navigator.mediaSession || null,
  });
  remote.bindControls({
    onPlay: () => { if (running) remote.resume(); else start(); },
    onPause: () => { if (running) stop(); },
    onNext: () => { if (running) ui.skip.click(); },
  });
  setInterval(() => remote.watch(), 1000);
  applyMode(mode);
  // The home-screen app's shell. Only where a worker is allowed (https, or
  // loopback); the page works the same without one.
  if ("serviceWorker" in navigator && window.isSecureContext) {
    navigator.serviceWorker.register("/sw.js").catch(() => {});
  }
  el("mode-toggle")?.addEventListener("click", () => {
    const next = streamMode ? "mixer" : "stream";
    store(RS.MODE_KEY, next);
    const wasRunning = running;
    if (wasRunning) stop();
    applyMode(next);
    if (wasRunning) start();
  });
  buildRemotePanel();
}

function applyMode(mode) {
  streamMode = mode === "stream";
  // Items scheduled on one clock mean nothing on the other.
  clockOffset = null;
  driftSamples.length = 0;
  ui.root.dataset.mode = streamMode ? "stream" : "mixer";
  const toggle = el("mode-toggle");
  if (toggle) {
    toggle.textContent = streamMode ? "Stream" : "Mix here";
    toggle.setAttribute("aria-pressed", String(streamMode));
    toggle.title = streamMode
      ? "Playing the console's own output. Tap to mix in this browser instead."
      : "Mixing in this browser. Tap to play the console's stream instead.";
  }
}

/* Remote listening settings (station.yaml remote.*) and what the tunnel and
   the stream are doing. */
async function buildRemotePanel() {
  const status = el("remote-status"), enabled = el("remote-enabled"), mute = el("remote-mute");
  if (!status || !enabled || !mute) return;
  const show = (data) => {
    enabled.checked = !!data.enabled;
    mute.checked = !!data.mute_local;
    const tunnel = data.tunnel?.state || "off";
    const listeners = data.broadcast?.listeners ?? 0;
    status.textContent = !data.console_running
      ? "The console isn't running, so there is nothing to stream."
      : `Tunnel ${tunnel}${data.tunnel?.note ? ` (${data.tunnel.note})` : ""} · ${listeners} listening`;
  };
  const refresh = async () => { try { show(await api("/api/remote/status")); } catch { /* keep the last */ } };
  const save = async (body) => {
    try { show({ ...(await api("/api/remote/status")), ...(await api("/api/remote/config", { method: "POST", body: JSON.stringify(body) })) }); }
    catch (error) { toast(error.message, "bad"); }
  };
  enabled.addEventListener("change", () => save({ enabled: enabled.checked }));
  mute.addEventListener("change", () => save({ mute_local: mute.checked }));
  await refresh();
  setInterval(() => { if (!document.hidden && ui.panelBody && !ui.panelBody.hidden) refresh(); }, 10000);
}

/* The page used to be rendered by the station, so the name and tagline came
   with it. Now it is served by the app and has to ask -- boot already did. */
function nameTheStation(status) {
  if (!status) {
    // No station. The console still runs; the radio half simply is not there.
    el("ident-tag").textContent = "";
    return;
  }
  const identity = status.identity || {};
  if (identity.name) {
    el("ident-name").textContent = identity.name;
    document.title = identity.name;
  }
  if (identity.tagline) el("ident-tag").textContent = identity.tagline;
}

boot();
