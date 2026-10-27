// UIDM service worker.
//
// Keeps one native-messaging port to the UIDM host (which relays to the
// daemon), takes downloads away from the browser and hands them to UIDM,
// adds a "Download with UIDM" context menu, and serves the popup.
//
// A download is only taken from the browser while UIDM is connected, capture
// is on, and it passes the capture rules from UIDM's settings (see rules.js);
// otherwise the browser downloads it as usual. If UIDM cannot even start a
// download it took, it is handed back to the browser.
//
// It also notes the videos each tab loads (see media.js) for the video
// button that content.js shows over playing videos, and collects a page's
// links for the window's link picker.

importScripts("rules.js", "post.js", "media.js");

const HOST = "com.uidm.host";
const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 30000;

let port = null;
let connected = false; // true once the daemon has answered hello
let lastError = "";
let reconnectDelay = RECONNECT_MIN_MS;
let reconnectTimer = null;

// Mirror of the daemon's download list, by id.
const downloads = new Map();
// Downloads taken from the browser: url -> how it was requested (null for a
// GET, else the rebuilt POST) until their ack arrives, then id -> { url, post }
// until they start, finish, or are handed back.
const awaitingAck = new Map();
const captured = new Map();

// Recent requests in frames that sent a body (form submissions), oldest
// first, and the Content-Type each sent, by request id. A download a form
// started is matched to its request here (see post.js).
const posts = [];
const postTypes = new Map();
if (chrome.webRequest) {
  const frames = { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] };
  chrome.webRequest.onBeforeRequest.addListener(
    (d) => {
      if (d.method !== "POST" || d.incognito) return;
      const now = Date.now();
      prunePosts(posts, now);
      posts.push({ id: d.requestId, url: d.url, time: now, requestBody: d.requestBody });
    },
    frames,
    ["requestBody"],
  );
  chrome.webRequest.onBeforeSendHeaders.addListener(
    (d) => {
      if (d.method !== "POST") return;
      const type = (d.requestHeaders || []).find((h) => h.name.toLowerCase() === "content-type");
      const record = posts.find((r) => r.id === d.requestId);
      if (record && type) record.contentType = type.value;
    },
    frames,
    ["requestHeaders"],
  );
}

// Video and audio each tab loaded: tab id -> Map(url -> entry). A tab's list
// starts over when it goes to another page.
const media = new Map();
if (chrome.webRequest) {
  chrome.webRequest.onHeadersReceived.addListener(
    (d) => {
      if (d.tabId < 0 || d.incognito) return;
      const found = classifyMedia(d);
      if (found) recordMedia(media, d.tabId, d.url, found, Date.now());
    },
    { urls: ["<all_urls>"], types: ["media", "xmlhttprequest", "object", "other"] },
    ["responseHeaders"],
  );
  chrome.webRequest.onBeforeRequest.addListener(
    (d) => {
      if (d.tabId >= 0) media.delete(d.tabId);
    },
    { urls: ["<all_urls>"], types: ["main_frame"] },
  );
}
if (chrome.tabs) chrome.tabs.onRemoved.addListener((tabId) => media.delete(tabId));

// What UIDM said each stream offers, by URL, and the probes waiting for an
// answer, by request id.
const mediaInfos = new Map();
const probes = new Map();
let nextProbe = 1;
const PROBE_TIMEOUT_MS = 20000;

// Ask UIDM what the stream at `url` offers, sending the page's cookies
// along. Resolves to a MediaInfo, or null without an answer.
async function probeStream(url, referrer) {
  if (mediaInfos.has(url)) return mediaInfos.get(url);
  const request = nextProbe++;
  const download = { url, user_agent: navigator.userAgent };
  const cookies = await cookieHeader(url);
  if (cookies) download.cookies = cookies;
  if (referrer) download.referrer = referrer;
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      probes.delete(request);
      resolve(null);
    }, PROBE_TIMEOUT_MS);
    probes.set(request, (info) => {
      clearTimeout(timer);
      // Failures are not kept: the next click asks again.
      if (!info.error || info.drm) mediaInfos.set(url, info);
      resolve(info);
    });
    if (!send({ op: "probe_media", request, download })) {
      clearTimeout(timer);
      probes.delete(request);
      resolve(null);
    }
  });
}

// The video button's menu for tab `tabId`.
async function videoMenu(tabId, referrer) {
  const entries = [...(media.get(tabId) || new Map()).values()];
  const streams = entries.filter((e) => e.kind !== "file");
  const infos = new Map();
  await Promise.all(
    streams.map(async (e) => {
      const info = await probeStream(e.url, referrer);
      if (info) infos.set(e.url, info);
    }),
  );
  return mediaMenu(entries, infos);
}

// Send media entry `entryId` of tab `tabId` to UIDM, in the chosen quality.
async function downloadMedia(tabId, msg, referrer) {
  const entry = [...(media.get(tabId) || new Map()).values()].find((e) => e.id === msg.entry);
  if (!entry || !connected) return false;
  const download = { op: "add", url: entry.url, user_agent: navigator.userAgent };
  const cookies = await cookieHeader(entry.url);
  if (cookies) download.cookies = cookies;
  if (referrer) download.referrer = referrer;
  const proxy = await browserProxy();
  if (proxy) download.proxy = proxy;
  if (entry.kind === "file") {
    const ext = extensionOf(urlPath(entry.url)) || (entry.mime.startsWith("audio/") ? "mp3" : "mp4");
    download.filename = titleFileName(msg.title, ext);
  } else {
    download.filename = titleFileName(msg.title, "mp4");
    download.media = { kind: entry.kind };
    if (typeof msg.video === "string") download.media.video = msg.video;
    if (typeof msg.audio === "string") download.media.audio = msg.audio;
  }
  if (!download.filename) delete download.filename;
  return send(download);
}

// A content script's request. Pages are untrusted, so a content script can
// only ask about its own tab and choose among the media the worker saw.
function onContentMessage(msg, sender, sendResponse) {
  const tabId = sender.tab.id;
  const pageUrl = sender.url || sender.tab.url || "";
  switch (msg && msg.type) {
    case "video:enabled":
      sendResponse({ enabled: videoButtonAllowed(rules, pageUrl) });
      return false;
    case "video:menu":
      if (!connected) {
        sendResponse({ connected: false, items: [] });
        return false;
      }
      videoMenu(tabId, pageUrl).then((items) => sendResponse({ connected: true, items }));
      return true;
    case "video:download":
      downloadMedia(tabId, msg, pageUrl).then((ok) => sendResponse({ ok }));
      return true;
    case "link:skip":
      // The skip key was held: the download this click starts stays.
      skipClick = { time: Date.now() };
      sendResponse({ ok: true });
      return false;
    case "link:force":
      // The force key was held: the link goes to UIDM whatever it is.
      if (!connected || !captureEnabled || !isWeb(msg.url)) {
        sendResponse({ ok: false });
        return false;
      }
      handOff(msg.url, pageUrl).then((ok) => sendResponse({ ok }));
      return true;
    default:
      return false;
  }
}

let captureEnabled = true;
// UIDM's settings, which carry the capture rules; null until they arrive.
let rules = null;
// "Skip next download" in the popup: the next download stays in the browser.
let skipNext = false;
// The last link click with the skip key held, as { time }.
let skipClick = null;
// A download in UIDM waiting for a new link: the next download from the
// browser gives it one instead of starting a new download.
let refreshWaiting = null;
const settingsReady = chrome.storage.local.get({ capture: true }).then((s) => {
  captureEnabled = s.capture;
});
chrome.storage.onChanged.addListener((changes) => {
  if (changes.capture) captureEnabled = changes.capture.newValue;
});

// --- Connection to the host -------------------------------------------------

function connect() {
  clearTimeout(reconnectTimer);
  reconnectTimer = null;
  try {
    port = chrome.runtime.connectNative(HOST);
  } catch (e) {
    onDisconnect(e.message);
    return;
  }
  port.onMessage.addListener(onHostMessage);
  port.onDisconnect.addListener(() => {
    onDisconnect(chrome.runtime.lastError?.message || "UIDM disconnected");
  });
  port.postMessage({ op: "hello", version: chrome.runtime.getManifest().version });
  port.postMessage({ op: "list" });
  port.postMessage({ op: "get_settings" });
}

function onDisconnect(reason) {
  port = null;
  connected = false;
  lastError = reason;
  updateBadge();
  if (!reconnectTimer) {
    reconnectTimer = setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, RECONNECT_MAX_MS);
  }
}

function send(msg) {
  if (!port) return false;
  try {
    port.postMessage(msg);
    return true;
  } catch (_) {
    return false;
  }
}

function upsert(id, url) {
  let e = downloads.get(id);
  if (!e) {
    e = { id, url, path: null, total: null, done: 0, status: "queued", error: null };
    downloads.set(id, e);
  }
  return e;
}

const TERMINAL = new Set(["done", "failed", "canceled", "interrupted", "waiting"]);

function onHostMessage(msg) {
  switch (msg.op) {
    case "hello":
      connected = true;
      lastError = "";
      reconnectDelay = RECONNECT_MIN_MS;
      break;
    case "ack":
      if (awaitingAck.has(msg.url)) {
        captured.set(msg.id, { url: msg.url, post: awaitingAck.get(msg.url) });
        awaitingAck.delete(msg.url);
      }
      break;
    case "downloads":
      downloads.clear();
      for (const e of msg.items) downloads.set(e.id, e);
      break;
    case "added":
      upsert(msg.id, msg.url);
      break;
    case "started": {
      const e = upsert(msg.id, msg.url);
      Object.assign(e, { status: "downloading", path: msg.path, total: msg.total, error: null });
      // It started, so it is UIDM's for good: no fallback from here on.
      captured.delete(msg.id);
      break;
    }
    case "progress": {
      const e = upsert(msg.id, msg.url);
      if (!TERMINAL.has(e.status)) e.status = "downloading";
      e.done = msg.done;
      if (msg.total != null) e.total = msg.total;
      break;
    }
    case "done": {
      const e = upsert(msg.id, msg.url);
      Object.assign(e, { status: "done", path: msg.path, error: null });
      if (e.total != null) e.done = e.total;
      captured.delete(msg.id);
      break;
    }
    case "failed": {
      const e = upsert(msg.id, msg.url);
      Object.assign(e, { status: "failed", error: msg.error });
      handBackIfNeverStarted(msg.id);
      break;
    }
    case "canceled":
      upsert(msg.id, msg.url).status = "canceled";
      captured.delete(msg.id);
      break;
    case "removed":
      downloads.delete(msg.id);
      break;
    case "name_taken": {
      // UIDM asks in its window whether to rename or overwrite; it keeps the
      // download either way, so no hand-back.
      const e = upsert(msg.id, msg.url);
      Object.assign(e, { status: "confirming", path: msg.path, total: msg.total, error: null });
      captured.delete(msg.id);
      break;
    }
    case "updated":
      downloads.set(msg.item.id, msg.item);
      // Kept for later: it is UIDM's now, so no hand-back.
      if (msg.item.status === "waiting") captured.delete(msg.item.id);
      break;
    case "settings":
      rules = msg.settings;
      // Content scripts read the keys from storage, so a click can be
      // handled at once without asking the worker.
      chrome.storage.local.set({
        captureKeys: { skip: rules.capture_skip_key || "none", force: rules.capture_force_key || "none" },
      });
      break;
    case "refresh_waiting":
      refreshWaiting = msg.id ?? null;
      break;
    case "media_info": {
      const done = probes.get(msg.request);
      probes.delete(msg.request);
      if (done) done(msg.info || {});
      break;
    }
    case "error":
      lastError = msg.message;
      break;
  }
  updateBadge();
}

// A download UIDM took but could not even start (the server refused it, say)
// goes back to the browser so the user does not lose it.
function handBackIfNeverStarted(id) {
  const taken = captured.get(id);
  if (!taken) return;
  captured.delete(id);
  send({ op: "remove", id });
  giveBack(taken.url, taken.post);
}

// Let the browser download it after all, the way it was first requested.
function giveBack(url, post) {
  if (!post) {
    chrome.downloads.download({ url }).catch(() => {});
    return;
  }
  // The browser can only resend a text body.
  if (post.text === undefined) return;
  const options = { url, method: "POST", body: post.text };
  if (post.contentType) options.headers = [{ name: "Content-Type", value: post.contentType }];
  chrome.downloads.download(options).catch(() => {});
}

function updateBadge() {
  let active = 0;
  for (const e of downloads.values()) if (!TERMINAL.has(e.status)) active++;
  chrome.action.setBadgeText({ text: active ? String(active) : "" });
  chrome.action.setBadgeBackgroundColor({ color: connected ? "#2563eb" : "#9ca3af" });
}

// --- Capture ----------------------------------------------------------------

const isWeb = (url) => /^https?:\/\//i.test(url || "");

async function cookieHeader(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url });
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch (_) {
    return "";
  }
}

function baseName(path) {
  const name = (path || "").split(/[\\/]/).pop();
  return name || undefined;
}

// The browser's fixed proxy as `scheme://host:port`, for UIDM's "use the
// browser's proxy" option. Only plain HTTP and SOCKS proxies are reported;
// automatic and system settings report nothing.
async function browserProxy() {
  try {
    const { value } = await chrome.proxy.settings.get({});
    if (!value || value.mode !== "fixed_servers" || !value.rules) return undefined;
    const p = value.rules.singleProxy || value.rules.proxyForHttps || value.rules.proxyForHttp;
    if (!p || !p.host || !["http", "socks4", "socks5"].includes(p.scheme || "http")) return undefined;
    const scheme = p.scheme || "http";
    const port = p.port || (scheme === "http" ? 80 : 1080);
    return `${scheme}://${p.host}:${port}`;
  } catch (_) {
    return undefined;
  }
}

async function handOff(url, referrer, filename, later = false, post = null, refreshId = null) {
  const cookies = await cookieHeader(url);
  const msg = { op: "add", url, user_agent: navigator.userAgent };
  const proxy = await browserProxy();
  if (proxy) msg.proxy = proxy;
  if (post) {
    msg.method = "POST";
    msg.body = post.body;
    if (post.contentType) msg.headers = { "Content-Type": post.contentType };
  }
  // "Download later": UIDM adds it to its main queue without starting it.
  if (later) msg.start = false;
  if (referrer) msg.referrer = referrer;
  if (filename) msg.filename = filename;
  if (cookies) msg.cookies = cookies;
  if (refreshId !== null) {
    // The same request, as the new link of a download already in UIDM.
    const { op, ...download } = msg;
    return send({ op: "refresh_link", id: refreshId, download });
  }
  return send(msg);
}

chrome.downloads.onCreated.addListener(async (item) => {
  await settingsReady;
  if (!captureEnabled || !connected) return; // let the browser handle it
  if (item.incognito) return; // keep private-window downloads out of UIDM's list
  if (item.byExtensionId === chrome.runtime.id) return; // our own hand-back
  const url = item.finalUrl || item.url;
  if (!isWeb(url)) return; // blob:, data:, file: only exist in the browser
  if (skipNext) {
    skipNext = false;
    return;
  }
  if (clickApplies(skipClick, Date.now())) {
    skipClick = null;
    return;
  }
  // A download waiting for a new link takes this one, whatever the capture
  // rules say: the user asked for it.
  const refreshId = refreshWaiting;
  if (refreshId === null) {
    const size = item.totalBytes > 0 ? item.totalBytes : item.fileSize > 0 ? item.fileSize : 0;
    const decision = captureDecision(rules, {
      url,
      referrer: item.referrer,
      filename: baseName(item.filename),
      mime: item.mime,
      size,
    });
    if (!decision.capture) return;
  }

  // A form's download is sent as the same POST; one whose request cannot be
  // rebuilt stays in the browser.
  let post = null;
  let requestUrl = url;
  const record = findPost(posts, item.url, Date.now()) || findPost(posts, url, Date.now());
  if (record) {
    post = rebuildPost(record);
    if (post.leave) return;
    requestUrl = record.url;
  }

  // Take it from the browser before it writes much, then hand it over.
  try {
    await chrome.downloads.cancel(item.id);
    await chrome.downloads.erase({ id: item.id });
  } catch (_) {
    // Already finished or gone; nothing to take.
    return;
  }
  if (refreshId !== null) {
    refreshWaiting = null;
    if (await handOff(requestUrl, item.referrer, baseName(item.filename), false, post, refreshId)) return;
    giveBack(requestUrl, post);
    return;
  }
  awaitingAck.set(requestUrl, post);
  const sent = await handOff(requestUrl, item.referrer, baseName(item.filename), false, post);
  if (!sent) {
    // The port dropped between the check and the send: give it back.
    awaitingAck.delete(requestUrl);
    giveBack(requestUrl, post);
  }
});

// --- All links on a page ----------------------------------------------------

// The most links one page sends to the picker.
const MAX_PAGE_LINKS = 5000;

// Runs in each frame of the page (so it cannot use anything outside itself):
// the frame's links and images as { url, text, image }. With `selection`,
// only the links in the selected text, and any bare addresses in it.
function collectLinks(selection) {
  const out = [];
  const sel = getSelection();
  if (selection && (!sel || sel.isCollapsed)) return out;
  const ranges = selection ? Array.from({ length: sel.rangeCount }, (_, i) => sel.getRangeAt(i)) : [];
  const chosen = (node) => !selection || ranges.some((r) => r.intersectsNode(node));
  const text = (s) => String(s || "").replace(/\s+/g, " ").trim().slice(0, 200);
  for (const a of document.querySelectorAll("a[href], area[href]")) {
    if (chosen(a)) out.push({ url: a.href, text: text(a.textContent || a.getAttribute("alt")) });
  }
  if (selection) {
    for (const m of String(sel).matchAll(/https?:\/\/[^\s<>"'`]+/g)) {
      out.push({ url: m[0].replace(/[).,;:!?\]]+$/, ""), text: "" });
    }
  } else {
    for (const img of document.images) {
      const src = img.currentSrc || img.src;
      if (src) out.push({ url: src, text: text(img.alt), image: true });
    }
  }
  return out;
}

// Collect the links of tab `tabId` (in every frame) and send them to the
// window's picker, with the cookies of each site they point to.
async function pickLinks(tabId, pageUrl, selection = false) {
  if (!connected || !chrome.scripting || tabId == null || tabId < 0) return false;
  let results;
  try {
    results = await chrome.scripting.executeScript({
      target: { tabId, allFrames: true },
      func: collectLinks,
      args: [selection],
    });
  } catch (_) {
    return false; // a page extensions may not script, such as the web store
  }
  const links = [];
  for (const r of results || []) {
    for (const l of r.result || []) {
      if (links.length < MAX_PAGE_LINKS && isWeb(l.url)) links.push(l);
    }
  }
  if (links.length === 0) return false;
  const cookies = new Map();
  for (const l of links) {
    let origin;
    try {
      origin = new URL(l.url).origin;
    } catch (_) {
      continue;
    }
    if (!cookies.has(origin)) cookies.set(origin, await cookieHeader(l.url));
    if (cookies.get(origin)) l.cookies = cookies.get(origin);
  }
  return send({ op: "pick_links", page: pageUrl, links, user_agent: navigator.userAgent });
}

// --- Context menu -----------------------------------------------------------

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: "uidm-link", title: "Download with UIDM", contexts: ["link"] });
    chrome.contextMenus.create({
      id: "uidm-media",
      title: "Download with UIDM",
      contexts: ["image", "video", "audio"],
    });
    chrome.contextMenus.create({ id: "uidm-later-link", title: "Download later with UIDM", contexts: ["link"] });
    chrome.contextMenus.create({
      id: "uidm-later-media",
      title: "Download later with UIDM",
      contexts: ["image", "video", "audio"],
    });
    chrome.contextMenus.create({ id: "uidm-all-links", title: "Download all links with UIDM", contexts: ["page", "frame"] });
  });
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId === "uidm-all-links") {
    await pickLinks(tab && tab.id, info.pageUrl || (tab && tab.url) || "");
    return;
  }
  const isLink = info.menuItemId === "uidm-link" || info.menuItemId === "uidm-later-link";
  const later = info.menuItemId === "uidm-later-link" || info.menuItemId === "uidm-later-media";
  const url = isLink ? info.linkUrl : info.srcUrl;
  if (!isWeb(url)) return;
  if (connected && (await handOff(url, info.pageUrl, undefined, later))) return;
  // UIDM is not available; the click should still download something.
  chrome.downloads.download({ url }).catch(() => {});
});

// --- Popup ------------------------------------------------------------------

function snapshot() {
  return {
    connected,
    lastError,
    capture: captureEnabled,
    skipNext,
    refreshWaiting,
    videoButton: !rules || rules.video_button !== false,
    videoExcluded: rules ? rules.video_button_excluded_sites || [] : [],
    items: [...downloads.values()].sort((a, b) => b.id - a.id),
  };
}

// Change the video button setting in UIDM: on or off everywhere, or for one
// site. UIDM keeps it, and the window's Options shows it.
function setVideoButton(change) {
  if (!rules) return;
  const next = { ...rules };
  if (typeof change.on === "boolean") next.video_button = change.on;
  if (typeof change.site === "string" && /^[a-z0-9.-]+$/i.test(change.site)) {
    const site = change.site.toLowerCase();
    const list = (rules.video_button_excluded_sites || []).filter((s) => !siteMatches(s, site));
    if (!change.siteOn) list.push(site);
    next.video_button_excluded_sites = list;
  }
  send({ op: "set_settings", settings: next });
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  // Only this extension's own pages and content scripts talk to the worker.
  if (sender.id !== chrome.runtime.id) return;
  // Content scripts run in web pages; the popup (even opened in a tab) is
  // one of the extension's own pages.
  const ownPage = String(sender.url || "").startsWith(`chrome-extension://${chrome.runtime.id}/`);
  if (sender.tab && !ownPage) return onContentMessage(msg, sender, sendResponse);
  switch (msg.type) {
    case "state":
      break;
    case "setCapture":
      captureEnabled = !!msg.value;
      chrome.storage.local.set({ capture: captureEnabled });
      break;
    case "cancelRefresh":
      refreshWaiting = null;
      send({ op: "cancel_refresh" });
      break;
    case "setSkipNext":
      skipNext = !!msg.value;
      break;
    case "setVideoButton":
      setVideoButton(msg);
      break;
    case "add":
      if (isWeb(msg.url)) send({ op: "add", url: msg.url });
      break;
    case "allLinks":
      // The picker opens in the window; the popup only starts it.
      chrome.tabs
        .query({ active: true, currentWindow: true })
        .then(([tab]) => tab && pickLinks(tab.id, tab.url || ""))
        .catch(() => {});
      break;
    case "start":
      if (Number.isInteger(msg.id)) {
        const confirm = { op: "confirm", id: msg.id, start: true };
        // Started from the popup, nothing asks about a taken name: under
        // "ask" it gets a new one instead of asking again.
        if (rules && rules.file_exists === "ask") confirm.file_exists = "rename";
        send(confirm);
      }
      break;
    case "cancel":
    case "retry":
    case "remove":
      if (Number.isInteger(msg.id)) send({ op: msg.type, id: msg.id });
      break;
    case "reconnect":
      reconnectDelay = RECONNECT_MIN_MS;
      if (!port) connect();
      break;
    default:
      return;
  }
  sendResponse(snapshot());
});

connect();
updateBadge();
