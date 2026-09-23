/* The local output, from bass at the left to treble at the right. */
(() => {
  "use strict";
  const count = 48, levels = new Float32Array(count), peaks = new Float32Array(count);
  const toggle = document.getElementById("visualizer-enabled");
  const panel = document.getElementById("audio-visualizer");
  if (!toggle || !panel) return;
  try { toggle.checked = localStorage.getItem("defalt.visualizer") !== "off"; } catch (_) {}
  function visibility() {
    panel.hidden = !toggle.checked;
    try { localStorage.setItem("defalt.visualizer", toggle.checked ? "on" : "off"); } catch (_) {}
  }
  toggle.addEventListener("change", visibility);
  visibility();
  let previous = 0, bins;
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
  // The canvas box, kept current by a ResizeObserver: measuring it each frame
  // with getBoundingClientRect would force a layout every time.
  let box = null;
  const observer = typeof window.ResizeObserver === "function"
    ? new window.ResizeObserver(entries => { const r = entries[0].contentRect; box = {width: r.width, height: r.height}; })
    : null;
  let observed = null;
  function measure(canvas) {
    if (observer) {
      if (observed !== canvas) { observed = canvas; box = null; observer.observe(canvas); }
      if (box) return box;
    }
    const rect = canvas.getBoundingClientRect();
    if (observer) box = {width: rect.width, height: rect.height};
    return rect;
  }
  // Bass energy, 0..1, for the studio to breathe with. Worked out on its own
  // (the visualizer may be switched off) and at most 15 times a second.
  let low = 0, lowAt = 0, lowBins;
  function lowBand(analyser, now = performance.now()) {
    if (now - lowAt < 66) return low;
    const dt = Math.min(.5, (now - lowAt) / 1000);
    lowAt = now;
    let value = 0;
    if (analyser) {
      if (!lowBins || lowBins.length !== analyser.frequencyBinCount) lowBins = new Float32Array(analyser.frequencyBinCount);
      analyser.getFloatFrequencyData(lowBins);
      const rate = analyser.context.sampleRate || 48000;
      const lo = Math.max(1, Math.floor(40 * analyser.fftSize / rate)), hi = Math.min(lowBins.length, Math.ceil(160 * analyser.fftSize / rate) + 1);
      let db = -Infinity;
      for (let b = lo; b < hi; b++) if (Number.isFinite(lowBins[b])) db = Math.max(db, lowBins[b]);
      value = Math.max(0, Math.min(1, (db + 60) / 50));
    }
    low += (value - low) * (1 - Math.exp(-dt / (value > low ? .08 : .5)));
    return low;
  }
  window.RadioVisualizer = {
    lowBand,
    draw(canvas, analyser, now = performance.now()) {
      if (!toggle.checked || document.hidden) return;
      const calm = reduced.matches || document.getElementById("reduced")?.checked;
      const interval = calm ? 100 : 1000 / 30;
      if (now - previous < interval) return;
      const dt = Math.min(.2, (now - previous) / 1000);
      previous = now;
      const rect = measure(canvas);
      if (rect.width < 1 || rect.height < 1) return;
      const ratio = Math.min(2, window.devicePixelRatio || 1);
      const w = rect.width, h = rect.height;
      if (canvas.width !== Math.round(w * ratio) || canvas.height !== Math.round(h * ratio)) {
        canvas.width = Math.round(w * ratio); canvas.height = Math.round(h * ratio);
      }
      const g = canvas.getContext("2d");
      g.setTransform(ratio, 0, 0, ratio, 0, 0);
      g.clearRect(0, 0, w, h);
      if (analyser) {
        if (!bins || bins.length !== analyser.frequencyBinCount) bins = new Float32Array(analyser.frequencyBinCount);
        analyser.getFloatFrequencyData(bins);
      }
      const rate = analyser?.context.sampleRate || 48000;
      const high = Math.min(16000, rate * .45);
      const baseline = h - 8, available = Math.max(0, h - 12), step = w / count;
      const bar = Math.max(1, step * .72);
      for (let i = 0; i < count; i++) {
        let value = 0;
        if (analyser) {
          const lo = Math.max(1, Math.floor(40 * (high / 40) ** (i / count) * analyser.fftSize / rate));
          const hi = Math.min(bins.length, Math.max(lo + 1, Math.ceil(40 * (high / 40) ** ((i + 1) / count) * analyser.fftSize / rate)));
          let db = -Infinity;
          for (let b = lo; b < hi; b++) if (Number.isFinite(bins[b])) db = Math.max(db, bins[b]);
          value = Math.max(0, Math.min(1, (db + 66) / 66));
        }
        const response = calm ? .3 : value > levels[i] ? .045 : .24;
        levels[i] += (value - levels[i]) * (1 - Math.exp(-dt / response));
        peaks[i] = Math.max(levels[i], peaks[i] - dt * .6);
        const x = (i + .5) * step - bar / 2, height = Math.max(2, levels[i] * available), t = i / (count - 1);
        const color = `rgb(${Math.round(239 - t * 150)},${Math.round(189 + t * 13)},${Math.round(113 + t * 111)})`;
        g.fillStyle = color; g.globalAlpha = .85;
        g.fillRect(x, baseline - height, bar, height);
        if (!calm) {
          g.globalAlpha = .16; g.fillRect(x, baseline + 3, bar, height * .12);
          if (levels[i] > .035) {
            g.globalAlpha = 1; g.fillStyle = "#f5e0be";
            g.fillRect(x, baseline - peaks[i] * available, bar, 1);
          }
        }
      }
      g.globalAlpha = 1;
    }
  };
})();
