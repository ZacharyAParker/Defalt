const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const source = fs.readFileSync('web/static/radio.js', 'utf8');
function element() {
  return {value: '', textContent: '', hidden: false, disabled: false, dataset: {}, handlers: {}, children: [],
    addEventListener(type, handler) { this.handlers[type] = handler; },
    append(...children) { this.children.push(...children); }, replaceChildren() { this.children = []; }, focus() {}};
}
async function run() {
  const ui = Object.fromEntries(['form','input','note','requestMode','requestPrompt','spotifyNote','spotifyResults',
    'vibePanel','vibeDescription','vibeClear'].map(name => [name, element()]));
  ui.requestMode.value = 'request';
  let timer, pending = [], posts = [];
  const context = vm.createContext({ui, document: {createElement: element},
    setTimeout(fn) { timer = fn; return 1; }, clearTimeout() { timer = null; },
    mmss(s) { return `${Math.floor(s/60)}:${String(Math.floor(s%60)).padStart(2,'0')}`; }, loadQueue() {},
    api(path, options) {
      if (path.startsWith('/api/spotify/search')) return new Promise((resolve, reject) => pending.push({path, resolve, reject}));
      if (path === '/api/request') posts.push(JSON.parse(options.body));
      return Promise.resolve({ok: true, message: 'queued', vibe: {}});
    }});
  vm.runInContext(source.slice(source.indexOf('const KIND_LABEL ='), source.indexOf('const STAGE_LABEL =')), context);
  const track = {artist: 'Artist', title: '<img src=x>', duration_ms: 123000, album: 'Album', year: '2024'};
  ui.input.value = 'first'; ui.input.handlers.input(); const old = timer();
  ui.input.value = 'second'; ui.input.handlers.input(); const current = timer();
  pending[0].resolve({tracks: [track]}); await old;
  assert.equal(ui.spotifyResults.children.length, 0, 'stale result was shown');
  pending[1].resolve({tracks: [track]}); await current;
  const button = ui.spotifyResults.children[0].children[0];
  assert.equal(button.children[0].textContent, '<img src=x>', 'result must be plain text');
  button.handlers.click();
  assert.equal(ui.input.value, 'Artist - <img src=x>');
  await ui.form.handlers.submit({preventDefault() {}});
  assert.equal(posts[0].selection.duration_ms, 123000);
  assert.equal(posts[0].selection.album, 'Album');
  assert.equal(ui.spotifyResults.hidden, true);
  ui.input.value = 'https://youtu.be/dQw4w9WgXcQ'; ui.input.handlers.input();
  assert.equal(timer, null, 'YouTube links must not search Spotify');
  await ui.form.handlers.submit({preventDefault() {}});
  assert.equal(posts[1].selection, null);
  ui.requestMode.value = 'vibe'; ui.requestMode.handlers.change();
  ui.input.value = 'working'; ui.input.handlers.input();
  assert.equal(timer, null, 'vibe text must not search Spotify');
  ui.requestMode.value = 'request'; ui.requestMode.handlers.change();
  ui.input.value = 'failure'; ui.input.handlers.input(); const failure = timer();
  pending[2].reject({payload: {message: 'Spotify rate limited'}}); await failure;
  assert.equal(ui.spotifyNote.textContent, 'Spotify rate limited');
  assert.equal(ui.input.disabled, false);
  console.log('Browser Spotify selection, stale responses, links, vibe mode, and errors passed.');
}
run().catch(error => { console.error(error); process.exitCode = 1; });
