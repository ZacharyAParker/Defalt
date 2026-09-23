// Run with node tests/test_browser_rates.js. The helpers are independent of DOM/audio.
const assert = require("node:assert/strict");
const { section } = require("./browser_harness");
const helpers = section("web/static/radio.js", "const clamp =", "function mmss(");
const { playbackCurve, playbackAt } = new Function(helpers + "; return {playbackCurve, playbackAt};")();
const curve = [[0, 0.96], [10, 0.96], [30, 1]];
const close = (actual, expected) => assert.ok(Math.abs(actual - expected) < 1e-8, `${actual} != ${expected}`);
close(playbackAt(curve, 5).source, 4.8);
close(playbackAt(curve, 20).source, 19.3);
close(playbackAt(curve, 20).rate, 0.98);
close(playbackAt(curve, 35).source, 34.2);
close(playbackAt(curve, 35).rate, 1);
const late = playbackAt(curve, 20);
close(playbackAt(curve, 35).source - late.source, 14.9);
assert.deepEqual(playbackCurve({kind: "music", meta: {playback_rate: 1.04}}), [[0, 1.04]]);
assert.deepEqual(playbackCurve({kind: "voice", meta: {playback_rate: 1.04, rate_curve: curve}}), [[0, 1]]);
assert.deepEqual(playbackCurve({kind: "music", meta: {rate_curve: [[0, 1], [10, 0.5]]}}), [[0, 1]]);
assert.deepEqual(playbackCurve({kind: "music", meta: {rate_curve: [[0, 1], [0, 1.02]]}}), [[0, 1]]);
console.log("Browser playback-rate integration checks passed.");
