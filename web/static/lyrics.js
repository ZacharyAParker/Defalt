/* =========================================================================
   Side Room — the lyric line under now playing.

   The station keeps synced lyrics from LRCLIB for some records and marks
   those items (`meta.lyrics`). This fetches a record's lines once, then on
   every frame shows the line being sung, bright, and the next one, dim.
   It follows what you hear: radio.js hands it station time, which in stream
   mode already has the stream's measured delay taken off, and the item's
   offset and playing rate turn that into seconds of the record itself.
   ========================================================================= */
"use strict";

(function () {
  const HOLD = 10;   // a line is not still being sung this long after it started
  const LEAD = 6;    // the first line is teased this far ahead

  /* The line at `seconds` of the record: { current, next } texts or null. */
  function lineAt(lines, seconds) {
    const none = { current: null, next: null };
    if (!Array.isArray(lines) || !lines.length || !Number.isFinite(seconds)) return none;
    let index = 0;
    while (index < lines.length && lines[index].t <= seconds) index++;
    const upcoming = (from) => lines.slice(from).find((line) => line.text) || null;
    if (index === 0) {
      const first = upcoming(0);
      return { current: null, next: first && first.t - seconds <= LEAD ? first.text : null };
    }
    const line = lines[index - 1];
    const sung = Boolean(line.text) && seconds - line.t < HOLD;
    const next = upcoming(index);
    return {
      current: sung ? line.text : null,
      next: next && (sung || next.t - seconds <= LEAD) ? next.text : null,
    };
  }

  /* Seconds into the record at station time `now`: its offset, plus what its
     rate (and any tempo recovery curve) has played since it started. */
  function sourceAt(item, now, playbackAt, curve) {
    const elapsed = Math.max(0, now - item.start_at);
    const played = playbackAt && curve ? playbackAt(curve, elapsed).source
      : elapsed * (Number(item.meta?.playback_rate) || 1);
    return (Number(item.offset) || 0) + played;
  }

  const cache = new Map();   // key -> lines | "pending" | null

  function load(key, fetcher) {
    if (!key || cache.has(key)) return;
    cache.set(key, "pending");
    if (cache.size > 40) cache.delete(cache.keys().next().value);
    fetcher(`/api/lyrics/${encodeURIComponent(key)}`)
      .then((body) => cache.set(key, Array.isArray(body?.lines) && body.lines.length ? body.lines : null))
      .catch(() => cache.set(key, null));
  }

  /* Draw the line for this frame. `music` is the item on air (or null). */
  function update(nodes, music, now, options = {}) {
    const lines = music && music.meta?.lyrics ? cache.get(music.meta.key) : null;
    if (music && music.meta?.lyrics && !cache.has(music.meta.key) && options.fetcher) {
      load(music.meta.key, options.fetcher);
    }
    const showing = Array.isArray(lines)
      ? lineAt(lines, sourceAt(music, now, options.playbackAt, options.curve)) : { current: null, next: null };
    const visible = Boolean(showing.current || showing.next);
    if (nodes.box.hidden === visible) nodes.box.hidden = !visible;
    if (nodes.line.textContent !== (showing.current || "")) nodes.line.textContent = showing.current || "";
    if (nodes.next.textContent !== (showing.next || "")) nodes.next.textContent = showing.next || "";
    return showing;
  }

  window.RadioLyrics = { lineAt, sourceAt, update, cache };
})();
