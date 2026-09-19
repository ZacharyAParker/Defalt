const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');

class Element {
  constructor(tag = 'div', page) {
    this.tag = tag; this.dataset = {infoPage: page}; this.handlers = {};
    this.children = []; this.attributes = {}; this.open = false;
  }
  addEventListener(name, fn) { this.handlers[name] = fn; }
  append(child) { this.children.push(child); }
  replaceChildren() { this.children = []; }
  setAttribute(name, value) { this.attributes[name] = value; }
  showModal() { this.open = true; }
  close() { this.open = false; this.handlers.close(); }
  focus() { this.focused = true; }
  get innerHTML() { throw new Error('Documents must never render executable HTML'); }
  set innerHTML(_) { throw new Error('Documents must never render executable HTML'); }
}

async function run() {
  const names = ['patches', 'privacy', 'terms', 'copyright'];
  const outside = names.map(page => new Element('button', page));
  const inside = names.map(page => new Element('button', page));
  const elements = Object.fromEntries(['info-window', 'info-document', 'info-title', 'info-close'].map(id => [id, new Element()]));
  const dialog = elements['info-window'];
  dialog.querySelectorAll = () => inside;
  const calls = [];
  let resolve, reject;
  const classes = new Set();
  const document = {
    body: {classList: {add: key => classes.add(key), remove: key => classes.delete(key)}},
    getElementById: id => elements[id],
    querySelectorAll: () => [...outside, ...inside],
    createElement: tag => new Element(tag),
  };
  vm.runInNewContext(fs.readFileSync('web/static/about.js', 'utf8'), {
    document, AbortSignal,
    fetch: path => {
      calls.push(path);
      return new Promise((yes, no) => { resolve = yes; reject = no; });
    },
  });
  const pendingPatches = outside[0].handlers.click();
  const pendingPrivacy = inside[1].handlers.click();
  assert.equal(dialog.open, true);
  assert.deepEqual(calls, ['/api/about']);
  reject(new Error('offline'));
  await Promise.all([pendingPatches, pendingPrivacy]);
  assert.match(elements['info-document'].children[0].textContent, /Couldn’t load/);
  const retry = elements['info-document'].children[1].handlers.click();
  const docs = Object.fromEntries(names.map(page => [page, `# ${page}\n\n<script>unsafe()</script>\n- A note\n\n---\nFooter`]));
  resolve({ok: true, json: async () => ({documents: docs})});
  await retry;
  assert.equal(elements['info-title'].textContent, 'Privacy policy');
  assert.equal(elements['info-document'].children[0].textContent, 'privacy');
  assert.equal(elements['info-document'].children[1].textContent, '<script>unsafe()</script>');
  assert.equal(elements['info-document'].children.length, 3);
  let stopped = false;
  dialog.handlers.keydown({stopPropagation() { stopped = true; }});
  assert.equal(stopped, true, 'reading keys must not reach playback shortcuts');
  elements['info-close'].handlers.click();
  assert.equal(dialog.open, false);
  assert.equal(outside[0].focused, true);
  assert.equal(classes.size, 0);
  await outside[3].handlers.click();
  assert.equal(elements['info-title'].textContent, 'Copyright');
  assert.equal(calls.length, 2, 'already-loaded documents need no further network requests');
  assert.ok(calls.every(path => path === '/api/about'), 'release pages must never call playback APIs');
  console.log('Browser release pages: tab race, offline retry, safe text, keyboard isolation, focus restoration passed.');
}
run().catch(error => { console.error(error); process.exitCode = 1; });
