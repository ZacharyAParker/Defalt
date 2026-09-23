const vm = require('node:vm');
const assert = require('node:assert/strict');
const {section} = require('./browser_harness');
function element() {
  return {value: '', disabled: false, hidden: false, dataset: {}, handlers: {},
    addEventListener(type, fn) { this.handlers[type] = fn; }, focus() {}, replaceChildren() {}};
}
async function run() {
  const ui = Object.fromEntries(['form','input','articleInput','note','requestMode','requestPrompt',
    'spotifyNote','spotifyResults','vibePanel','vibeDescription','vibeClear'].map(key => [key, element()]));
  const calls = [];
  let fail = false;
  const context = vm.createContext({ui, clearTimeout() {}, loadQueue() {},
    setTimeout() { throw new Error('Articles must not search Spotify'); },
    api: async (path, options) => {
      calls.push([path, options]);
      if (fail) throw {payload: {message: 'Paste a longer article.'}};
      return {ok: true, kind: 'article', message: 'Article queued.'};
    }});
  vm.runInContext(section('web/static/radio.js', 'const KIND_LABEL =', 'const STAGE_LABEL ='), context);
  ui.input.value = 'Artist - Song';
  ui.requestMode.value = 'article';
  ui.requestMode.handlers.change();
  assert.equal(ui.input.hidden, true);
  assert.equal(ui.articleInput.hidden, false);
  assert.equal(ui.requestPrompt.htmlFor, 'article-input');
  ui.input.handlers.input();
  const text = 'A full article paragraph.\n'.repeat(50);
  ui.articleInput.value = text;
  await ui.form.handlers.submit({preventDefault() {}});
  const posted = JSON.parse(calls.find(([path]) => path === '/api/request')[1].body);
  assert.equal(posted.mode, 'article');
  assert.equal(posted.query, text.trim());
  assert.equal(posted.selection, null);
  assert.equal(ui.input.value, 'Artist - Song', 'article submission must preserve the song draft');
  assert.equal(ui.articleInput.value, '');
  assert.equal(ui.requestMode.disabled, false);
  assert.equal(ui.note.dataset.tone, 'good');
  fail = true;
  ui.articleInput.value = 'short';
  await ui.form.handlers.submit({preventDefault() {}});
  assert.equal(ui.articleInput.value, 'short', 'a rejected draft must remain editable');
  assert.equal(ui.articleInput.disabled, false);
  assert.equal(ui.note.textContent, 'Paste a longer article.');
  ui.requestMode.value = 'request';
  ui.requestMode.handlers.change();
  assert.equal(ui.input.hidden, false);
  assert.equal(ui.articleInput.hidden, true);
  console.log('Browser article mode, multiline submission, draft isolation, and recovery passed.');
}
run().catch(error => { console.error(error); process.exitCode = 1; });
