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
  scope: el("scope"), meterL: el("meter-l"), meterR: el("meter-r"),
  progress: el("progress"), fill: el("progress-fill"),
  pos: el("pos"), dur: el("dur"),
  power: el("power"), powerLabel: el("power-label"), skip: el("skip"),
  volume: el("volume"), mute: el("mute"), volIcon: el("vol-icon"),
  up: el("rate-up"), down: el("rate-down"), hint: el("hint"),
  transcript: el("transcript"), transcriptEmpty: el("transcript-empty"),
  hostPill: el("host-pill"),
  form: el("request-form"), input: el("request-input"), note: el("request-note"),
  requestMode: el("request-mode"), requestPrompt: el("request-prompt"),
  spotifyNote: el("spotify-note"), spotifyResults: el("spotify-results"),
  vibePanel: el("vibe-panel"), vibeDescription: el("vibe-description"), vibeClear: el("vibe-clear"),
  queue: el("queue"), lineupCount: el("lineup-count"),
  lineupEmpty: el("lineup-empty"), queueClear: el("queue-clear"),
  timeline: el("timeline"), timelineWrap: document.querySelector(".timeline"),
  faders: el("faders"), panelToggle: el("panel-toggle"), panelBody: el("panel-body"),
  toast: el("toast"),
};

/* ── State ─────────────────────────────────────────────────────────── */
let ctx = null;
let master = null;
let analyser = null;
let running = false;

let clockOffset = null;      // audioCtx.currentTime - stationTime
let items = [];              // everything the server has told us about
const scheduled = new Map(); // item.id -> { source, gain, item }
const buffers = new Map();   // url -> AudioBuffer | Promise
const seenLines = new Set();
const reported = new Set();
let currentKey = null;
let startedAt = 0;
let hostNames = "";
let epoch = null;          // server bumps this when the clock jumps
let volume = 0.9;
let muted = false;

/* ── Small helpers ─────────────────────────────────────────────────── */
const clamp = (n, lo, hi) => Math.min(Math.max(n, lo), hi);

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

/* Station time, as the server counts it. */
const stationNow = () => (clockOffset === null ? 0 : ctx.currentTime - clockOffset);

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
    buffers.set(url, buffer);
    return buffer;
  } catch (error) {
    buffers.delete(url);
    throw error;
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

/* Build only the nodes a transition actually needs. Most records get a plain
   gain node; a filtered transition adds shelves and sweeps for that item
   alone, so the graph stays as small as the programming allows. */
function buildChain(item, startAt, offset) {
  const automation = (item.meta && item.meta.automation) || {};
  const nodes = [];
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

  for (const node of [
    shelf("lowshelf", BAND_LOW_HZ, automation.low),
    shelf("peaking", BAND_MID_HZ, automation.mid),
    shelf("highshelf", BAND_HIGH_HZ, automation.high),
    sweep("lowpass", 20000, automation.lpf),
    sweep("highpass", 20, automation.hpf),
  ]) {
    if (node) nodes.push(node);
  }

  // source -> [filters...] -> gain -> master
  let tail = gain;
  const echo = item.meta?.echo;
  if (echo && Number(echo.mix) > 0) {
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
  gain.connect(master);
  const filterCount = nodes.length - (echo && Number(echo.mix) > 0 ? 5 : 0);
  for (let i = filterCount - 1; i >= 0; i--) {
    nodes[i].connect(tail);
    tail = nodes[i];
  }
  return { input: tail, gain, nodes };
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
  source.connect(chain.input);

  applyEnvelope(chain.gain.gain, item.envelope, startAt, offset);
  source.start(startAt, (item.offset || 0) + playback.source);
  source.stop(startAt + Math.max(0.01, item.duration - offset));

  source.onended = () => {
    const entry = scheduled.get(item.id);
    if (entry && entry.source === source) scheduled.delete(item.id);
    try {
      chain.gain.disconnect();
      for (const node of chain.nodes) node.disconnect();
    } catch { /* already torn down */ }
  };

  scheduled.set(item.id, { source, gain: chain.gain, item });
}

async function pumpAudio() {
  if (!running || clockOffset === null) return;
  const now = stationNow();

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

function stopAll() {
  for (const { source } of scheduled.values()) {
    try { source.stop(); } catch { /* not started */ }
  }
  scheduled.clear();
}

/* ── Schedule polling ──────────────────────────────────────────────── */
async function poll() {
  try {
    const snapshot = await api("/api/schedule");

    // The station clock can move discontinuously -- a skip winds it forward
    // into the next transition. Without noticing, we would keep playing to
    // the old timeline and never hear the skip at all.
    if (epoch !== null && snapshot.epoch !== epoch) {
      stopAll();
      clockOffset = null;
    }
    epoch = snapshot.epoch;

    if (clockOffset === null && ctx) clockOffset = ctx.currentTime - snapshot.now;

    // Gentle resync: only when nothing is mid-flight, so we never jump audio.
    if (ctx && clockOffset !== null && scheduled.size === 0) {
      const drift = Math.abs(stationNow() - snapshot.now);
      if (drift > 1.5) clockOffset = ctx.currentTime - snapshot.now;
    }

    const known = new Set(items.map((i) => i.id));
    items = snapshot.items;
    for (const item of items) {
      if (!known.has(item.id) && item.kind === "voice") queueLine(item);
    }

    ui.state.textContent = snapshot.status || "—";
    if (!items.length) ui.timelineWrap.dataset.ready = "0";
    else ui.timelineWrap.dataset.ready = "1";
    pumpAudio();
  } catch (error) {
    ui.state.textContent = "server unreachable";
    console.warn(error);
  }
}

/* ── Transcript ────────────────────────────────────────────────────── */
const pendingLines = [];

function queueLine(item) {
  if (!item.meta || !item.meta.text || seenLines.has(item.id)) return;
  seenLines.add(item.id);
  pendingLines.push(item);
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

function updateNowPlaying() {
  const now = stationNow();
  const music = currentMusic(now);
  const talking = talkingNow(now);
  ui.root.dataset.talking = talking ? "1" : "0";

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
      ui.source.textContent = "mic open";
      ui.title.textContent = hostNames || "On the mic";
      ui.artist.textContent = "talking";
    } else if (running) {
      ui.source.textContent = "standing by";
      ui.title.textContent = "Lining up the next record";
      ui.artist.textContent = "";
    }
    ui.fill.style.setProperty("--p", "0");
    return;
  }

  const position = now - music.start_at;
  const key = music.meta.key;

  if (key !== currentKey) {
    currentKey = key;
    ui.title.textContent = music.meta.title || "Unknown";
    ui.artist.textContent = music.meta.artist || "";
    ui.up.disabled = ui.down.disabled = false;
    if (!reported.has(`start:${music.id}`)) {
      reported.add(`start:${music.id}`);
      api("/api/report", {
        method: "POST",
        body: JSON.stringify({ kind: "started", key }),
      }).catch(() => {});
    }
  }

  ui.source.textContent = talking ? "mic open" : "on air";
  ui.pos.textContent = mmss(position);
  ui.dur.textContent = mmss(music.duration);
  const fraction = clamp(position / music.duration, 0, 1);
  ui.fill.style.setProperty("--p", fraction.toFixed(4));
  ui.progress.setAttribute("aria-valuenow", Math.round(fraction * 100));

  // Report completion once, near the end.
  if (position > music.duration - 1.2 && !reported.has(`end:${music.id}`)) {
    reported.add(`end:${music.id}`);
    api("/api/report", {
      method: "POST",
      body: JSON.stringify({
        kind: "played", key, position, duration: music.duration,
      }),
    }).catch(() => {});
  }
}

/* Match a canvas bitmap to its laid-out box.

   Measured from the border box and rounded, and only written when it actually
   changes. Setting canvas.width/height alters the element's intrinsic size, so
   a canvas whose CSS size depends on its own content will grow every frame --
   the CSS keeps these two out of flow to prevent exactly that, and the
   rounding here stops sub-pixel jitter from rewriting the bitmap forever. */
function fitCanvas(canvas) {
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const box = canvas.getBoundingClientRect();
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

/* The trace gets a display gain so quiet passages stay legible. This is a
   scope, not a meter -- it shows shape, and the meters carry the level. */
const SCOPE_GAIN = 2.0;

function drawScope() {
  const canvas = ui.scope;
  const size = fitCanvas(canvas);
  if (!size) return;
  const { width, height, dpr } = size;
  const g = canvas.getContext("2d");
  g.setTransform(dpr, 0, 0, dpr, 0, 0);
  g.clearRect(0, 0, width, height);

  if (!analyser) return;
  if (!wave.data) wave.data = new Uint8Array(analyser.fftSize);
  analyser.getByteTimeDomainData(wave.data);

  const talking = ui.root.dataset.talking === "1";
  const colour = talking ? "#6ee7a8" : "#ffb020";
  const mid = height / 2;

  // Centre line
  g.strokeStyle = "#ffffff0d";
  g.lineWidth = 1;
  g.beginPath(); g.moveTo(0, mid); g.lineTo(width, mid); g.stroke();

  // Waveform, drawn as a filled envelope rather than a hairline — reads as
  // a signal on a scope instead of a sparkline.
  const step = Math.max(1, Math.floor(wave.data.length / width));
  g.beginPath();
  let peak = 0;
  for (let x = 0; x < width; x++) {
    let max = 0;
    for (let i = 0; i < step; i++) {
      const sample = Math.abs(wave.data[x * step + i] - 128) / 128;
      if (sample > max) max = sample;
    }
    if (max > peak) peak = max;
    const y = Math.min(max * SCOPE_GAIN, 1) * (mid - 3);
    if (x === 0) g.moveTo(x, mid - y); else g.lineTo(x, mid - y);
  }
  for (let x = width - 1; x >= 0; x--) {
    let max = 0;
    for (let i = 0; i < step; i++) {
      const sample = Math.abs(wave.data[x * step + i] - 128) / 128;
      if (sample > max) max = sample;
    }
    g.lineTo(x, mid + max * (mid - 3));
  }
  g.closePath();
  g.fillStyle = colour + "33";
  g.fill();
  g.strokeStyle = colour;
  g.lineWidth = 1.2;
  g.stroke();

  wave.peak = Math.max(peak, wave.peak * 0.92);
  const level = meterScale(wave.peak);
  ui.meterL.style.setProperty("--h", level.toFixed(3));
  ui.meterR.style.setProperty("--h", (level * 0.96).toFixed(3));
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
  g.font = '9px "IBM Plex Mono", monospace';
  g.textBaseline = "top";
  for (let t = Math.ceil((now - spanBack) / 30) * 30; t < now + spanFwd; t += 30) {
    const px = Math.round(x(t)) + 0.5;
    g.strokeStyle = "#ffffff0a";
    g.beginPath(); g.moveTo(px, laneTop - 8); g.lineTo(px, height - 4); g.stroke();
    g.fillStyle = "#6f6759";
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
function frame() {
  try {
    if (running) {
      updateNowPlaying();
      renderLines();
      drawScope();
      drawTimeline();
      ui.uptime.textContent = hhmmss((performance.now() - startedAt) / 1000);
    }
    const now = new Date();
    ui.wallclock.textContent =
      `${String(now.getHours()).padStart(2, "0")}:${String(now.getMinutes()).padStart(2, "0")}`;
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

/* Keyboard, but never while someone is typing a request. */
document.addEventListener("keydown", (event) => {
  const typing = event.target instanceof HTMLInputElement
    || event.target instanceof HTMLTextAreaElement;
  if (typing || event.metaKey || event.ctrlKey || event.altKey) return;

  if (event.key === "ArrowUp") { setVolume(volume + 0.05); event.preventDefault(); }
  else if (event.key === "ArrowDown") { setVolume(volume - 0.05); event.preventDefault(); }
  else if (event.key.toLowerCase() === "m") { ui.mute.click(); }
});

/* ── Controls ──────────────────────────────────────────────────────── */
async function start() {
  if (!ctx) buildGraph();

  // Never await resume(). Under an autoplay policy the promise can stay
  // pending indefinitely, and awaiting it would strand the whole start path
  // -- the station would look dead while the server was already running.
  ctx.resume().catch(() => {});

  running = true;
  startedAt = performance.now();
  clockOffset = null;
  ui.root.dataset.state = "live";
  ui.powerLabel.textContent = "Stop";
  ui.power.classList.remove("btn--primary");
  ui.skip.disabled = false;
  ui.hint.textContent = "Warming up — the first record has to download before it can play.";

  await poll();
  watchAudioPermission();
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
  stopAll();
  if (ctx) ctx.suspend();
  ui.root.dataset.state = "idle";
  ui.powerLabel.textContent = "Start the station";
  ui.power.classList.add("btn--primary");
  ui.skip.disabled = true;
  ui.up.disabled = ui.down.disabled = true;
  ui.source.textContent = "off air";
  ui.hint.textContent = "The station holds its place while you're away.";
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
  topic: "topic", segment: "segment", directive: "play less", vibe: "vibe", clear_vibe: "vibe",
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
  if (ui.requestMode.value === "vibe" || query.length < 2 || /https?:\/\/|youtube\.com\/|youtu\.be\//i.test(query)) return;
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
  const query = ui.input.value.trim();
  if (!query) return;

  ui.note.textContent = "Working out what you meant…";
  ui.note.dataset.tone = "";
  ui.input.disabled = true;

  let payload;
  try {
    payload = await api("/api/request", {
      method: "POST", body: JSON.stringify({ query, mode: ui.requestMode.value,
        selection: ui.requestMode.value !== "vibe" && spotifySelection
          && query === `${spotifySelection.artist} - ${spotifySelection.title}` ? spotifySelection : null }),
    });
  } catch (error) {
    // A refusal comes back as a 400 with the reason already written for a
    // person, so show it as-is rather than wrapping it in our own wording.
    payload = error.payload && error.payload.message
      ? { ...error.payload, ok: false }
      : { ok: false, message: error.message };
  } finally {
    ui.input.disabled = false;
  }

  const kind = payload.intent && payload.intent.kind;
  ui.note.textContent = (payload.ok && kind && KIND_LABEL[kind])
    ? `${KIND_LABEL[kind]} — ${payload.message}`
    : payload.message || "something went wrong";
  ui.note.dataset.tone = payload.ok ? "good" : "bad";

  if (payload.ok) {
    clearSpotify();
    ui.input.value = "";
    ui.input.focus();
  }
  loadQueue();
  loadVibe();
});

ui.requestMode.addEventListener("change", () => {
  clearSpotify();
  const vibeMode = ui.requestMode.value === "vibe";
  ui.requestPrompt.textContent = vibeMode ? "What is the mood, or what are you doing?" : "What would you like to hear?";
  ui.input.placeholder = vibeMode ? "Studying, calm and jazzy. No heavy metal." : "A song, YouTube link, genre, or topic…";
  ui.note.textContent = vibeMode ? "Stays on until changed or cleared. Planned mixes finish first; song requests keep priority." : "Ask for a song, artist, genre, or something for the hosts to discuss.";
  ui.note.dataset.tone = "";
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
const STAGE_LABEL = { on_deck: "on deck", queued: "queued", finding: "finding" };

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

let queueBusy = false;

async function loadQueue() {
  if (queueBusy) return;
  queueBusy = true;
  let data, wishes = [];
  try {
    data = await api("/api/queue");
    wishes = (await api("/api/requests")).wishes || [];
  } catch {
    queueBusy = false;
    return;
  }
  queueBusy = false;

  const rows = data.items || [];
  const movable = rows.filter((r) => r.stage === "queued");

  ui.queue.replaceChildren();

  // Topics and forced segments sit above the music -- they change the next
  // break rather than the running order.
  for (const wish of wishes) {
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

  const count = rows.filter((r) => !r.playing).length + wishes.length;
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
    option.textContent = name;
    if (name === current) option.selected = true;
    select.append(option);
  }

  const note = document.createElement("p");
  note.className = "fader__hint";
  note.textContent = TRANSITION_HELP[current];

  select.addEventListener("change", async () => {
    note.textContent = TRANSITION_HELP[select.value] || "";
    try {
      await api("/api/transition", {
        method: "POST", body: JSON.stringify({ preset: select.value }),
      });
      toast(`Transitions: ${select.value}`);
    } catch (error) {
      toast(error.message, "bad");
    }
  });

  wrap.append(top, select, note);
  ui.faders.prepend(wrap);
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
  applyVolume();

  try {
    const status = await api("/api/status");
    if (status.hosts && status.hosts.length) {
      const names = status.hosts.map((h) => h.name);
      ui.hostPill.textContent = names.join(" · ");
      hostNames = names.length === 2 ? names.join(" and ") : names.join(", ");
    }
    if (!status.llm.configured) {
      toast("No OPENROUTER_API_KEY set — the hosts will fall back to canned lines.", "bad");
    }
  } catch { /* the status panel is not worth blocking the page for */ }

  buildBoard().then(buildTransitionPicker);
  loadVibe();
  setInterval(loadVibe, 4000);
  loadQueue();
  setInterval(() => { if (running) loadQueue(); }, 15000);
  setInterval(() => { if (running) poll(); }, POLL_MS);
  setInterval(() => { if (running) pumpAudio(); }, 900);

  // The station clock stops when it stops hearing from a listener. The
  // schedule poll normally does that, but it can be slow while tracks are
  // downloading -- so keep a cheap heartbeat on its own timer. Losing the
  // clock mid-record would put a hole in the broadcast.
  //
  // A hidden tab throttles this to about once a minute, which is why the
  // server allows a generous window before it decides nobody is there.
  setInterval(() => {
    if (running) fetch("/api/heartbeat", { method: "POST" }).catch(() => {});
  }, 8000);

  // Coming back to the tab should catch up immediately rather than waiting
  // for the next throttled tick.
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden && running) {
      fetch("/api/heartbeat", { method: "POST" }).catch(() => {});
    }
  });
  requestAnimationFrame(frame);
  nameTheStation();
}

/* The page used to be rendered by the station, so the name and tagline came
   with it. Now it is served by the app and has to ask. */
async function nameTheStation() {
  try {
    const status = await api("/api/status");
    const identity = status.identity || {};
    if (identity.name) {
      el("ident-name").textContent = identity.name;
      document.title = identity.name;
    }
    if (identity.tagline) el("ident-tag").textContent = identity.tagline;
  } catch {
    // No station. The console still runs; the radio half simply is not there.
    el("ident-tag").textContent = "";
  }
}


document.addEventListener("visibilitychange", () => {
  if (!document.hidden && running) poll();
});

boot();
