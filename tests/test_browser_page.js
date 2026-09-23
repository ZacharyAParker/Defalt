// The page itself: no third-party requests, cache-busted assets, fonts that
// exist and carry their licence, and CSS that reads at a legible size.
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const {root, read} = require('./browser_harness');

const html = read('web/index.html');
assert.ok(!/fonts\.googleapis|fonts\.gstatic/.test(html), 'fonts are self-hosted');
const external = [...html.matchAll(/<(?:link|script)[^>]+(?:href|src)="(https?:[^"]+)"/g)].map((m) => m[1]);
assert.deepEqual(external, [], 'the page loads nothing from another origin');

const assets = [...html.matchAll(/(?:href|src)="(\/static\/[^"]+)"/g)].map((m) => m[1]);
assert.ok(assets.length >= 10);
for (const asset of assets) {
  assert.match(asset, /\?v=\{\{APP_VERSION\}\}$/, `${asset} is versioned`);
  const file = path.join(root, 'web', asset.replace(/\?.*$/, ''));
  assert.ok(fs.existsSync(file), `${asset} exists`);
}

const fonts = read('web/static/fonts.css');
const files = [...fonts.matchAll(/url\(([^)?]+)/g)].map((m) => m[1]);
assert.ok(files.length >= 4);
for (const file of files) assert.ok(fs.existsSync(path.join(root, 'web/static', file)), `${file} exists`);
for (const family of ['Archivo', 'IBM Plex Mono']) assert.ok(fonts.includes(`font-family: '${family}'`));
for (const licence of ['OFL-Archivo.txt', 'OFL-IBMPlexMono.txt']) {
  assert.match(read(`web/static/fonts/${licence}`), /SIL OPEN FONT LICENSE/i, `${licence} ships with the fonts`);
}

// Legibility: muted text meets WCAG AA on every panel colour, and nothing
// (bar the decorative vinyl label) is set below 11px.
const css = read('web/static/radio.css');
const token = (name) => new RegExp(`--${name}:\\s*(#[0-9a-f]{6})`, 'i').exec(css)[1];
const luminance = (hex) => {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
    .map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};
const contrast = (a, b) => {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
};
for (const ground of ['ink-000', 'ink-050', 'ink-100', 'ink-200', 'ink-300']) {
  assert.ok(contrast(token('text-mute'), token(ground)) >= 4.5, `--text-mute on --${ground}`);
}
for (const file of ['radio.css', 'studio.css', 'director-chat.css', 'about.css']) {
  const source = read(`web/static/${file}`);
  for (const match of source.matchAll(/([^{}]*)\{[^}]*?font-size:\s*([\d.]+)px/g)) {
    if (match[1].includes('#studio-cover-label')) continue;
    assert.ok(Number(match[2]) >= 11, `${file}: ${match[1].trim()} is ${match[2]}px`);
  }
}

// Dead rules stay dead, and each selector is defined once.
assert.ok(!/\.desk|\.wave\b|\.pitch/.test(css), 'the old browser deck styles are gone');
assert.equal(css.match(/^\.picker \{/gm).length, 1);
assert.equal(css.match(/^\.request__note\[data-tone="bad"\]/gm).length, 1);
assert.ok(!read('web/static/studio.css').includes('pixelated'), 'downscaled painted art is not pixelated');
assert.ok(!fs.existsSync(path.join(root, 'web/static/studio')), 'the unused first studio art is gone');
console.log('Browser page: same-origin assets, versioned URLs, self-hosted fonts, contrast, text sizes and CSS hygiene passed.');
