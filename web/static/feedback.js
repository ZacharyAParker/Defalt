/* Report a bug or suggestion. Filed to reports/ by the station for review;
   see docs/FEEDBACK.md. Loaded first so it hears errors from everything after. */
(() => {
  "use strict";
  const KEEP = 200;
  const heard = [];

  function words(value) {
    if (value instanceof Error) return `${value.name}: ${value.message}${value.stack ? `\n${value.stack}` : ""}`;
    if (typeof value === "string") return value;
    try { return JSON.stringify(value); } catch { return String(value); }
  }

  // The page's own complaints, stamped, for the report's logs.txt.
  function hear(level, parts) {
    const line = `${new Date().toISOString()} [${level}] ${parts.map(words).join(" ")}`;
    for (const piece of line.slice(0, 4000).split("\n")) heard.push(piece);
    while (heard.length > KEEP) heard.shift();
  }

  for (const level of ["error", "warn"]) {
    const original = console[level];
    if (typeof original !== "function") continue;
    console[level] = (...parts) => {
      hear(level, parts);
      return original.apply(console, parts);
    };
  }
  if (typeof window !== "undefined" && window.addEventListener) {
    window.addEventListener("error", event => hear("error", [event.error || event.message || "error"]));
    window.addEventListener("unhandledrejection", event => hear("error", ["unhandled rejection:", event.reason]));
  }

  function el(tag, props = {}, ...children) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(props)) {
      if (key === "text") node.textContent = value;
      else if (key in node || key === "className") node[key] = value;
      else node.setAttribute(key, value);
    }
    node.append(...children);
    return node;
  }

  const kindBug = el("input", {type: "radio", name: "feedback-kind", value: "bug", checked: true, id: "feedback-kind-bug"});
  const kindIdea = el("input", {type: "radio", name: "feedback-kind", value: "suggestion", id: "feedback-kind-idea"});
  const title = el("input", {type: "text", id: "feedback-title", maxLength: 200, autocomplete: "off",
    placeholder: "Skip stopped the music"});
  const description = el("textarea", {id: "feedback-description", rows: 5, maxLength: 20000});
  const expected = el("textarea", {id: "feedback-expected", rows: 2, maxLength: 20000});
  const expectedField = el("div", {className: "feedback-field"},
    el("label", {htmlFor: "feedback-expected", text: "What I expected (optional)"}), expected);
  const describeLabel = el("label", {htmlFor: "feedback-description", text: "What happened"});
  const attach = el("input", {type: "checkbox", id: "feedback-logs", checked: true});
  const status = el("p", {className: "feedback-status", role: "status"});
  const send = el("button", {type: "submit", className: "btn btn--primary", text: "File report"});
  const cancel = el("button", {type: "button", className: "btn", text: "Cancel"});
  const form = el("form", {className: "feedback-form", method: "dialog"},
    el("fieldset", {className: "feedback-kind"},
      el("legend", {text: "Type"}),
      el("label", {}, kindBug, " Bug"),
      el("label", {}, kindIdea, " Suggestion")),
    el("div", {className: "feedback-field"}, el("label", {htmlFor: "feedback-title", text: "Title"}), title),
    el("div", {className: "feedback-field"}, describeLabel, description),
    expectedField,
    el("label", {className: "feedback-check"}, attach, " Attach logs from the last ten minutes"),
    el("div", {className: "feedback-actions"}, send, cancel, status));
  const dialog = el("dialog", {className: "feedback", id: "feedback-dialog", "aria-labelledby": "feedback-heading"},
    el("h2", {id: "feedback-heading", text: "Report a bug or suggestion"}),
    el("p", {className: "feedback-hint", text: "Saved to reports/ on this computer, with the station’s state and recent logs."}),
    form);
  document.body.append(dialog);

  let opener = null;
  let sending = false;

  function kind() { return kindIdea.checked ? "suggestion" : "bug"; }

  function shape() {
    const bug = kind() === "bug";
    expectedField.hidden = !bug;
    describeLabel.textContent = bug ? "What happened" : "The idea";
    title.placeholder = bug ? "Skip stopped the music" : "Show the next record’s key";
  }

  function text(id) {
    const node = document.getElementById(id);
    return node ? String(node.textContent || "").trim() : null;
  }

  // What this page can see; the station adds its own side.
  function context() {
    const audio = Array.from(document.querySelectorAll("audio") || []).map(player => ({
      paused: player.paused, time: Math.round((player.currentTime || 0) * 10) / 10,
      ready: player.readyState, network: player.networkState, muted: player.muted,
      volume: player.volume, error: player.error ? player.error.code : null,
      src: String(player.currentSrc || "").split("?")[0],
    }));
    return {
      page: location.pathname, agent: navigator.userAgent,
      viewport: `${window.innerWidth}x${window.innerHeight}`,
      visible: document.visibilityState, online: navigator.onLine,
      showing: {title: text("now-title"), artist: text("now-artist"), state: text("state-value"),
                engine: text("engine-value"), host: text("host-pill")},
      audio,
    };
  }

  function open(source) {
    if (dialog.open) return;
    opener = source || null;
    status.textContent = "";
    shape();
    dialog.showModal();
    title.focus();
  }

  async function submit(event) {
    event?.preventDefault();
    if (sending) return;
    if (!title.value.trim() && !description.value.trim()) {
      status.textContent = "Give it a title or describe what happened.";
      return;
    }
    sending = true;
    send.disabled = true;
    status.textContent = "Filing…";
    try {
      const response = await fetch("/api/feedback", {
        method: "POST", headers: {"Content-Type": "application/json"},
        signal: AbortSignal.timeout(20000),
        body: JSON.stringify({
          kind: kind(), title: title.value.trim(), description: description.value.trim(),
          expected: kind() === "bug" ? expected.value.trim() : "", attach_logs: attach.checked,
          client: "browser", client_context: context(), client_logs: attach.checked ? heard.slice() : [],
        }),
      });
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(body.error || `The station answered ${response.status}.`);
      title.value = "";
      description.value = "";
      expected.value = "";
      status.textContent = `Filed as ${body.id}.`;
      dialog.close();
      const toast = document.getElementById("toast");
      if (toast) {
        toast.textContent = `Report filed: ${body.path}`;
        toast.hidden = false;
        setTimeout(() => { toast.hidden = true; }, 4000);
      }
    } catch (error) {
      const offline = error && (error.name === "TypeError" || error.name === "TimeoutError");
      status.textContent = offline
        ? "Couldn’t reach the station. Your report is still here; try again once it’s running."
        : error.message;
    } finally {
      sending = false;
      send.disabled = false;
    }
  }

  form.addEventListener("submit", submit);
  kindBug.addEventListener("change", shape);
  kindIdea.addEventListener("change", shape);
  cancel.addEventListener("click", () => dialog.close());
  dialog.addEventListener("close", () => opener?.focus?.());
  // Typing here must never reach the player's shortcuts.
  dialog.addEventListener("keydown", event => {
    event.stopPropagation();
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) submit(event);
  });
  const button = document.getElementById("feedback-open");
  if (button) button.addEventListener("click", () => open(button));
})();
