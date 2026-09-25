/* The first-run notice. Once per device, and again whenever TERMS.md gets a
   new terms version, it asks for the terms before anything can play. The
   version comes from the page, which the server fills in from TERMS.md. */
(() => {
  "use strict";
  const dialog = document.getElementById("legal-window");
  if (!dialog) return;
  const version = dialog.dataset.termsVersion || "";
  const KEY = "defalt.legal.accepted";
  const reduced = document.getElementById("legal-reduced");
  const booth = document.getElementById("reduced");
  let agreedHere = false;   // counts even where storage is blocked

  function stored() {
    try { return localStorage.getItem(KEY); } catch { return null; }
  }
  function accepted() {
    return agreedHere || (!!version && stored() === version);
  }
  function show() {
    if (accepted() || dialog.open) return;
    if (reduced && booth) reduced.checked = booth.checked;
    dialog.showModal();
    document.body.classList.add("reading-info");
  }
  function agree() {
    agreedHere = true;
    try { localStorage.setItem(KEY, version); } catch {}
    dialog.close();
  }

  document.getElementById("legal-agree").addEventListener("click", agree);
  // Escape doesn't count as agreeing.
  dialog.addEventListener("cancel", event => event.preventDefault());
  dialog.addEventListener("close", () => {
    document.body.classList.remove("reading-info");
    if (!accepted()) show();
  });
  // Playback shortcuts stay with the page underneath.
  dialog.addEventListener("keydown", event => event.stopPropagation());
  // The same switch as Studio settings, so the booth and its saved setting follow.
  reduced?.addEventListener("change", () => {
    if (!booth || booth.checked === reduced.checked) return;
    booth.checked = reduced.checked;
    booth.dispatchEvent(new Event("change"));
  });

  globalThis.DefaltLegal = {accepted, show};
  show();
})();
