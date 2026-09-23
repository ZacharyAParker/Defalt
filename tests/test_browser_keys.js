// Volume and mute shortcuts must never steal keys from a control that owns them.
const vm = require('node:vm');
const assert = require('node:assert/strict');
const {section} = require('./browser_harness');

let keydown;
const changes = [], mutes = [];
const context = vm.createContext({
  document: {addEventListener: (name, fn) => { if (name === 'keydown') keydown = fn; }},
  volume: 0.5,
  setVolume: (value) => changes.push(Math.round(value * 100) / 100),
  ui: {mute: {click: () => mutes.push(1)}},
});
vm.runInContext(section('web/static/radio.js', 'function ownsKeys(', '/* ── Controls ──'), context);

// A target that answers closest() like the DOM would for the given tag chain.
function target(...chain) {
  return {closest(selector) {
    const wanted = selector.split(',').map((part) => part.trim());
    return chain.find((node) => wanted.some((sel) => sel === node || (sel.startsWith('[') && node === sel))) || null;
  }};
}
function press(key, on, extra = {}) {
  let prevented = false;
  keydown({key, target: on, defaultPrevented: false, preventDefault() { prevented = true; }, ...extra});
  return prevented;
}

assert.equal(press('ArrowUp', target('body')), true);
assert.deepEqual(changes, [0.55], 'arrows on the page change the volume');
press('m', target('body'));
assert.equal(mutes.length, 1);

for (const owner of ['select', 'input', 'textarea', 'dialog', "[contenteditable]:not([contenteditable='false'])", "[role='listbox']"]) {
  assert.equal(press('ArrowDown', target('option', owner)), false, `${owner} keeps its arrow keys`);
  press('M', target(owner));
}
assert.deepEqual(changes, [0.55]);
assert.equal(mutes.length, 1, 'typing an M never mutes');

press('ArrowUp', target('body'), {defaultPrevented: true});
press('ArrowUp', target('body'), {ctrlKey: true});
assert.deepEqual(changes, [0.55], 'handled or modified keys are left alone');
press('ArrowDown', target('button'));
assert.deepEqual(changes, [0.55, 0.45], 'a focused button (Start, say) still lets the arrows through');
console.log('Browser keyboard shortcuts: page keys, owned controls, dialogs and handled events passed.');
