// Runs every tests/test_browser_*.js in its own node process, from the repo
// root, and fails if any of them fails. No npm packages.
//
//   node tests/run-browser.js            all suites
//   node tests/run-browser.js vibe skip  only files whose name contains a word
"use strict";
const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const root = path.resolve(__dirname, "..");
const filters = process.argv.slice(2);
const files = fs.readdirSync(__dirname)
  .filter((name) => /^test_browser_.+\.js$/.test(name))
  .filter((name) => !filters.length || filters.some((word) => name.includes(word)))
  .sort();

if (!files.length) {
  console.error("no browser tests matched");
  process.exit(1);
}

let failed = 0;
const started = Date.now();
for (const name of files) {
  const result = spawnSync(process.execPath, [path.join("tests", name)], {
    cwd: root, encoding: "utf8", timeout: 60000,
  });
  const output = `${result.stdout || ""}${result.stderr || ""}`.trim();
  const ok = result.status === 0 && !result.error;
  if (!ok) failed++;
  console.log(`${ok ? "ok  " : "FAIL"} ${name}`);
  if (!ok || process.env.VERBOSE) {
    if (output) console.log(output.replace(/^/gm, "     "));
    if (result.error) console.log(`     ${result.error.message}`);
  }
}
const seconds = ((Date.now() - started) / 1000).toFixed(1);
console.log(`\n${files.length - failed} passed, ${failed} failed, ${files.length} browser test files in ${seconds}s`);
process.exitCode = failed ? 1 : 0;
