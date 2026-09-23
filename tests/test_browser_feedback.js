const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');

class Element {
  constructor(tag) {
    this.tag = tag; this.handlers = {}; this.children = []; this.attributes = {};
    this.value = ''; this.checked = false; this.hidden = false; this.open = false;
    this.textContent = ''; this.disabled = false;
  }
  addEventListener(name, fn) { this.handlers[name] = fn; }
  append(...children) { this.children.push(...children); }
  setAttribute(name, value) { this.attributes[name] = value; }
  showModal() { this.open = true; }
  close() { this.open = false; this.handlers.close?.(); }
  focus() { this.focused = true; }
  get innerHTML() { throw new Error('Reports must never render HTML'); }
  set innerHTML(_) { throw new Error('Reports must never render HTML'); }
}
// Properties the dialog sets directly, as a browser element has them.
for (const name of ['type', 'name', 'id', 'maxLength', 'autocomplete', 'placeholder', 'rows', 'htmlFor',
  'className', 'method', 'role']) Element.prototype[name] = undefined;

async function run() {
  const opener = new Element('button');
  const page = {'feedback-open': opener, 'now-title': {textContent: ' One More Time '}, 'toast': new Element('div')};
  const created = [];
  const document = {
    body: new Element('body'),
    visibilityState: 'visible',
    createElement: tag => { const node = new Element(tag); created.push(node); return node; },
    getElementById: id => page[id] || created.find(node => node.id === id) || null,
    querySelectorAll: () => [{paused: false, currentTime: 12.34, readyState: 4, networkState: 2, muted: false,
      volume: 1, error: null, currentSrc: '/media/track/abc?t=1'}],
  };
  const calls = [];
  let answer = {ok: true, status: 201, json: async () => ({id: '20260922-140305-skip-stopped', path: 'reports/20260922-140305-skip-stopped/'})};
  const logged = [];
  const context = {
    document, AbortSignal, JSON, Array, Object, String, Math, Error, Date,
    console: {error: (...a) => logged.push(a), warn: (...a) => logged.push(a), log() {}},
    window: {innerWidth: 1280, innerHeight: 800, addEventListener(name, fn) { this[name] = fn; }},
    location: {pathname: '/'}, navigator: {userAgent: 'test', onLine: true},
    setTimeout: () => 1,
    fetch: async (path, options) => {
      calls.push({path, options, body: JSON.parse(options.body)});
      if (answer instanceof Error) throw answer;
      return answer;
    },
  };
  vm.runInNewContext(fs.readFileSync('web/static/feedback.js', 'utf8'), context);

  const dialog = document.body.children[0];
  assert.equal(dialog.tag, 'dialog');
  const byId = id => created.find(node => node.id === id);
  const form = created.find(node => node.tag === 'form');
  const status = created.find(node => node.className === 'feedback-status');

  // Errors the page logs before a report go with it.
  context.console.error('audio stalled', new Error('decode failed'));
  context.window.unhandledrejection({reason: 'schedule fetch timed out'});
  assert.equal(logged.length, 1, 'the real console still hears it');

  opener.handlers.click();
  assert.equal(dialog.open, true);
  assert.equal(byId('feedback-title').focused, true);

  await form.handlers.submit({preventDefault() {}});
  assert.equal(calls.length, 0, 'an empty report is never sent');
  assert.match(status.textContent, /title/);

  byId('feedback-kind-idea').checked = true;
  byId('feedback-kind-idea').handlers.change();
  assert.equal(created.find(node => node.children.includes(byId('feedback-expected'))).hidden, true);
  byId('feedback-kind-idea').checked = false;
  byId('feedback-kind-bug').checked = true;
  byId('feedback-kind-bug').handlers.change();

  byId('feedback-title').value = 'Skip stopped the music';
  byId('feedback-description').value = 'Pressed skip.';
  byId('feedback-expected').value = 'The next record.';
  answer = new TypeError('Failed to fetch');
  await form.handlers.submit({preventDefault() {}});
  assert.match(status.textContent, /Couldn’t reach the station/);
  assert.equal(byId('feedback-title').value, 'Skip stopped the music', 'a failed send keeps the draft');
  assert.equal(dialog.open, true);

  answer = {ok: false, status: 400, json: async () => ({error: 'Choose bug or suggestion.'})};
  await form.handlers.submit({preventDefault() {}});
  assert.equal(status.textContent, 'Choose bug or suggestion.');

  answer = {ok: true, status: 201, json: async () => ({id: '20260922-140305-skip-stopped', path: 'reports/20260922-140305-skip-stopped/'})};
  await form.handlers.submit({preventDefault() {}});
  const sent = calls.at(-1);
  assert.equal(sent.path, '/api/feedback');
  assert.equal(sent.options.method, 'POST');
  assert.deepEqual(
    {kind: sent.body.kind, title: sent.body.title, description: sent.body.description,
     expected: sent.body.expected, attach_logs: sent.body.attach_logs, client: sent.body.client},
    {kind: 'bug', title: 'Skip stopped the music', description: 'Pressed skip.', expected: 'The next record.',
     attach_logs: true, client: 'browser'});
  assert.equal(sent.body.client_context.showing.title, 'One More Time');
  assert.equal(sent.body.client_context.audio[0].src, '/media/track/abc');
  assert.ok(sent.body.client_logs.some(line => /\[error\] audio stalled Error: decode failed/.test(line)));
  assert.ok(sent.body.client_logs.some(line => /unhandled rejection: schedule fetch timed out/.test(line)));
  assert.ok(sent.body.client_logs.every(line => /^\d{4}-\d\d-\d\dT/.test(line) || !line.startsWith('[')));
  assert.equal(dialog.open, false);
  assert.equal(opener.focused, true, 'focus returns to the footer button');
  assert.equal(byId('feedback-title').value, '');
  assert.match(page.toast.textContent, /reports\/20260922-140305-skip-stopped\//);

  // Keys typed in the dialog never reach the player's shortcuts.
  let stopped = false;
  dialog.handlers.keydown({key: ' ', stopPropagation() { stopped = true; }});
  assert.equal(stopped, true);

  // Without logs attached, none are sent.
  opener.handlers.click();
  byId('feedback-title').value = 'Quiet';
  byId('feedback-logs').checked = false;
  await form.handlers.submit({preventDefault() {}});
  assert.deepEqual(calls.at(-1).body.client_logs, []);
  console.log('Browser feedback: draft kept on failure, context and page errors attached, logs optional, keys isolated passed.');
}
run().catch(error => { console.error(error); process.exitCode = 1; });
