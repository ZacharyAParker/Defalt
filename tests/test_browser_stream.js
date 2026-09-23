// Stream mode and the home-screen app: mode choice, the <audio> player and
// its MediaSession, the page's start path with no Web Audio at all, and the
// service worker's cache rules.
"use strict";
const vm = require("node:vm");
const assert = require("node:assert/strict");
const { read, section } = require("./browser_harness");

const RS = require("../web/static/stream.js");
const sw = require("../web/sw.js");

/* ── Mode ─────────────────────────────────────────────────────────── */
assert.equal(RS.chooseMode({ hostname: "127.0.0.1" }), "mixer");
assert.equal(RS.chooseMode({ hostname: "localhost" }), "mixer");
assert.equal(RS.chooseMode({ hostname: "radio.example.org" }), "stream");
assert.equal(RS.chooseMode({ hostname: "127.0.0.1", standalone: true }), "stream");
assert.equal(RS.chooseMode({ hostname: "127.0.0.1", search: "?app=1" }), "stream");
assert.equal(RS.chooseMode({ hostname: "radio.example.org", stored: "mixer" }), "mixer", "the toggle wins");
assert.equal(RS.chooseMode({ hostname: "127.0.0.1", stored: "stream" }), "stream");
assert.equal(RS.chooseMode({ hostname: "127.0.0.1", stored: "junk" }), "mixer");

/* ── Delay ────────────────────────────────────────────────────────── */
const near = (a, b) => assert.ok(Math.abs(a - b) < 1e-9, `${a} != ${b}`);
near(RS.streamDelay({ wallSeconds: 10, burst: 2, currentTime: 9 }), 3 + 0.35);
near(RS.streamDelay({ wallSeconds: 10, burst: 0, currentTime: 12 }), 0);
assert.equal(RS.streamDelay({ wallSeconds: 500, currentTime: 0 }), 30, "clamped");
assert.equal(RS.backoff(0), 1);
assert.equal(RS.backoff(3), 8);
assert.equal(RS.backoff(12), 30);

/* ── The player ───────────────────────────────────────────────────── */
function fakeAudio() {
  const handlers = {};
  return {
    handlers, currentTime: 0, paused: true, src: "", plays: 0, loads: 0, volume: 1, muted: false,
    addEventListener(type, fn) { handlers[type] = fn; },
    removeAttribute(name) { if (name === "src") this.src = ""; },
    load() { this.loads++; },
    play() { this.plays++; this.paused = false; return Promise.resolve(); },
    pause() { this.paused = true; },
    fire(type) { handlers[type]?.(); },
  };
}

function fakeSession() {
  return {
    metadata: null, handlers: {},
    setActionHandler(action, fn) { this.handlers[action] = fn; },
  };
}

(async () => {
  let clock = 100;
  const timers = [];
  const audio = fakeAudio();
  const session = fakeSession();
  const states = [], notes = [];
  const player = RS.create({
    audio, mediaSession: session, now: () => clock,
    fetchStatus: async () => ({ console_running: true, broadcast: { encoding: true, uptime_seconds: 60 } }),
    onState: (s) => states.push(s), onNote: (n) => notes.push(n),
    schedule: (fn, ms) => { timers.push({ fn, ms }); return timers.length; }, cancel: () => {},
    MediaMetadataClass: class { constructor(data) { Object.assign(this, data); } },
  });

  // Play is synchronous up to audio.play(): iOS needs the tap's gesture.
  player.play();
  assert.equal(audio.plays, 1);
  assert.match(audio.src, /^\/listen\?t=\d+$/);
  await new Promise((resolve) => setImmediate(resolve));
  clock += 5;
  audio.currentTime = 3;
  audio.fire("timeupdate");
  // 5 s since the request, 2 s of burst, 3 s played: 4 s behind, plus the pipeline.
  near(player.delay(), 4 + 0.35);

  // Stalls reconnect with backoff; a working stream resets it.
  audio.fire("playing");
  clock += 9;
  player.watch();
  assert.equal(timers.length, 1);
  assert.equal(timers[0].ms, 1000);
  assert.ok(states.includes("reconnecting"));
  timers[0].fn();
  assert.equal(audio.plays, 2, "reconnected");
  audio.fire("error");
  assert.equal(timers[1].ms, 2000, "the second failure waits longer");

  // MediaSession: what is on, with the station's artwork, and the controls.
  player.nowPlaying({ key: "a b", title: "Song", artist: "Artist" });
  assert.equal(session.metadata.title, "Song");
  assert.equal(session.metadata.artist, "Artist");
  assert.equal(session.metadata.artwork[0].src, "/api/artwork?key=a%20b");
  const calls = [];
  player.bindControls({ onPlay: () => calls.push("play"), onPause: () => calls.push("pause"),
                        onNext: () => calls.push("next") });
  session.handlers.play();
  session.handlers.pause();
  session.handlers.nexttrack();
  assert.deepEqual(calls, ["play", "pause", "next"]);
  assert.equal(session.handlers.seekto, null, "a live stream cannot seek");

  // Stopping lets go of the connection, and nothing reconnects after it.
  player.stop();
  assert.equal(audio.src, "");
  assert.equal(player.delay(), 0);
  const before = timers.length;
  audio.fire("error");
  assert.equal(timers.length, before);

  // A console that is not running is said so.
  const quiet = RS.create({ audio: fakeAudio(), fetchStatus: async () => ({ console_running: false }),
                            onNote: (n) => notes.push(n), now: () => clock });
  quiet.play();
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(notes.some((n) => n.includes("isn't running at home")));

  /* ── The page: no Web Audio in stream mode ─────────────────────── */
  const startSource = section("web/static/radio.js", "async function start() {", "/* Chrome will not let a page");
  function runStart(streamMode) {
    const log = [];
    const context = {
      streamMode, ctx: null, running: false, startedAt: 0, clockOffset: 5,
      remote: { play: () => log.push("stream") },
      buildGraph: () => { log.push("graph"); context.ctx = { resume: () => Promise.resolve() }; },
      watchAudioState: () => log.push("watch"),
      watchAudioPermission: () => log.push("permission"),
      poll: async () => log.push("poll"), connectEvents: () => log.push("events"),
      performance: { now: () => 0 },
      ui: { root: { dataset: {} }, powerLabel: {}, power: { classList: { remove() {} } }, skip: {}, hint: {} },
    };
    vm.runInNewContext(`${startSource}; result = start();`, context);
    return context.result.then(() => ({ log, context }));
  }
  const streamed = await runStart(true);
  assert.ok(!streamed.log.includes("graph"), "stream mode built a Web Audio graph");
  assert.ok(!streamed.log.includes("permission"));
  assert.deepEqual(streamed.log, ["stream", "poll", "events"]);
  assert.equal(streamed.context.ctx, null);
  assert.match(streamed.context.ui.hint.textContent, /few seconds behind/);
  const mixed = await runStart(false);
  assert.ok(mixed.log.includes("graph") && !mixed.log.includes("stream"));

  // The scheduler never touches the audio clock in stream mode.
  const pump = section("web/static/radio.js", "async function pumpAudio() {", "function refreshScheduledGains()");
  const pumpContext = { running: true, clockOffset: 1, streamMode: true,
                        stationNow: () => { throw new Error("scheduled in stream mode"); } };
  vm.runInNewContext(`${pump}; result = pumpAudio();`, pumpContext);
  await pumpContext.result;

  // The station clock in stream mode runs on the page's clock, minus the delay.
  const clockSource = section("web/static/radio.js", "const clockNow =", "/* ── Audio graph");
  const clockContext = { streamMode: true, clockOffset: 10, ctx: null, remote: { delay: () => 4 },
                         performance: { now: () => 100000 } };
  vm.runInNewContext(`${clockSource}; result = stationNow();`, clockContext);
  assert.equal(clockContext.result, 100 - 10 - 4);

  /* ── Service worker ────────────────────────────────────────────── */
  const origin = "https://radio.example.org";
  for (const [url, expected] of [
    ["/", "page"], ["/?app=1", "page"], ["/static/radio.js?v=1", "static"], ["/static/icons/icon-192.png", "static"],
    ["/manifest.webmanifest", "static"],
    ["/api/schedule", null], ["/api/events?topics=schedule", null], ["/api/artwork?key=x", null],
    ["/listen", null], ["/listen?t=5", null], ["/media/track/abc", null], ["/stream", null], ["/sw.js", null],
    ["https://elsewhere.example/static/x.js", null],
  ]) {
    assert.equal(sw.route(url.startsWith("http") ? url : origin + url, "GET", origin), expected, url);
  }
  assert.equal(sw.route(`${origin}/static/radio.js`, "POST", origin), null, "only GETs");
  assert.ok(sw.CACHE.includes("{{APP_VERSION}}"), "the cache is named for the release");
  assert.ok(!sw.SHELL.some((url) => sw.route(origin + url, "GET", origin) === null), "the shell caches only shell");

  // The page wires it up.
  const page = read("web/index.html");
  assert.ok(page.includes('id="stream-audio"'));
  assert.ok(page.indexOf("stream.js") < page.indexOf("radio.js"), "stream.js loads first");
  console.log("Stream mode, MediaSession and service worker checks passed.");
})().catch((error) => { console.error(error); process.exitCode = 1; });
