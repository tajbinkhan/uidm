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

// The skip and force keys: a link clicked with the skip key held downloads
// in the browser; one clicked with the force key goes to UIDM instead of
// opening. The keys come from UIDM's settings, which the worker keeps in
// storage, so a click is handled at once.
(() => {
  let keys = { skip: "none", force: "none" };
  chrome.storage.local.get("captureKeys").then((s) => s.captureKeys && (keys = s.captureKeys)).catch(() => {});
  chrome.storage.onChanged.addListener((changes) => {
    if (changes.captureKeys && changes.captureKeys.newValue) keys = changes.captureKeys.newValue;
  });
  const held = (k, ev) => (k === "alt" && ev.altKey) || (k === "ctrl" && ev.ctrlKey) || (k === "shift" && ev.shiftKey);

  function onClick(ev) {
    if (ev.button > 1) return;
    const link = ev.composedPath().find((n) => n instanceof HTMLAnchorElement && n.href);
    if (!link || !/^https?:/i.test(link.href)) return;
    if (held(keys.skip, ev)) {
      chrome.runtime.sendMessage({ type: "link:skip" }).catch(() => {});
    } else if (held(keys.force, ev)) {
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

(() => {
  // Frames too small to hold a video (trackers, widgets) are skipped.
  if (window.top !== window && innerWidth < 200) return;

  let enabled = null; // asked once, on the first video
  let host = null;
  let ui = null;
  let current = null; // the video the button belongs to
  let hideTimer = 0;
  let closeTimer = 0; // closes the menu after "Sent to UIDM"
  let frame = 0;
  const encrypted = new WeakSet();

  const ask = (msg) => chrome.runtime.sendMessage(msg).catch(() => null);

  function human(n) {
    if (!n) return "";
    const units = ["B", "KB", "MB", "GB"];
    let v = n;
    let u = 0;
    while (v >= 1024 && u < units.length - 1) {
      v /= 1024;
      u++;
    }
    return `${v.toFixed(u ? 1 : 0)} ${units[u]}`;
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
      .menu { position: absolute; right: 0; top: 32px; width: 300px; max-height: 340px; overflow-y: auto; padding: 6px; border-radius: 8px; background: #1f2328; color: #e6edf3; box-shadow: 0 4px 16px rgba(0,0,0,.5); }
      .head { padding: 4px 6px; color: #9198a1; font-size: 12px; }
      .opt { display: flex; width: 100%; gap: 8px; padding: 6px; border: 0; border-radius: 5px; background: none; color: inherit; text-align: left; cursor: pointer; }
      .opt:hover { background: #30363d; }
      .opt .size { margin-left: auto; color: #9198a1; }
      .note { padding: 6px; color: #d29922; }
    `;
    const button = el("button", "btn", "Download video");
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
    host.style.left = `${Math.max(0, Math.min(r.right, innerWidth) - 136)}px`;
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
    }
    if (!enabled) return;
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
      menu.replaceChildren(el("div", "note", "This video is protected by DRM, so UIDM will not download it."));
      return;
    }
    menu.replaceChildren(el("div", "head", "Looking for the video..."));
    const answer = await ask({ type: "video:menu" });
    menu.replaceChildren();
    if (!answer || !answer.connected) {
      menu.append(el("div", "note", "UIDM is not running or not connected."));
      return;
    }
    if (answer.items.length === 0) {
      menu.append(el("div", "note", "No video found yet. Let it play for a moment, then try again."));
      return;
    }
    for (const item of answer.items) {
      menu.append(el("div", "head", item.label));
      if (item.drm) {
        menu.append(el("div", "note", "Protected by DRM; UIDM will not download it."));
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
              title: document.title,
            });
            menu.replaceChildren(el("div", "head", sent && sent.ok ? "Sent to UIDM." : "Could not reach UIDM."));
            closeTimer = setTimeout(() => (menu.hidden = true), 1500);
          }),
        );
      }
    }
  }

  const isVideo = (t) => t instanceof HTMLVideoElement;
  document.addEventListener("play", (ev) => isVideo(ev.target) && show(ev.target), true);
  document.addEventListener("encrypted", (ev) => isVideo(ev.target) && encrypted.add(ev.target), true);
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
