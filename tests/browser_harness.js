// Shared helpers for the browser tests. Not a test itself (the runner only
// picks up test_browser_*.js).
//
// The tests run real code from web/static against a fake boundary. Slicing a
// file between two markers keeps that cheap, but a marker that silently stops
// matching used to turn into indexOf(-1) and a confusing failure far away, so
// every slice goes through section(), which says which marker went missing.
"use strict";
const fs = require("node:fs");
const path = require("node:path");

const root = path.resolve(__dirname, "..");

function read(file) {
  return fs.readFileSync(path.join(root, file), "utf8");
}

/* The source from the start of `from` up to (not including) `to`. Both are
   plain strings; `to` is searched for after `from`. Omit `to` for the rest. */
function section(file, from, to) {
  const source = read(file);
  const start = source.indexOf(from);
  if (start < 0) throw new Error(`${file}: start marker not found: ${JSON.stringify(from)}`);
  if (to === undefined) return source.slice(start);
  const end = source.indexOf(to, start + from.length);
  if (end < 0) throw new Error(`${file}: end marker not found after ${JSON.stringify(from)}: ${JSON.stringify(to)}`);
  return source.slice(start, end);
}

/* A forgiving stand-in for a DOM element: records handlers, children,
   attributes and style properties, and has a working classList. */
function element(tag = "div") {
  const classes = new Set();
  const node = {
    tag, value: "", textContent: "", hidden: false, disabled: false, checked: false,
    dataset: {}, handlers: {}, children: [], attributes: {}, styles: {},
    classList: {
      add: (...names) => names.forEach((n) => classes.add(n)),
      remove: (...names) => names.forEach((n) => classes.delete(n)),
      toggle: (name, force) => {
        const on = force === undefined ? !classes.has(name) : !!force;
        if (on) classes.add(name); else classes.delete(name);
        return on;
      },
      contains: (name) => classes.has(name),
    },
    style: { setProperty(name, value) { node.styles[name] = value; } },
    addEventListener(type, fn) { node.handlers[type] = fn; },
    removeEventListener(type) { delete node.handlers[type]; },
    append(...children) { node.children.push(...children); },
    prepend(...children) { node.children.unshift(...children); },
    replaceChildren(...children) { node.children = children; },
    remove() { node.removed = true; },
    setAttribute(name, value) { node.attributes[name] = String(value); },
    getAttribute(name) { return node.attributes[name] ?? null; },
    focus() { node.focused = true; },
    get firstElementChild() { return node.children[0]; },
    get childElementCount() { return node.children.length; },
  };
  return node;
}

module.exports = { root, read, section, element };
