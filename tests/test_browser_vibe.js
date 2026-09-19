// Run the actual request/vibe handlers against a small UI and API boundary.
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const source = fs.readFileSync('web/static/radio.js', 'utf8');
const start = source.indexOf('const KIND_LABEL =');
const end = source.indexOf('const STAGE_LABEL =', start);
assert(start >= 0 && end > start);
function element() {
  return { value: '', textContent: '', disabled: false, hidden: false, dataset: {}, handlers: {},
    addEventListener(type, fn) { this.handlers[type] = fn; }, focus() {}, replaceChildren() {} };
}
async function run() {
  const ui = Object.fromEntries(['form', 'input', 'note', 'requestMode', 'requestPrompt','articleInput',
    'vibePanel', 'vibeDescription', 'vibeClear', 'spotifyNote', 'spotifyResults'].map(key => [key, element()]));
  const calls = [];
  let state = {}, fail = false;
  const context = vm.createContext({ui, clearTimeout() {}, loadQueue() {}, api: async (path, options) => {
    calls.push([path, options]);
    if (fail) throw new Error('offline');
    if (path === '/api/request') {
      const body = JSON.parse(options.body);
      if (body.mode === 'vibe') state = {description: body.query};
      return {ok: true, intent: {kind: body.mode}, message: 'Saved'};
    }
    if (path === '/api/vibe/clear') state = {};
    return {ok: true, vibe: state, message: 'Cleared'};
  }});
  vm.runInContext(source.slice(start, end), context);
  ui.requestMode.value = 'vibe';
  ui.requestMode.handlers.change();
  assert.match(ui.requestPrompt.textContent, /what are you doing/);
  ui.input.value = 'Studying <img src=x onerror=bad()>';
  await ui.form.handlers.submit({preventDefault() {}});
  await vm.runInContext('loadVibe()', context);
  assert.equal(JSON.parse(calls[0][1].body).mode, 'vibe');
  assert.equal(ui.input.value, '');
  assert.equal(ui.vibeDescription.textContent, 'Studying <img src=x onerror=bad()>');
  assert.equal(ui.vibePanel.hidden, false);
  await ui.vibeClear.handlers.click();
  assert.equal(ui.vibePanel.hidden, true);
  assert.equal(ui.vibeClear.disabled, false);
  ui.requestMode.value = 'request';
  ui.input.value = 'Artist - Song';
  fail = true;
  await ui.form.handlers.submit({preventDefault() {}});
  assert.equal(ui.input.value, 'Artist - Song');
  assert.equal(ui.input.disabled, false);
  assert.equal(ui.note.dataset.tone, 'bad');
  console.log('Browser vibe submission, display, clear, and failure checks passed.');
}
run().catch(error => { console.error(error); process.exitCode = 1; });
