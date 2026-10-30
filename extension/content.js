// UIDM's video button: a small "Download video" button over a playing video.
// Clicking it lists what the page loaded (qualities, sizes) and sends the
// choice to UIDM through the service worker.
//
// This runs on every page, so it stays small and out of the way: it does
// nothing until a video plays, never touches the page's own elements or
// scripts, and keeps its button in a closed shadow root. Page text (titles,
// labels from the server) is shown with textContent only. A video playing
// through Encrypted Media Extensions is DRM protected: the menu says so and
// offers nothing.

// The mini view's button face: a down arrow, drawn without page text.
function uidmArrow() {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("width", "14");
  svg.setAttribute("height", "14");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(ns, "path");
  path.setAttribute("d", "M8 2v9m-4-4 4 4 4-4M3 14h10");
  path.setAttribute("fill", "none");
  path.setAttribute("stroke", "currentColor");
  path.setAttribute("stroke-width", "2");
  path.setAttribute("stroke-linecap", "round");
  path.setAttribute("stroke-linejoin", "round");
  svg.append(path);
  return svg;
}

// Give `button` the mini view: the arrow alone, its text as a tooltip and
// its accessible name.
function makeMini(button) {
  const label = button.textContent;
  button.replaceChildren(uidmArrow());
  button.title = label;
  button.setAttribute("aria-label", label);
  button.classList.add("mini");
}

// The skip and force keys: a link clicked with the skip keys held downloads
// in the browser; one clicked with the force keys goes to UIDM instead of
// opening. The keys come from UIDM's settings, which the worker keeps in
// storage, so a click is handled at once. rules.js (loaded first) matches
// them.
(() => {
  // The capture_* key settings; none until they arrive.
  let keys = {};
  chrome.storage.local.get("captureKeys").then((s) => s.captureKeys && (keys = s.captureKeys)).catch(() => {});
  chrome.storage.onChanged.addListener((changes) => {
    if (changes.captureKeys && changes.captureKeys.newValue) keys = changes.captureKeys.newValue;
  });
  // Insert is no modifier, so whether it is down is followed by hand.
  let insert = false;
  addEventListener("keydown", (ev) => ev.key === "Insert" && (insert = true), true);
  addEventListener("keyup", (ev) => ev.key === "Insert" && (insert = false), true);
  addEventListener("blur", () => (insert = false));

  function onClick(ev) {
    if (ev.button > 1) return;
    const link = ev.composedPath().find((n) => n instanceof HTMLAnchorElement && n.href);
    if (!link || !/^https?:/i.test(link.href)) return;
    const action = keyAction(keys, {
      alt: ev.altKey,
      ctrl: ev.ctrlKey,
      shift: ev.shiftKey,
      insert,
      button: ev.button,
      // A click made with Enter has a detail of 0.
      mouse: ev.detail > 0,
    });
    if (action === "skip") {
      chrome.runtime.sendMessage({ type: "link:skip" }).catch(() => {});
    } else if (action === "force" && !forceLeavesLink(keys, link.href)) {

      ev.preventDefault();
      ev.stopImmediatePropagation();
      const url = link.href;
      chrome.runtime
        .sendMessage({ type: "link:force", url })
        .then((r) => r && r.ok)
        .catch(() => false)
        .then((ok) => ok || location.assign(url));
    }
  }
  document.addEventListener("click", onClick, true);
  document.addEventListener("auxclick", onClick, true);
})();

// The selection button: selecting text that holds links (or bare web
// addresses) shows a small "Download with UIDM" button by the selection.
// It opens UIDM's link picker with just those links. Whether it shows at
// all is asked once, on the first such selection.
(() => {
  let enabled = null;
  let view = "full";
  let host = null;
  let button = null;
  let timer = 0;

  function build() {
    host = document.createElement("div");
    host.style.cssText = "position:fixed;z-index:2147483647;top:0;left:0;display:none;";
    const root = host.attachShadow({ mode: "closed" });
    const style = document.createElement("style");
    style.textContent = `
      :host { all: initial; }
      button { padding: 4px 9px; border: 0; border-radius: 6px; background: rgba(37,99,235,.95); color: #fff; font: 12px system-ui, sans-serif; cursor: pointer; box-shadow: 0 1px 4px rgba(0,0,0,.35); }
      button:hover { background: #1d4ed8; }
      button.mini { display: grid; place-items: center; width: 26px; height: 26px; padding: 0; border-radius: 50%; }
    `;
    button = document.createElement("button");
    button.textContent = chrome.i18n.getMessage("downloadWithUidm");
    if (view === "mini") makeMini(button);
    // Keep the selection: a press would otherwise clear it.
    button.addEventListener("mousedown", (ev) => ev.preventDefault());
    button.addEventListener("click", (ev) => {
      ev.stopPropagation();
      chrome.runtime.sendMessage({ type: "selection:links" }).catch(() => {});
      hide();
    });
    root.append(style, button);
    (document.body || document.documentElement).append(host);
  }

  function hide() {
    if (host) host.style.display = "none";
  }

  function hasLinks(sel) {
    if (/https?:\/\/\S/.test(String(sel))) return true;
    for (let i = 0; i < sel.rangeCount; i++) {
      const range = sel.getRangeAt(i);
      if (range.cloneContents().querySelector("a[href]")) return true;
      const a = range.commonAncestorContainer;
      const el = a.nodeType === 1 ? a : a.parentElement;
      if (el && el.closest("a[href]")) return true;
    }
    return false;
  }

  async function check() {
    const sel = getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount || !hasLinks(sel)) return hide();
    if (enabled === null) {
      const answer = await chrome.runtime.sendMessage({ type: "selection:enabled" }).catch(() => null);
      enabled = !!(answer && answer.enabled);
      if (answer && answer.view) view = answer.view;
    }
    if (!enabled) return;
    if (!host) build();
    const r = sel.getRangeAt(sel.rangeCount - 1).getBoundingClientRect();
    host.style.display = "block";
    // The button's width depends on the language its label is in.
    host.style.left = `${Math.max(0, Math.min(r.right, innerWidth - button.offsetWidth - 16))}px`;
    host.style.top = `${Math.max(0, Math.min(r.bottom + 6, innerHeight - 30))}px`;
  }

  document.addEventListener("mouseup", (ev) => {
    if (ev.target === host) return;
    clearTimeout(timer);
    timer = setTimeout(check, 10);
  });
  document.addEventListener("keyup", (ev) => ev.shiftKey && check());
  document.addEventListener("selectionchange", () => {
    const sel = getSelection();
    if (!sel || sel.isCollapsed) hide();
  });
  addEventListener("scroll", hide, true);
})();

(() => {
  // Frames too small to hold a video (trackers, widgets) are skipped.
  if (window.top !== window && innerWidth < 200) return;

  let enabled = null; // asked once, on the first video
  let view = "full";
  // Whether a protected video gets the button, to say it is protected.
  let protectedNote = true;
  let host = null;
  let ui = null;
  let current = null; // the video the button belongs to
  let hideTimer = 0;
  let closeTimer = 0; // closes the menu after "Sent to UIDM"
  let frame = 0;
  const encrypted = new WeakSet();

  const ask = (msg) => chrome.runtime.sendMessage(msg).catch(() => null);

  // A size, with numbers written the way the browser's language writes them.
  const SIZE_UNITS = ["sizeBytes", "sizeKB", "sizeMB", "sizeGB"];
  function human(n) {
    if (!n) return "";
    let v = n;
    let u = 0;
    while (v >= 1024 && u < SIZE_UNITS.length - 1) {
      v /= 1024;
      u++;
    }
    const digits = u ? 1 : 0;
    const number = v.toLocaleString(chrome.i18n.getUILanguage(), {
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
      useGrouping: false,
    });
    return chrome.i18n.getMessage(SIZE_UNITS[u], number);
  }

  function el(tag, cls, text) {
    const node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text != null) node.textContent = text;
    return node;
  }

  function build() {
    host = document.createElement("div");
    host.style.cssText = "position:fixed;z-index:2147483647;top:0;left:0;display:none;";
    const root = host.attachShadow({ mode: "closed" });
    const style = el("style");
    style.textContent = `
      :host { all: initial; }
      * { box-sizing: border-box; font: 13px system-ui, sans-serif; }
      .btn { padding: 5px 10px; border: 0; border-radius: 6px; background: rgba(37,99,235,.92); color: #fff; cursor: pointer; box-shadow: 0 1px 4px rgba(0,0,0,.4); }
      .btn:hover { background: #1d4ed8; }
      .btn.mini { display: grid; place-items: center; width: 28px; height: 28px; padding: 0; border-radius: 50%; }
      .menu { position: absolute; right: 0; top: 32px; width: 300px; max-height: 340px; overflow-y: auto; padding: 6px; border-radius: 8px; background: #1f2328; color: #e6edf3; box-shadow: 0 4px 16px rgba(0,0,0,.5); }
      .head { padding: 4px 6px; color: #9198a1; font-size: 12px; }
      .opt { display: flex; width: 100%; gap: 8px; padding: 6px; border: 0; border-radius: 5px; background: none; color: inherit; text-align: left; cursor: pointer; }
      .opt:hover { background: #30363d; }
      .opt .size { margin-left: auto; color: #9198a1; }
      .note { padding: 6px; color: #d29922; }
    `;
    const button = el("button", "btn", chrome.i18n.getMessage("downloadVideo"));
    if (view === "mini") makeMini(button);
    const menu = el("div", "menu");
    menu.hidden = true;
    button.addEventListener("click", (ev) => {
      ev.stopPropagation();
      if (menu.hidden) openMenu();
      else menu.hidden = true;
    });
    root.append(style, button, menu);
    ui = { button, menu };
    (document.body || document.documentElement).append(host);
  }

  // Keep the button at the video's top right while it is on screen.
  function place() {
    frame = 0;
    if (!current || !current.isConnected) return hide();
    const r = current.getBoundingClientRect();
    if (r.width < 160 || r.height < 90 || r.bottom < 0 || r.top > innerHeight) {
      host.style.display = "none";
      return;
    }
    host.style.display = "block";
    // The button's width depends on the language its label is in.
    host.style.left = `${Math.max(0, Math.min(r.right, innerWidth) - ui.button.offsetWidth - 20)}px`;
    host.style.top = `${Math.max(0, r.top) + 8}px`;
  }

  function schedule() {
    if (!frame && current) frame = requestAnimationFrame(place);
  }

  function hide() {
    current = null;
    if (host) {
      host.style.display = "none";
      ui.menu.hidden = true;
    }
  }

  async function show(video) {
    if (enabled === null) {
      const answer = await ask({ type: "video:enabled" });
      enabled = !!(answer && answer.enabled);
      if (answer && answer.view) view = answer.view;
      if (answer && answer.protectedNote === false) protectedNote = false;
    }
    if (!enabled) return;
    if (!protectedNote && (encrypted.has(video) || video.mediaKeys)) {
      if (current === video) hide();
      return;
    }
    if (!host) build();
    if (current !== video) ui.menu.hidden = true;
    current = video;
    clearTimeout(hideTimer);
    place();
  }

  function protectedVideo() {
    return !!current && (encrypted.has(current) || !!current.mediaKeys);
  }

  function option(label, size, onClick) {
    const b = el("button", "opt");
    b.append(el("span", "", label));
    if (size) b.append(el("span", "size", human(size)));
    b.addEventListener("click", (ev) => {
      ev.stopPropagation();
      onClick();
    });
    return b;
  }

  async function openMenu() {
    const menu = ui.menu;
    clearTimeout(closeTimer);
    menu.hidden = false;
    if (protectedVideo()) {
      menu.replaceChildren(el("div", "note", chrome.i18n.getMessage("videoProtected")));
      return;
    }
    menu.replaceChildren(el("div", "head", chrome.i18n.getMessage("lookingForVideo")));
    const answer = await ask({ type: "video:menu" });
    menu.replaceChildren();
    if (!answer || !answer.connected) {
      menu.append(el("div", "note", chrome.i18n.getMessage("uidmNotConnected")));
      return;
    }
    if (answer.items.length === 0) {
      menu.append(el("div", "note", chrome.i18n.getMessage("noVideoFound")));
      return;
    }
    for (const item of answer.items) {
      menu.append(el("div", "head", item.label));
      if (item.drm) {
        menu.append(el("div", "note", chrome.i18n.getMessage("streamProtected")));
        continue;
      }
      if (item.error) {
        menu.append(el("div", "note", item.error));
        continue;
      }
      for (const o of item.options) {
        menu.append(
          option(o.label, o.size, async () => {
            const sent = await ask({
              type: "video:download",
              entry: item.entry,
              video: o.video,
              audio: o.audio,
              audioOnly: o.audioOnly,
              title: document.title,
            });
            const result = sent && sent.ok ? chrome.i18n.getMessage("sentToUidm") : chrome.i18n.getMessage("couldNotReachUidm");
            menu.replaceChildren(el("div", "head", result));
            closeTimer = setTimeout(() => (menu.hidden = true), 1500);
          }),
        );
      }
    }
  }

  const isVideo = (t) => t instanceof HTMLVideoElement;
  document.addEventListener("play", (ev) => isVideo(ev.target) && show(ev.target), true);
  document.addEventListener(
    "encrypted",
    (ev) => {
      if (!isVideo(ev.target)) return;
      encrypted.add(ev.target);
      if (!protectedNote && ev.target === current) hide();
    },
    true,
  );
  document.addEventListener(
    "mouseover",
    (ev) => {
      if (isVideo(ev.target) && ev.target.readyState > 0) show(ev.target);
    },
    true,
  );
  document.addEventListener(
    "mouseout",
    (ev) => {
      if (ev.target !== current || ev.relatedTarget === host || !current.paused) return;
      clearTimeout(hideTimer);
      hideTimer = setTimeout(() => ui.menu.hidden && hide(), 2500);
    },
    true,
  );
  document.addEventListener("click", (ev) => {
    if (ui && ev.target !== host) ui.menu.hidden = true;
  });
  addEventListener("scroll", schedule, true);
  addEventListener("resize", schedule);
  document.addEventListener("fullscreenchange", schedule);
})();
