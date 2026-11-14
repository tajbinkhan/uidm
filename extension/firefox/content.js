// UDM's video button: a small "Download video" button over a playing video
// or audio player (or an <object>/<embed> one).
// Clicking it lists what the page loaded (qualities, sizes) and sends the
// choice to UDM through the service worker.
//
// This runs on every page, so it stays small and out of the way: it does
// nothing until a video plays, never touches the page's own elements or
// scripts, and keeps its button in a closed shadow root. Page text (titles,
// labels from the server) is shown with textContent only. A video playing
// through Encrypted Media Extensions is DRM protected: the menu says so and
// offers nothing.

// A tab open when UDM was installed or connected gets this script from the
// worker (injectOpenTabs, in background.js), which skips frames that have
// it; should a frame get it twice anyway, the second copy does nothing, so
// nothing listens twice.
var udmContentAgain = globalThis.udmContentLoaded === true;
globalThis.udmContentLoaded = true;

// What this page calls itself, for naming its videos (videoName, in
// media.js): its title, og:title and og:site_name. The worker asks the top
// frame for it when a video in a frame is downloaded.
function pageNames() {
  const meta = (p) => {
    const m = document.querySelector(`meta[property="${p}"], meta[name="${p}"]`);
    return m ? String(m.getAttribute("content") || "").slice(0, 500) : "";
  };
  return { title: document.title, ogTitle: meta("og:title"), siteName: meta("og:site_name") };
}

// The video button's "?": the help page on streams and the video button.
var VIDEO_HELP = "https://github.com/tajbinkhan/UDM/blob/main/docs/streams-batches-and-grabber.md";

// The mini view's button face: a down arrow, drawn without page text.
function udmArrow() {
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
  button.replaceChildren(udmArrow());
  button.title = label;
  button.setAttribute("aria-label", label);
  button.classList.add("mini");
}

// The skip and force keys: a link clicked with the skip keys held downloads
// in the browser; one clicked with the force keys goes to UDM instead of
// opening. The keys come from UDM's settings, which the worker keeps in
// storage, so a click is handled at once. rules.js (loaded first) matches
// them.
(() => {
  if (udmContentAgain) return;
  // The capture_* key settings; none until they arrive.
  let keys = {};
  // Whether magnet links go to UDM as they are clicked: UDM is connected
  // and capture is on (the worker shares it as `magnetCapture`).
  let magnetCapture = false;
  // Where the browser has no downloads API (Safari), whether links to files
  // are taken as they are clicked, and the capture rules: { on, rules }, as
  // the worker shares them (shareLinkCapture); none elsewhere.
  let linkCapture = null;
  chrome.storage.local
    .get(["captureKeys", "linkCapture", "magnetCapture"])
    .then((s) => {
      if (s.captureKeys) keys = s.captureKeys;
      if (s.linkCapture) linkCapture = s.linkCapture;
      magnetCapture = !!s.magnetCapture;
    })
    .catch(() => {});
  chrome.storage.onChanged.addListener((changes) => {
    if (changes.captureKeys && changes.captureKeys.newValue) keys = changes.captureKeys.newValue;
    if (changes.linkCapture) linkCapture = changes.linkCapture.newValue || null;
    if (changes.magnetCapture) magnetCapture = !!changes.magnetCapture.newValue;
  });
  // Insert and Del are no modifiers, so whether they are down is followed
  // by hand (trackKey, in rules.js).
  const held = { insert: false, del: false };
  // What the held keys ask for goes to the worker too, which applies it to
  // any download this tab starts while they are down: a form, a script's
  // download (heldReport, in rules.js, sends only changes and refreshes).
  // Whether only a left click counts does not matter here: no click.
  const report = { sent: "", time: 0 };
  function reportHeld(ev) {
    const action = ev
      ? keyAction(
          { ...keys, capture_keys_left_click: false },
          { alt: ev.altKey, ctrl: ev.ctrlKey, shift: ev.shiftKey, insert: held.insert, del: held.del },
        )
      : "";
    if (heldReport(report, action, Date.now())) chrome.runtime.sendMessage({ type: "keys:held", action }).catch(() => {});
  }
  // A page's script can send key events; only the user's own count.
  addEventListener(
    "keydown",
    (ev) => {
      trackKey(held, "keydown", ev.key);
      if (ev.isTrusted) reportHeld(ev);
    },
    true,
  );
  addEventListener(
    "keyup",
    (ev) => {
      trackKey(held, "keyup", ev.key);
      if (ev.isTrusted) reportHeld(ev);
    },
    true,
  );
  // Keys let go while the page has no focus are never seen: forget them.
  const letGo = () => {
    trackKey(held, "blur");
    reportHeld(null);
  };
  addEventListener("blur", letGo);
  document.addEventListener("visibilitychange", () => document.visibilityState === "hidden" && letGo());

  function onClick(ev) {
    // A page's script can send a click with the force key "held"; only the
    // user's own clicks count.
    if (!ev.isTrusted || ev.button > 1) return;
    const link = ev.composedPath().find((n) => n instanceof HTMLAnchorElement && n.href);
    if (!link) return;
    const magnet = isMagnet(link.href);
    if (!magnet && !/^https?:/i.test(link.href)) return;
    const action = keyAction(keys, {
      alt: ev.altKey,
      ctrl: ev.ctrlKey,
      shift: ev.shiftKey,
      insert: held.insert,
      del: held.del,
      button: ev.button,
      // A click made with Enter has a detail of 0.
      mouse: ev.detail > 0,
    });
    if (magnet) {
      // A magnet link is no download the browser reports: UDM takes it as
      // it is clicked, or the browser opens it as it would have, with the
      // program it hands magnet links to (the skip keys, a middle click, or
      // UDM not there).
      if (action === "skip" || ev.button !== 0 || !magnetCapture) return;
      ev.preventDefault();
      ev.stopImmediatePropagation();
      chrome.runtime
        .sendMessage({ type: "link:magnet", url: link.href })
        .then((r) => r && r.ok)
        .catch(() => false)
        .then((ok) => ok || link.click());
    } else if (action === "skip") {
      chrome.runtime.sendMessage({ type: "link:skip" }).catch(() => {});
    } else if (action === "" && linkCapture && linkCapture.on && clickTaken(linkCapture.rules, link.href, link.getAttribute("download"), location.href)) {
      // Safari: the worker hands it to UDM, or the link is clicked again
      // for the browser to follow as it would have (its target, its
      // download attribute); that click is not the user's, so it is not
      // taken again.
      ev.preventDefault();
      ev.stopImmediatePropagation();
      chrome.runtime
        .sendMessage({ type: "link:capture", url: link.href, download: link.getAttribute("download") })
        .then((r) => r && r.ok)
        .catch(() => false)
        .then((ok) => ok || link.click());
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

// A form with file fields tells the service worker which they are as it is
// sent: the browser shares a multipart form's text fields with extensions
// but not its files, so a download the form starts is rebuilt without
// them, or left in the browser when a file goes along (see post.js).
if (!udmContentAgain) document.addEventListener(
  "submit",
  (ev) => {
    const form = ev.target;
    if (!(form instanceof HTMLFormElement)) return;
    const files = formFileFields(form.elements);
    if (!files) return;
    const action = (ev.submitter && ev.submitter.formAction) || form.action;
    chrome.runtime.sendMessage({ type: "form:files", url: action, ...files }).catch(() => {});
  },
  true,
);

// Whether selection `sel` (this document's) holds anything "Download
// selected links" would collect: the same extraction (collectPageLinks, in
// links.js), so the menu and the button show just when there is something.
function selectionHasLinks(sel) {
  const box = document.activeElement;
  const inBox = box && /^(TEXTAREA|INPUT)$/.test(box.tagName) && box.selectionEnd > box.selectionStart;
  if ((!sel || sel.isCollapsed) && !inBox && !document.querySelector("iframe:not([src]), iframe[src^='about:' i], frame:not([src])")) return false;
  return selectionHasPageLinks(document);
}

// What the context menu asks this frame: the right-clicked link's
// `download` attribute and title (the menu only knows its address), and
// whether the selection holds links (Firefox hides "Download selected
// links" when not). The link is the last one right-clicked, and is only
// described when it is the menu's link. The top frame also says what the
// page calls itself (pageNames).
(() => {
  if (udmContentAgain) return;
  let menuLink = null;
  document.addEventListener(
    "contextmenu",
    (ev) => {
      menuLink = ev.composedPath().find((n) => (n instanceof HTMLAnchorElement || n instanceof HTMLAreaElement) && n.href) || null;
    },
    true,
  );
  chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (msg && msg.type === "link:details") {
      const a = menuLink && menuLink.href === msg.url ? menuLink : null;
      sendResponse(
        a
          ? {
              download: a.getAttribute("download") || "",
              title: a.getAttribute("title") || "",
              text: String(a.textContent || a.getAttribute("alt") || "").slice(0, 1000),
            }
          : null,
      );
    } else if (msg && msg.type === "selection:hasLinks") {
      sendResponse({ links: selectionHasLinks(getSelection()) });
    } else if (msg && msg.type === "page:names") {
      sendResponse(pageNames());
    }
    return false;
  });
})();

// The selection button: selecting text that holds links (or bare web
// addresses) shows a small "Download with UDM" button by the selection.
// It opens UDM's link picker with just those links. Whether it shows at
// all is asked once, on the first such selection.
(() => {
  if (udmContentAgain) return;
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
    button.textContent = chrome.i18n.getMessage("downloadWithUdm");
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

  async function check() {
    const sel = getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount || !selectionHasLinks(sel)) return hide();
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
  if (udmContentAgain) return;
  // Frames too small to hold a video (trackers, widgets) are skipped.
  if (window.top !== window && innerWidth < 200) return;

  let enabled = null; // asked once, on the first video
  let view = "full";
  // Whether a protected video gets the button, to say it is protected.
  let protectedNote = true;
  let host = null;
  let ui = null;
  let current = null; // the player the button belongs to
  let hideTimer = 0;
  let closeTimer = 0; // closes the menu after "Sent to UDM"
  let frame = 0;
  // While the button shows, where it stands is looked at again now and
  // then: a dialog may open over the player without a scroll or resize.
  let recheck = 0;
  const encrypted = new WeakSet();
  // Each player's number, which the worker matches the streams it saw to
  // (see elementEntries in media.js), and when each last said it started.
  const ids = new WeakMap();
  let nextId = 1;
  const startSent = new WeakMap();

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

  // The players the button goes on: <video> and <audio>, and an <object> or
  // <embed> that plays media (mediaPlugin, in rules.js).
  const isMediaElement = (t) => t instanceof HTMLVideoElement || t instanceof HTMLAudioElement;
  function isPlayer(t) {
    if (isMediaElement(t)) return true;
    if (t instanceof HTMLObjectElement) return mediaPlugin(t.type, t.data);
    if (t instanceof HTMLEmbedElement) return mediaPlugin(t.type, t.src);
    return false;
  }

  function idOf(player) {
    if (!ids.has(player)) ids.set(player, nextId++);
    return ids.get(player);
  }

  // The addresses a player loads from itself (a blob: address, which MSE
  // players use, is left to the worker to ignore).
  function sources(player) {
    if (player instanceof HTMLObjectElement) return [player.data];
    if (player instanceof HTMLEmbedElement) return [player.src];
    const out = [player.currentSrc, player.src];
    for (const s of player.querySelectorAll("source")) out.push(s.src);
    return out.filter(Boolean);
  }

  // Tell the worker that `player` started loading or playing, so the
  // playlists this frame asks for from now on are taken as its own. Once a
  // second per player is plenty.
  function started(player) {
    const now = Date.now();
    if (now - (startSent.get(player) || 0) < 1000) return;
    startSent.set(player, now);
    chrome.runtime.sendMessage({ type: "video:started", element: idOf(player) }).catch(() => {});
  }

  // Players whose button was closed with its "x"; they get none until the
  // page loads again.
  const closed = new WeakSet();

  function build() {
    host = document.createElement("div");
    host.style.cssText = "position:fixed;z-index:2147483647;top:0;left:0;display:none;";
    const root = host.attachShadow({ mode: "closed" });
    const style = el("style");
    style.textContent = `
      :host { all: initial; }
      * { box-sizing: border-box; font: 13px system-ui, sans-serif; }
      .bar { display: inline-flex; align-items: center; gap: 3px; padding: 2px; border: 1px solid #7f9dc4; border-radius: 3px; background: linear-gradient(#f5f9fe, #d3e2f4); box-shadow: 0 1px 3px rgba(0,0,0,.4); }
      .btn, .small { border: 1px solid #6f8fba; border-radius: 2px; background: linear-gradient(#ffffff, #cddcf0); color: #0d2a52; cursor: pointer; }
      .btn:hover, .small:hover { background: linear-gradient(#ffffff, #b4cbec); }
      .btn { display: inline-flex; align-items: center; gap: 6px; height: 22px; padding: 0 9px 0 6px; font-weight: 600; font-size: 12px; white-space: nowrap; }
      .play { width: 0; height: 0; border-left: 10px solid #1e9b33; border-top: 6px solid transparent; border-bottom: 6px solid transparent; }
      .small { display: grid; place-items: center; width: 18px; height: 18px; padding: 0; font-size: 12px; font-weight: 700; line-height: 1; }
      .btn.mini { display: grid; place-items: center; width: 22px; padding: 0; }
      .menu { position: absolute; right: 0; top: 30px; width: 300px; max-height: 340px; overflow-y: auto; padding: 6px; border-radius: 8px; background: #1f2328; color: #e6edf3; box-shadow: 0 4px 16px rgba(0,0,0,.5); }
      .head { padding: 4px 6px; color: #9198a1; font-size: 12px; }
      .opt { display: flex; width: 100%; gap: 8px; padding: 6px; border: 0; border-radius: 5px; background: none; color: inherit; text-align: left; cursor: pointer; }
      .opt:hover { background: #30363d; }
      .opt .size { margin-left: auto; color: #9198a1; }
      .note { padding: 6px; color: #d29922; }
      .subs { display: flex; flex-wrap: wrap; gap: 4px 10px; padding: 2px 6px 6px; color: #9198a1; font-size: 12px; }
      .subs label { display: inline-flex; align-items: center; gap: 4px; color: #e6edf3; cursor: pointer; }
    `;
    // IDM's look: a bar with the button, "?" for help and "x" to close it
    // for this player.
    const bar = el("div", "bar");
    const button = el("button", "btn", chrome.i18n.getMessage("downloadVideo"));
    if (view === "mini") makeMini(button);
    else button.prepend(el("span", "play"));
    const help = el("button", "small", "?");
    help.title = chrome.i18n.getMessage("videoButtonHelp");
    const close = el("button", "small", "×");
    close.title = chrome.i18n.getMessage("videoButtonClose");
    bar.append(button, help, close);
    const menu = el("div", "menu");
    menu.hidden = true;
    button.addEventListener("click", (ev) => {
      ev.stopPropagation();
      if (menu.hidden) openMenu();
      else menu.hidden = true;
    });
    help.addEventListener("click", (ev) => {
      ev.stopPropagation();
      window.open(VIDEO_HELP, "_blank", "noopener");
    });
    close.addEventListener("click", (ev) => {
      ev.stopPropagation();
      if (current) closed.add(current);
      hide();
    });
    root.append(style, bar, menu);
    ui = { bar, button, menu };
    (document.body || document.documentElement).append(host);
  }

  // Whether the button has no business showing now, though its player is
  // still there: the tab is in the background or its window minimized
  // (document.hidden), something is full screen (the button cannot be over
  // it), or the player is covered by something else on the page.
  function outOfSight(r) {
    if (document.hidden || document.fullscreenElement || document.webkitFullscreenElement) return true;
    // While its menu is open the button stays: the menu itself is on top.
    if (!ui.menu.hidden) return false;
    const points = samplePoints(r, innerWidth, innerHeight);
    return playerCovered(current, playerContainer(current), points, (x, y) => document.elementFromPoint(x, y), host);
  }

  // Keep the button at the player's top right while it is on screen. An
  // audio player is short: it gets the button above it (below, at the top
  // of the window), and needs only some width.
  function place() {
    frame = 0;
    if (!current || !current.isConnected) return hide();
    const r = current.getBoundingClientRect();
    const short = current instanceof HTMLAudioElement;
    const tooSmall = short ? r.width < 160 || r.height < 16 : r.width < 160 || r.height < 90;
    if (tooSmall || r.bottom < 0 || r.top > innerHeight || r.right < 0 || r.left > innerWidth || outOfSight(r)) {
      host.style.display = "none";
      return;
    }
    host.style.display = "block";
    // The button's width depends on the language its label is in.
    host.style.left = `${Math.max(0, Math.min(r.right, innerWidth) - ui.bar.offsetWidth - 20)}px`;
    if (short) {
      const height = ui.bar.offsetHeight || 28;
      host.style.top = `${r.top - height - 4 >= 0 ? r.top - height - 4 : Math.max(0, r.bottom + 4)}px`;
    } else {
      host.style.top = `${Math.max(0, r.top) + 8}px`;
    }
  }

  function schedule() {
    if (!frame && current) frame = requestAnimationFrame(place);
  }

  function hide() {
    current = null;
    clearInterval(recheck);
    recheck = 0;
    if (host) {
      host.style.display = "none";
      ui.menu.hidden = true;
    }
  }

  async function show(player) {
    if (enabled === null) {
      const answer = await ask({ type: "video:enabled" });
      enabled = !!(answer && answer.enabled);
      if (answer && answer.view) view = answer.view;
      if (answer && answer.protectedNote === false) protectedNote = false;
    }
    if (!enabled || closed.has(player)) return;
    if (!protectedNote && (encrypted.has(player) || player.mediaKeys)) {
      if (current === player) hide();
      return;
    }
    if (!host) build();
    if (current !== player) ui.menu.hidden = true;
    current = player;
    clearTimeout(hideTimer);
    if (!recheck) recheck = setInterval(schedule, 1000);
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
    // This player's streams first: the worker lists only them when it can
    // tell which they are, else everything the page loaded.
    const answer = await ask({ type: "video:menu", element: idOf(current), srcs: sources(current) });
    menu.replaceChildren();
    if (!answer || !answer.connected) {
      menu.append(el("div", "note", chrome.i18n.getMessage("udmNotConnected")));
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
      // Subtitle tracks to keep, ticked as the stream's defaults; they go
      // with whichever quality is clicked, but not with the sound alone.
      const subs = item.subtitles || [];
      const picked = new Set(subs.filter((s) => s.on).map((s) => s.id));
      if (subs.length) {
        const row = el("div", "subs", chrome.i18n.getMessage("subtitles"));
        for (const s of subs) {
          const label = el("label");
          const box = el("input");
          box.type = "checkbox";
          box.checked = picked.has(s.id);
          box.addEventListener("click", (ev) => ev.stopPropagation());
          box.addEventListener("change", () => (box.checked ? picked.add(s.id) : picked.delete(s.id)));
          label.append(box, el("span", "", s.label));
          row.append(label);
        }
        menu.append(row);
      }
      for (const o of item.options) {
        menu.append(
          option(o.label, o.size, async () => {
            // A playlist's videos are picked in UDM's window.
            if (o.playlist) {
              const listed = await ask({ type: "video:playlist", entry: item.entry });
              const said = listed && listed.ok ? chrome.i18n.getMessage("sentToUdm") : chrome.i18n.getMessage("couldNotReachUdm");
              menu.replaceChildren(el("div", "head", said));
              closeTimer = setTimeout(() => (menu.hidden = true), 1500);
              return;
            }
            // The worker names it after the tab's top page (pageNames).
            const sent = await ask({
              type: "video:download",
              entry: item.entry,
              video: o.video,
              audio: o.audio,
              audioOnly: o.audioOnly,
              subtitles: subs.length && !o.audioOnly ? [...picked] : undefined,
              ...pageNames(),
            });
            const result = sent && sent.ok ? chrome.i18n.getMessage("sentToUdm") : chrome.i18n.getMessage("couldNotReachUdm");
            menu.replaceChildren(el("div", "head", result));
            closeTimer = setTimeout(() => (menu.hidden = true), 1500);
          }),
        );
      }
    }
  }

  document.addEventListener("play", (ev) => isMediaElement(ev.target) && (started(ev.target), show(ev.target)), true);
  document.addEventListener("loadstart", (ev) => isMediaElement(ev.target) && started(ev.target), true);
  document.addEventListener(
    "encrypted",
    (ev) => {
      if (!isMediaElement(ev.target)) return;
      encrypted.add(ev.target);
      if (!protectedNote && ev.target === current) hide();
    },
    true,
  );
  document.addEventListener(
    "mouseover",
    (ev) => {
      const t = ev.target;
      if (isMediaElement(t) ? t.readyState > 0 : isPlayer(t)) show(t);
    },
    true,
  );
  document.addEventListener(
    "mouseout",
    (ev) => {
      if (ev.target !== current || ev.relatedTarget === host || current.paused === false) return;
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
  document.addEventListener("webkitfullscreenchange", schedule);
  document.addEventListener("visibilitychange", schedule);
  // A single-page site moved to another page (the worker saw it): the
  // players and their streams are the old page's.
  chrome.runtime.onMessage.addListener((msg) => {
    if (msg && msg.type === "video:reset") hide();
    return false;
  });
})();
