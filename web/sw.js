/* =========================================================================
   Defalt — the home-screen app's service worker.

   Caches the app shell only: the page and its versioned styles, scripts,
   fonts and icons, so the app opens instantly and says something useful
   when the station at home is unreachable. Nothing live is ever cached or
   even touched -- /api, /listen, /media and /stream go straight to the
   network, exactly as if this worker were not here.
   ========================================================================= */
"use strict";

const VERSION = "{{APP_VERSION}}";
const CACHE = `defalt-shell-${VERSION}`;
const SHELL = [
  "/",
  "/manifest.webmanifest",
  `/static/fonts.css?v=${VERSION}`,
  `/static/radio.css?v=${VERSION}`,
  `/static/stream.css?v=${VERSION}`,
  `/static/stream.js?v=${VERSION}`,
  `/static/radio.js?v=${VERSION}`,
  "/static/icons/apple-touch-icon.png",
  "/static/icons/icon-192.png",
];
const LIVE = ["/api/", "/listen", "/media/", "/stream", "/sw.js"];

/* What to do with a request: "page" (network first, the cached shell when
   offline), "static" (cache first), or null (not ours: the network, untouched). */
function route(url, method = "GET", origin = null) {
  const parsed = new URL(url, origin || "http://localhost");
  if (method !== "GET") return null;
  if (origin && parsed.origin !== origin) return null;
  const path = parsed.pathname;
  if (LIVE.some((prefix) => path === prefix || path.startsWith(prefix))) return null;
  if (path === "/" || path === "/index.html") return "page";
  if (path === "/manifest.webmanifest") return "static";
  if (path.startsWith("/static/")) return "static";
  return null;
}

if (typeof self !== "undefined" && typeof self.addEventListener === "function" && !self.DefaltSWTest) {
  self.addEventListener("install", (event) => {
    event.waitUntil(caches.open(CACHE)
      // Each on its own: one missing file must not leave the shell uncached.
      .then((cache) => Promise.all(SHELL.map((url) => cache.add(new Request(url, { credentials: "include" }))
        .catch(() => {}))))
      .then(() => self.skipWaiting()));
  });

  self.addEventListener("activate", (event) => {
    event.waitUntil(caches.keys()
      .then((names) => Promise.all(names.filter((n) => n.startsWith("defalt-shell-") && n !== CACHE)
        .map((n) => caches.delete(n))))
      .then(() => self.clients.claim()));
  });

  self.addEventListener("fetch", (event) => {
    const kind = route(event.request.url, event.request.method, self.location.origin);
    if (!kind) return;
    const keep = (response) => {
      // Only a plain same-origin answer: an Access sign-in redirect is not the shell.
      if (response.ok && response.type === "basic") {
        const copy = response.clone();
        caches.open(CACHE).then((cache) => cache.put(event.request, copy)).catch(() => {});
      }
      return response;
    };
    if (kind === "page") {
      event.respondWith(fetch(event.request).then(keep)
        .catch(() => caches.match(event.request, { ignoreSearch: true })
          .then((hit) => hit || caches.match("/"))
          .then((hit) => hit || new Response("Defalt is offline.", { status: 503 }))));
      return;
    }
    event.respondWith(caches.match(event.request).then((hit) => hit || fetch(event.request).then(keep)));
  });
}

if (typeof module !== "undefined") module.exports = { route, CACHE, SHELL, LIVE };
