/* Release pages never touch the player or its schedule. */
(() => {
  "use strict";
  const dialog = document.getElementById("info-window");
  const body = document.getElementById("info-document");
  const title = document.getElementById("info-title");
  const labels = {
    patches: "Patch notes", privacy: "Privacy policy", terms: "Terms of use", copyright: "Copyright",
    license: "License", notices: "Third-party notices",
  };
  let documents;
  let active;
  let opener;
  let request;

  function paragraph(tag, text, parent = body) {
    const element = document.createElement(tag);
    element.textContent = text;
    parent.append(element);
    return element;
  }

  function render(text) {
    body.replaceChildren();
    let list;
    for (const raw of text.split("\n---\n")[0].split("\n")) {
      const line = raw.trim();
      if (!line) { list = null; continue; }
      const heading = /^(#{1,3}) (.+)$/.exec(line);
      if (heading) {
        paragraph(`h${Number(heading[1].length) + 1}`, heading[2]);
        list = null;
      } else if (line.startsWith("- ")) {
        list ||= paragraph("ul", "");
        paragraph("li", line.slice(2), list);
      } else {
        paragraph("p", line);
        list = null;
      }
    }
    body.scrollTop = 0;
  }

  async function open(page, source) {
    if (!Object.hasOwn(labels, page)) return;
    active = page;
    if (!dialog.open) {
      opener = source;
      dialog.showModal();
      document.body.classList.add("reading-info");
    }
    title.textContent = labels[page];
    body.setAttribute("aria-label", labels[page]);
    dialog.querySelectorAll("[data-info-page]").forEach(button => {
      button.setAttribute("aria-current", button.dataset.infoPage === page ? "page" : "false");
    });
    body.replaceChildren();
    paragraph("p", "Loading…");
    try {
      if (!documents) {
        request ||= fetch("/api/about", {cache: "no-store", signal: AbortSignal.timeout(10000)})
          .then(response => {
            if (!response.ok) throw new Error("Document request failed");
            return response.json();
          }).then(data => {
            if (!data.documents || Object.keys(labels).some(key => typeof data.documents[key] !== "string")) {
              throw new Error("Incomplete documents");
            }
            documents = data.documents;
          }).finally(() => { request = null; });
        await request;
      }
      if (active === page && dialog.open) render(documents[page]);
    } catch {
      if (active !== page || !dialog.open) return;
      body.replaceChildren();
      paragraph("p", "Couldn’t load this page. Check that the local server is running, then try again.");
      const retry = paragraph("button", "Try again");
      retry.type = "button";
      retry.addEventListener("click", () => open(page, source));
    }
  }

  document.querySelectorAll("[data-info-page]").forEach(button => {
    button.addEventListener("click", () => open(button.dataset.infoPage, button));
  });
  document.getElementById("info-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("close", () => {
    document.body.classList.remove("reading-info");
    opener?.focus();
  });
  dialog.addEventListener("keydown", event => event.stopPropagation());
})();
