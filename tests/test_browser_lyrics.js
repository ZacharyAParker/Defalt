// The lyric line under now playing: the right line for what the listener
// actually hears, through the item's offset, its rate and the stream delay.
"use strict";
const vm = require("node:vm");
const assert = require("node:assert/strict");
const { read, section, element } = require("./browser_harness");

const context = { window: {}, Math, Number, Array, Map, Boolean, encodeURIComponent, console };
vm.runInNewContext(read("web/static/lyrics.js"), context);
const { lineAt, sourceAt, update } = context.window.RadioLyrics;

// radio.js's own rate integration and station clock, as the page runs them.
const clock = {
  clamp: (n, lo, hi) => Math.min(Math.max(n, lo), hi), Math, Number, Array,
  streamMode: false, remote: null, clockOffset: 0, ctx: { currentTime: 0 },
  performance: { now: () => 0 },
};
vm.runInNewContext(section("web/static/radio.js", "function playbackCurve(item) {", "function mmss(seconds) {")
  + section("web/static/radio.js", "const clockNow = () =>", "/* ── Audio graph"), clock);
clock.stationNowFn = vm.runInNewContext("stationNow", clock);
const playbackAt = vm.runInNewContext("playbackAt", clock);
const playbackCurve = vm.runInNewContext("playbackCurve", clock);

const lines = [
  { t: 40, text: "one" }, { t: 50, text: "two" }, { t: 60, text: "three" },
  { t: 64, text: "" }, { t: 90, text: "after the break" },
];

// Karaoke selection.
assert.deepEqual({ ...lineAt(lines, 30) }, { current: null, next: null });
assert.deepEqual({ ...lineAt(lines, 35) }, { current: null, next: "one" });
assert.deepEqual({ ...lineAt(lines, 51) }, { current: "two", next: "three" });
assert.deepEqual({ ...lineAt(lines, 70) }, { current: null, next: null }, "a blank stamp ends the line");
assert.deepEqual({ ...lineAt(lines, 86) }, { current: null, next: "after the break" });
assert.deepEqual({ ...lineAt(lines, 200) }, { current: null, next: null }, "not held forever");
assert.deepEqual({ ...lineAt([], 10) }, { current: null, next: null });

// An item 30 s into its file, 5% fast, on air from station second 100.
const item = { id: "m", kind: "music", start_at: 100, duration: 200, offset: 30,
               meta: { key: "band|song", lyrics: "synced", playback_rate: 1.05 } };
const curve = playbackCurve(item);
assert.ok(Math.abs(sourceAt(item, 120, playbackAt, curve) - 51) < 1e-9);
assert.ok(Math.abs(sourceAt(item, 120) - 51) < 1e-9, "the plain rate agrees");

// Stream mode: the page hears the station a few seconds late, and
// stationNow() already takes the measured delay off.
clock.streamMode = true;
clock.remote = { delay: () => 4 };
clock.performance.now = () => 124000;      // 124 s on the page clock
assert.equal(clock.stationNowFn(), 120);
assert.equal(lineAt(lines, sourceAt(item, clock.stationNowFn(), playbackAt, curve)).current, "two");

// update(): fetches once, draws the line, hides when there is nothing.
(async () => {
  const nodes = { box: element("p"), line: element("span"), next: element("span") };
  nodes.box.hidden = true;
  const asked = [];
  let answer;
  const fetcher = (path) => { asked.push(path); return new Promise((resolve) => { answer = resolve; }); };
  update(nodes, item, 120, { fetcher, playbackAt, curve });
  update(nodes, item, 120, { fetcher, playbackAt, curve });
  assert.deepEqual(asked, ["/api/lyrics/band%7Csong"], "asked once");
  assert.equal(nodes.box.hidden, true, "nothing shown before the lines arrive");
  answer({ lines });
  await new Promise((resolve) => setImmediate(resolve));
  update(nodes, item, 120, { fetcher, playbackAt, curve });
  assert.equal(nodes.box.hidden, false);
  assert.equal(nodes.line.textContent, "two");
  assert.equal(nodes.next.textContent, "three");
  update(nodes, item, 140, { fetcher, playbackAt, curve });   // 72 s in: the pause
  assert.equal(nodes.box.hidden, true);
  // A record without lyrics never asks and never shows.
  const plain = { ...item, meta: { key: "x|y" } };
  update(nodes, plain, 120, { fetcher, playbackAt, curve });
  assert.equal(asked.length, 1);
  assert.equal(nodes.box.hidden, true);
  update(nodes, null, 120, { fetcher });
  assert.equal(nodes.box.hidden, true);
  console.log("Browser lyric line checks passed.");
})().catch((error) => { console.error(error); process.exitCode = 1; });
