/* =========================================================================
   Defalt — stream mode.

   Away from home the page does not mix anything. The console at home plays
   the station -- stems, spinbacks, rolls, reverb, all of it -- and /listen
   is that output, encoded. This plays it through one plain <audio> element,
   which is what a locked phone, CarPlay and the lock screen understand, and
   keeps the page's picture of the station a few seconds behind the clock so
   the transcript matches what you hear rather than what the console is
   playing right now.

   radio.js owns the station view and asks this file three things: which
   mode to be in, how far behind the stream is, and to start or stop it.
   ========================================================================= */
"use strict";

(function (global) {
  const MODE_KEY = "defalt.playback";
  const PIPELINE = 0.35;       // encoder and network, on top of what is measured
  const MAX_DELAY = 30;
  const STALL_SECONDS = 8;     // no progress this long while playing: reconnect
  const LOOPBACK = new Set(["127.0.0.1", "localhost", "::1", "[::1]"]);

  /* Stream on a remote name or as an installed app; the mixer at home. A
     choice made with the toggle wins either way. */
  function chooseMode({ hostname = "", standalone = false, search = "", stored = null } = {}) {
    if (stored === "stream" || stored === "mixer") return stored;
    if (standalone || /(^|[?&])app=1(&|$)/.test(search)) return "stream";
    return LOOPBACK.has(String(hostname).toLowerCase()) ? "mixer" : "stream";
  }

  /* How far behind live the audio being heard is, in seconds: time since the
     request, plus the recent audio the server sent first, minus what has
     actually been played. Stalls grow it, as they should. */
  function streamDelay({ wallSeconds, burst = 0, currentTime = 0 }) {
    const delay = wallSeconds + burst - currentTime + PIPELINE;
    if (!Number.isFinite(delay)) return PIPELINE;
    return Math.min(Math.max(delay, 0), MAX_DELAY);
  }

  /* Reconnect waits: 1, 2, 4 ... 30 seconds. */
  const backoff = (attempt) => Math.min(30, 2 ** Math.max(0, attempt));

  function metadataFor(meta, origin = "") {
    if (!meta) return null;
    const artwork = meta.key
      ? [{ src: `${origin}/api/artwork?key=${encodeURIComponent(meta.key)}`, sizes: "512x512", type: "image/png" }]
      : [{ src: `${origin}/static/icons/icon-512.png`, sizes: "512x512", type: "image/png" }];
    return { title: meta.title || "Defalt", artist: meta.artist || "", album: meta.album || "Defalt", artwork };
  }

  /* The player. Everything outside the page is passed in, for the tests. */
  function create({ audio, fetchStatus, onNote = () => {}, onState = () => {}, mediaSession = null,
                    now = () => performance.now() / 1000, schedule = setTimeout, cancel = clearTimeout,
                    MediaMetadataClass = global.MediaMetadata, origin = "" } = {}) {
    let wanted = false;
    let requestedAt = 0;
    let burst = 0;
    let attempt = 0;
    let retryTimer = null;
    let lastProgress = { at: 0, time: 0 };
    let delay = PIPELINE;
    let connections = 0;

    function measure() {
      if (!requestedAt) return delay;
      delay = streamDelay({ wallSeconds: now() - requestedAt, burst, currentTime: audio.currentTime || 0 });
      return delay;
    }

    /* Synchronous up to play(): iOS only lets a tap start audio if nothing
       was awaited in between. */
    function connect() {
      cancel(retryTimer);
      retryTimer = null;
      burst = 0;
      const generation = ++connections;
      requestedAt = now();
      lastProgress = { at: now(), time: 0 };
      audio.src = `/listen?t=${Math.round(now() * 1000)}`;
      try { audio.load?.(); } catch { /* not every element has it */ }
      onState("connecting");
      const played = audio.play?.();
      if (played && typeof played.catch === "function") {
        played.catch(() => onNote("Tap play to start the stream."));
      }
      // The server sends up to two seconds of recent audio first when its
      // encoder was already running (the listener that starts it gets none);
      // that is part of how far behind we are.
      Promise.resolve().then(fetchStatus).then((status) => {
        if (generation !== connections) return;
        if (status && status.console_running === false) {
          onNote("The console isn't running at home — start Defalt to listen.");
        }
        const running = status?.broadcast;
        if (running?.encoding && Number(running.uptime_seconds) >= 2) burst = 2;
      }).catch(() => { /* measure without it */ });
    }

    function retry(reason) {
      if (!wanted || retryTimer) return;
      const wait = backoff(attempt++);
      onState("reconnecting");
      onNote(`${reason} Reconnecting in ${wait}s…`);
      retryTimer = schedule(() => { retryTimer = null; if (wanted) connect(); }, wait * 1000);
    }

    audio.addEventListener("playing", () => {
      attempt = 0;
      onState("playing");
      onNote("");
      lastProgress = { at: now(), time: audio.currentTime || 0 };
    });
    audio.addEventListener("timeupdate", () => {
      if ((audio.currentTime || 0) > lastProgress.time) lastProgress = { at: now(), time: audio.currentTime };
      measure();
    });
    audio.addEventListener("waiting", () => onState("buffering"));
    audio.addEventListener("error", () => retry("The stream dropped."));
    audio.addEventListener("ended", () => retry("The stream ended."));

    /* Called about once a second: a stream that has stopped moving without
       telling anyone is reconnected. */
    function watch() {
      if (!wanted || retryTimer || audio.paused) return;
      if (now() - lastProgress.at > STALL_SECONDS) {
        lastProgress.at = now();
        retry("The stream stalled.");
      }
    }

    function play() {
      if (wanted) return;
      wanted = true;
      attempt = 0;
      connect();
    }

    /* Back from an interruption (a call, another app's audio): a live stream
       resumes at live, not where it paused. */
    function resume() {
      if (!wanted) return play();
      attempt = 0;
      connect();
    }

    function stop() {
      wanted = false;
      cancel(retryTimer);
      retryTimer = null;
      requestedAt = 0;
      delay = PIPELINE;
      try { audio.pause?.(); } catch { /* already */ }
      // Letting go of the source closes the connection, so the console
      // counts one listener fewer and can stop encoding.
      audio.removeAttribute?.("src");
      try { audio.load?.(); } catch { /* ok */ }
      onState("stopped");
    }

    function setVolume(level, muted) {
      audio.volume = Math.min(Math.max(level, 0), 1);
      audio.muted = !!muted;
    }

    let shown = null;
    function nowPlaying(meta) {
      if (!mediaSession) return;
      const data = metadataFor(meta, origin);
      const mark = JSON.stringify(data);
      if (!data || mark === shown) return;
      shown = mark;
      try { mediaSession.metadata = MediaMetadataClass ? new MediaMetadataClass(data) : data; } catch { /* old browser */ }
    }

    function bindControls({ onPlay, onPause, onNext }) {
      if (!mediaSession?.setActionHandler) return;
      const set = (action, handler) => { try { mediaSession.setActionHandler(action, handler); } catch { /* unsupported */ } };
      set("play", () => onPlay());
      set("pause", () => onPause());
      set("stop", () => onPause());
      set("nexttrack", () => onNext());
      // Seeking a live stream means nothing; say so rather than pretend.
      set("seekto", null);
      set("seekbackward", null);
      set("seekforward", null);
      set("previoustrack", null);
    }

    return {
      play, resume, stop, watch, setVolume, nowPlaying, bindControls,
      delay: () => (wanted ? measure() : 0),
      playing: () => wanted,
    };
  }

  const exported = { chooseMode, streamDelay, backoff, metadataFor, create, MODE_KEY };
  if (typeof module !== "undefined" && module.exports) module.exports = exported;
  global.RemoteStream = exported;
})(typeof window !== "undefined" ? window : globalThis);
