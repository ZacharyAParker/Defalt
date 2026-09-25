/* The startup intro: the OBBY STUDIO ident, once per page load, muted.
   Every visit, once a day (a still of the final logo after the first) or
   never, from the setting in the reading window's foot. Reduced motion gets
   the still. It's a modal on top of everything, the first-run notice
   included, and never holds anything up: the notice and the player start
   underneath it as if it weren't there. The video isn't cached by the
   service worker; it's a one-off. */
(() => {
  "use strict";
  const dialog = document.getElementById("intro");
  const video = document.getElementById("intro-video");
  const skipButton = document.getElementById("intro-skip");
  const setting = document.getElementById("intro-mode");
  if (!dialog || !video || typeof dialog.showModal !== "function") return;

  const MODE = "defalt.intro.mode";
  const DAY = "defalt.intro.day";
  const MODES = ["every", "daily", "off"];
  const STILL_MS = 1000;      // the final logo, on later visits the same day
  const REDUCED_MS = 800;     // the final logo, with reduced motion
  const LONGEST_MS = 9000;    // a video that never ends still ends
  const FADE_MS = 260;

  function get(key) { try { return localStorage.getItem(key); } catch { return null; } }
  function set(key, value) { try { localStorage.setItem(key, value); } catch {} }
  function today(now = new Date()) {
    const pad = n => String(n).padStart(2, "0");
    return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
  }
  function mode() {
    const saved = get(MODE);
    return MODES.includes(saved) ? saved : "every";
  }
  function reduced() {
    const booth = document.getElementById("reduced");
    if (booth?.checked) return true;
    try { return !!globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches; } catch { return false; }
  }
  /* "full", "still" or "skip". */
  function plan() {
    const chosen = mode();
    if (chosen === "off") return "skip";
    if (reduced()) return "still";
    if (chosen === "daily" && get(DAY) === today()) return "still";
    return "full";
  }

  let finished = false;
  let timer = null;

  function finish() {
    if (finished || !dialog.open) return;
    finished = true;
    clearTimeout(timer);
    dialog.classList.add("intro--out");
    setTimeout(() => {
      video.pause?.();
      video.removeAttribute("src");
      dialog.close();
      document.body.classList.remove("intro-playing");
    }, reduced() ? 0 : FADE_MS);
  }

  /* A key that skipped the intro mustn't go on to press whatever has focus
     once it's gone -- the first-run notice's "I agree", say. Swallowed
     until it's let go. */
  function swallow(key) {
    const eat = event => {
      if (event.key !== key) return;
      event.preventDefault();
      event.stopPropagation();
      if (event.type === "keyup") {
        globalThis.removeEventListener("keydown", eat, true);
        globalThis.removeEventListener("keyup", eat, true);
      }
    };
    globalThis.addEventListener("keydown", eat, true);
    globalThis.addEventListener("keyup", eat, true);
  }

  function still(ms) {
    video.removeAttribute("src");
    timer = setTimeout(finish, ms);
  }

  function start() {
    const chosen = plan();
    if (chosen === "skip") return chosen;
    video.poster = dialog.dataset.poster || "";
    video.muted = true;
    video.playsInline = true;
    dialog.showModal();
    document.body.classList.add("intro-playing");
    if (chosen === "still") {
      still(reduced() ? REDUCED_MS : STILL_MS);
      return chosen;
    }
    set(DAY, today());
    video.addEventListener("ended", finish);
    video.addEventListener("error", () => { if (!finished) { clearTimeout(timer); still(REDUCED_MS); } });
    video.src = dialog.dataset.video || "";
    timer = setTimeout(finish, LONGEST_MS);
    // Autoplay refused (a battery saver, say): the still instead.
    const playing = video.play?.();
    playing?.catch?.(() => { if (!finished) { clearTimeout(timer); still(REDUCED_MS); } });
    return chosen;
  }

  dialog.addEventListener("keydown", event => {
    event.stopPropagation();
    if (event.key === "Escape" || event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      swallow(event.key);
      finish();
    }
  });
  // Escape through the browser's own path: skip, don't just close.
  dialog.addEventListener("cancel", event => { event.preventDefault(); finish(); });
  // A click, not a press: a tap that closed it on the way down would land
  // its click on whatever is underneath.
  dialog.addEventListener("click", () => finish());
  skipButton?.addEventListener("click", event => { event.stopPropagation(); finish(); });

  if (setting) {
    setting.value = mode();
    setting.addEventListener("change", () => {
      if (MODES.includes(setting.value)) set(MODE, setting.value);
    });
  }

  globalThis.DefaltIntro = {plan, mode, today, finish, started: start()};
})();
