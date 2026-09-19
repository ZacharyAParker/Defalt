// Exercise the actual browser Skip handler with a stubbed audio/UI boundary.
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const source = fs.readFileSync('web/static/radio.js', 'utf8');
const start = source.indexOf('ui.skip.addEventListener("click", async () => {');
assert(start >= 0);
const end = source.indexOf('\n});', start);
assert(end > start);
const handlerSource = source.slice(start, end + 4);

async function run(result) {
  let handler;
  const calls = {stops: 0, polls: 0, pumps: 0, toasts: []};
  const context = {
    ui: {skip: {disabled: false, addEventListener: (_, fn) => { handler = fn; }}},
    running: true, clockOffset: 123, currentKey: 'current',
    api: async () => result,
    stopAll: () => { calls.stops++; },
    poll: async () => { calls.polls++; },
    pumpAudio: () => { calls.pumps++; },
    loadQueue: () => {}, setTimeout: () => {},
    toast: text => calls.toasts.push(text),
  };
  vm.runInNewContext(handlerSource, context);
  await handler();
  assert.equal(context.ui.skip.disabled, false);
  assert.equal(calls.polls, 1);
  assert.equal(calls.pumps, 1);
  return {context, calls};
}

(async () => {
  for (const mode of ['preparing', 'speaking', 'already_mixing', 'empty']) {
    const {context, calls} = await run({mode, skipped: 0});
    assert.equal(calls.stops, 0, `${mode} interrupted the playing deck`);
    assert.equal(context.clockOffset, 123);
    assert.equal(context.currentKey, 'current');
    assert.notEqual(calls.toasts[0], 'Skipped');
  }
  const {context, calls} = await run({mode: 'transition', skipped: 120, into: 'Next'});
  assert.equal(calls.stops, 1);
  assert.equal(context.clockOffset, null);
  assert.equal(context.currentKey, null);
  console.log('Browser skip preparation and transition checks passed.');
})().catch(error => { console.error(error); process.exitCode = 1; });
