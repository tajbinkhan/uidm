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

// Chrome runs this as a service worker and it loads its helpers itself;
// Firefox runs it as a background script after the helpers the manifest
// lists, and has no importScripts.
if (typeof importScripts === "function") importScripts("rules.js", "post.js", "media.js");

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
// Downloads taken from the browser: url -> { post, filename } until their
// ack arrives (`post` is null for a GET, else the rebuilt POST), then
// id -> { url, post, filename } until they start, finish, or are handed back.
const awaitingAck = new Map();
const captured = new Map();

// Recent requests in frames that sent a body (form submissions), oldest
// first, and the Content-Type each sent, by request id. A download a form
// started is matched to its request here (see post.js).
const posts = [];
const postTypes = new Map();
// Private-window tabs. Firefox marks webRequest events from them, Chrome
// does not, so they are followed here: what happens in them stays out of
// UIDM and out of the "recent videos" menus of normal windows.
const privateTabs = new Set();
const isPrivate = (tabId) => privateTabs.has(tabId);
if (chrome.tabs) {
  const note = (tab) => tab && tab.incognito && privateTabs.add(tab.id);
  chrome.tabs.query({}).then((tabs) => tabs.forEach(note)).catch(() => {});
  chrome.tabs.onCreated.addListener(note);
  chrome.tabs.onUpdated.addListener((_id, _change, tab) => note(tab));
  chrome.tabs.onRemoved.addListener((id) => privateTabs.delete(id));
}

if (chrome.webRequest) {
  const frames = { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] };
  chrome.webRequest.onBeforeRequest.addListener(
    (d) => {
      if (d.method !== "POST" || d.incognito || isPrivate(d.tabId)) return;
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
      if (d.tabId < 0 || d.incognito || isPrivate(d.tabId)) return;
      const found = classifyMedia(d, rules);
      if (found) {
        recordMedia(media, d.tabId, d.url, found, Date.now());
        scheduleRecentMenu();
      }
    },
    { urls: ["<all_urls>"], types: ["media", "xmlhttprequest", "object", "other"] },
    ["responseHeaders"],
  );
  chrome.webRequest.onBeforeRequest.addListener(
    (d) => {
      if (d.tabId >= 0 && media.delete(d.tabId)) scheduleRecentMenu();
    },
    { urls: ["<all_urls>"], types: ["main_frame"] },
  );
}
if (chrome.tabs) chrome.tabs.onRemoved.addListener((tabId) => media.delete(tabId) && scheduleRecentMenu());
// A YouTube video, which moves between videos without loading a page.
if (chrome.tabs) {
  chrome.tabs.onUpdated.addListener((tabId, change, tab) => {
    const page = change.url && !tab.incognito && videoPageUrl(change.url);
    if (page) {
      recordPage(media, tabId, page, Date.now());
      scheduleRecentMenu();
    }
  });
}

// What UIDM said each stream offers, by URL, and the probes waiting for an
// answer, by request id.
const mediaInfos = new Map();
const probes = new Map();
let nextProbe = 1;
const PROBE_TIMEOUT_MS = 20000;
// yt-dlp takes longer: it reads the page and the player's code first.
const PAGE_PROBE_TIMEOUT_MS = 60000;

// Ask UIDM what the stream at `url` offers, sending the page's cookies
// along. Resolves to a MediaInfo, or null without an answer.
async function probeStream(url, referrer, timeout = PROBE_TIMEOUT_MS) {
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
    }, timeout);
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

// The video button's menu for tab `tabId`. Pages in a private window are
// not recorded, so they stay out of the "last videos" menus.
async function videoMenu(tabId, referrer, incognito) {
  const page = !incognito && videoPageUrl(referrer);
  if (page) recordPage(media, tabId, page, Date.now());
  const entries = [...(media.get(tabId) || new Map()).values()];
  const streams = entries.filter((e) => e.kind !== "file");
  const infos = new Map();
  await Promise.all(
    streams.map(async (e) => {
      const info = await probeStream(e.url, referrer, e.kind === "page" ? PAGE_PROBE_TIMEOUT_MS : PROBE_TIMEOUT_MS);
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
    download.filename = titleFileName(msg.title, mediaFileExtension(entry));
  } else {
    const soundOnly = ["m4a", "mp3"].includes(msg.audioOnly) ? msg.audioOnly : null;
    const title = entry.kind === "page" ? pageTitle(msg.title) : msg.title;
    download.filename = titleFileName(title, soundOnly || "mp4");
    download.media = { kind: entry.kind };
    if (typeof msg.video === "string") download.media.video = msg.video;
    if (typeof msg.audio === "string") download.media.audio = msg.audio;
    if (soundOnly) download.media.audio_only = soundOnly;
  }
  if (!download.filename) delete download.filename;
  return send(download);
}

// A content script's request. Pages are untrusted, so a content script can
// only ask about its own tab and choose among the media the worker saw.
function onContentMessage(msg, sender, sendResponse) {
  // Nothing from a private window goes to UIDM: its buttons stay off and
  // a forced link opens in the browser.
  if (sender.tab.incognito) {
    sendResponse({ enabled: false, connected: false, ok: false, items: [] });
    return false;
  }
  const tabId = sender.tab.id;
  const pageUrl = sender.url || sender.tab.url || "";
  switch (msg && msg.type) {
    case "video:enabled":
      sendResponse({
        enabled: videoButtonAllowed(rules, pageUrl),
        view: (rules && rules.video_view) || "full",
        protectedNote: !rules || rules.video_protected_note !== false,
      });
      return false;
    case "video:menu":
      if (!connected) {
        sendResponse({ connected: false, items: [] });
        return false;
      }
      videoMenu(tabId, pageUrl, sender.tab.incognito).then((items) => sendResponse({ connected: true, items }));
      return true;
    case "video:download":
      downloadMedia(tabId, msg, pageUrl).then((ok) => sendResponse({ ok }));
      return true;
    case "selection:enabled":
      sendResponse({ enabled: connected && selectionButtonAllowed(rules, pageUrl), view: (rules && rules.video_view) || "full" });
      return false;
    case "selection:links":
      // Only the selection's links, from every frame of this tab.
      pickLinks(tabId, sender.tab.url || pageUrl, true).then((ok) => sendResponse({ ok }));
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
const settingsReady = chrome.storage.local.get({ capture: true, takeoverNotice: true }).then((s) => {
  captureEnabled = s.capture;
  takeoverNotice = s.takeoverNotice;
});
chrome.storage.onChanged.addListener((changes) => {
  if (changes.capture) captureEnabled = changes.capture.newValue;
  if (changes.takeoverNotice) takeoverNotice = changes.takeoverNotice.newValue;
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
    onDisconnect(chrome.runtime.lastError?.message || chrome.i18n.getMessage("uidmDisconnected"));
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
        captured.set(msg.id, { url: msg.url, ...awaitingAck.get(msg.url) });
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
      handBackIfNeverStarted(msg.id, msg.error);
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
        captureKeys: {
          capture_skip_key: rules.capture_skip_key || "none",
          capture_force_key: rules.capture_force_key || "none",
          capture_keys_left_click: !!rules.capture_keys_left_click,
          capture_force_skips_pages: rules.capture_force_skips_pages !== false,
        },
      });
      if (JSON.stringify(rules.browser_menu_hidden || []) !== menuBuiltFor) buildMenus();
      break;
    case "refresh_waiting":
      refreshWaiting = msg.id ?? null;
      break;
    case "render_page":
      renderPage(msg.url).then((page) =>
        send({
          op: "rendered",
          request: msg.request,
          html: page ? page.html : null,
          url: page ? page.url : null,
          popups: page ? page.popups : [],
        }),
      );
      break;
    case "cookies_wanted":
      // A site crawl in UIDM signs in with the browser's cookies.
      if (isWeb(msg.url)) {
        cookieHeader(msg.url).then((cookies) => send({ op: "cookies", request: msg.request, cookies: cookies || null }));
      }
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
    case "rejected": {
      // UIDM refused a download the browser already let go of: give it back.
      lastError = msg.message;
      const taken = awaitingAck.get(msg.url);
      if (!taken) break;
      awaitingAck.delete(msg.url);
      giveBack(msg.url, taken.post);
      noticeTakeoverFailed(msg.url, taken.filename, msg.message);
      break;
    }
  }
  updateBadge();
}

// A download UIDM took but could not even start (the server refused it, say)
// goes back to the browser so the user does not lose it. `error` is why.
function handBackIfNeverStarted(id, error) {
  const taken = captured.get(id);
  if (!taken) return;
  captured.delete(id);
  send({ op: "remove", id });
  giveBack(taken.url, taken.post);
  noticeTakeoverFailed(taken.url, taken.filename, error || "?");
}

// --- The notice that UIDM could not take a download ---------------------------

const TAKEOVER_NOTICE = "uidm-takeover";
// Whether the notice shows; its "Don't show again" turns it off for good.
let takeoverNotice = true;
// Whether the last notice had that button; Firefox's notices have none, so
// there a click on the notice itself turns them off.
let noticeHasButton = false;

// Tell the user UIDM could not take the download of `url` (named
// `filename`), which the browser keeps or gets back. `reason` is UIDM's
// error, or null when UIDM is not connected. One notice replaces the last.
async function noticeTakeoverFailed(url, filename, reason) {
  if (!takeoverNotice || !chrome.notifications) return;
  const file = filename || url;
  const message = reason ? chrome.i18n.getMessage("takeoverError", [file, reason]) : chrome.i18n.getMessage("takeoverNotConnected", file);
  const options = {
    type: "basic",
    iconUrl: chrome.runtime.getURL("icons/48.png"),
    title: chrome.i18n.getMessage("takeoverFailed"),
    message,
  };
  try {
    await chrome.notifications.create(TAKEOVER_NOTICE, { ...options, buttons: [{ title: chrome.i18n.getMessage("dontShowAgain") }] });
    noticeHasButton = true;
  } catch (_) {
    noticeHasButton = false;
    const hint = chrome.i18n.getMessage("takeoverClickToStop");
    await chrome.notifications.create(TAKEOVER_NOTICE, { ...options, message: `${message} ${hint}` }).catch(() => {});
  }
}

function stopTakeoverNotices() {
  takeoverNotice = false;
  chrome.storage.local.set({ takeoverNotice: false });
  chrome.notifications.clear(TAKEOVER_NOTICE).catch(() => {});
}

if (chrome.notifications) {
  chrome.notifications.onButtonClicked.addListener((id) => id === TAKEOVER_NOTICE && stopTakeoverNotices());
  chrome.notifications.onClicked.addListener((id) => {
    if (id !== TAKEOVER_NOTICE) return;
    if (noticeHasButton) chrome.notifications.clear(id).catch(() => {});
    else stopTakeoverNotices();
  });
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
    if (value && "proxyType" in value) return firefoxProxy(value);
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

// Firefox reports its proxy as `{ proxyType, http, ssl, socks, ... }`, each
// proxy a "host:port" string. The HTTPS one is preferred, as Chrome's is.
function firefoxProxy(value) {
  if (value.proxyType !== "manual") return undefined;
  const http = value.httpProxyAll ? value.http : value.ssl || value.http;
  if (http) return proxyAddress("http", http, 80);
  if (value.socks) return proxyAddress(value.socksVersion === 4 ? "socks4" : "socks5", value.socks, 1080);
  return undefined;
}

function proxyAddress(scheme, address, defaultPort) {
  const hostPort = String(address).trim().replace(/^[a-z0-9+.-]+:\/\//i, "").replace(/\/.*$/, "");
  const m = /^(\[[^\]]+\]|[^:\s]+)(?::(\d+))?$/.exec(hostPort);
  if (!m) return undefined;
  return `${scheme}://${m[1]}:${Number(m[2]) || defaultPort}`;
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
  if (!captureEnabled) return; // let the browser handle it
  if (item.incognito) return; // keep private-window downloads out of UIDM's list
  if (item.byExtensionId === chrome.runtime.id) return; // our own hand-back
  const url = item.finalUrl || item.url;
  if (!isWeb(url)) return; // blob:, data:, file: only exist in the browser
  const size = item.totalBytes > 0 ? item.totalBytes : item.fileSize > 0 ? item.fileSize : 0;
  const described = { url, referrer: item.referrer, filename: baseName(item.filename), mime: item.mime, size };
  if (!connected) {
    // It stays in the browser; say so if UIDM would have taken it.
    if (!skipNext && !clickApplies(skipClick, Date.now()) && captureDecision(rules, described).capture) {
      noticeTakeoverFailed(url, described.filename, null);
    }
    return;
  }
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
  if (refreshId === null && !captureDecision(rules, described).capture) return;

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
  awaitingAck.set(requestUrl, { post, filename: described.filename });
  const sent = await handOff(requestUrl, item.referrer, described.filename, false, post);
  if (!sent) {
    // The port dropped between the check and the send: give it back.
    awaitingAck.delete(requestUrl);
    giveBack(requestUrl, post);
    noticeTakeoverFailed(requestUrl, described.filename, null);
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

// --- Pages for a site crawl --------------------------------------------------

// A page UIDM's grabber reads after its scripts run: opened in a background
// tab, given time to load and settle, then read and closed.
const RENDER_TIMEOUT_MS = 30000;
const RENDER_SETTLE_MS = 1500;

function waitForLoad(tabId, ms) {
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      chrome.tabs.onUpdated.removeListener(listener);
      resolve();
    };
    const listener = (id, info) => {
      if (id === tabId && info.status === "complete") done();
    };
    const timer = setTimeout(done, ms);
    chrome.tabs.onUpdated.addListener(listener);
  });
}

// The page at `url` as the browser shows it: { html, url, popups }, or
// null. Windows its scripts open are closed at once, and their addresses
// listed in `popups` for UIDM to follow or ignore.
async function renderPage(url) {
  if (!isWeb(url) || !chrome.scripting || !chrome.tabs || !chrome.tabs.create) return null;
  let tab = null;
  const popups = [];
  const opened = (t) => {
    if (!tab || t.openerTabId !== tab.id) return;
    const at = t.pendingUrl || t.url;
    if (isWeb(at)) popups.push(at);
    chrome.tabs.remove(t.id).catch(() => {});
  };
  chrome.tabs.onCreated.addListener(opened);
  try {
    tab = await chrome.tabs.create({ url, active: false });
    await waitForLoad(tab.id, RENDER_TIMEOUT_MS);
    await new Promise((r) => setTimeout(r, RENDER_SETTLE_MS));
    const [res] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: () => ({ html: document.documentElement.outerHTML, url: location.href }),
    });
    return res && res.result ? { ...res.result, popups } : null;
  } catch (_) {
    return null;
  } finally {
    chrome.tabs.onCreated.removeListener(opened);
    if (tab) chrome.tabs.remove(tab.id).catch(() => {});
  }
}

// --- Context menu -----------------------------------------------------------

// The menu entries of each item UIDM's Options can leave out (its
// `browser_menu_hidden` names them), with their titles and contexts.
const PAGE_CONTEXTS = ["page", "frame", "video", "audio"];
const MENU_ITEMS = {
  download: [
    { id: "uidm-link", title: chrome.i18n.getMessage("downloadWithUidm"), contexts: ["link"] },
    { id: "uidm-media", title: chrome.i18n.getMessage("downloadWithUidm"), contexts: ["image", "video", "audio"] },
  ],
  download_later: [
    { id: "uidm-later-link", title: chrome.i18n.getMessage("downloadLaterWithUidm"), contexts: ["link"] },
    { id: "uidm-later-media", title: chrome.i18n.getMessage("downloadLaterWithUidm"), contexts: ["image", "video", "audio"] },
  ],
  all_links: [{ id: "uidm-all-links", title: chrome.i18n.getMessage("downloadAllLinksWithUidm"), contexts: ["page", "frame"] }],
  selected_links: [{ id: "uidm-selection-links", title: chrome.i18n.getMessage("downloadSelectedLinksWithUidm"), contexts: ["selection"] }],
  last_video: [{ id: "uidm-last-video", title: chrome.i18n.getMessage("downloadLastVideo"), contexts: PAGE_CONTEXTS }],
  recent_videos: [{ id: "uidm-recent", title: chrome.i18n.getMessage("recentVideos"), contexts: PAGE_CONTEXTS }],
};
// How many videos the recent videos submenu lists.
const RECENT_VIDEOS = 10;
const RECENT_PREFIX = "uidm-recent-";
// The hidden list the menu was last built for, as JSON.
let menuBuiltFor = null;
const ignoreMenuError = () => void chrome.runtime.lastError;

function menuHidden() {
  return (rules && rules.browser_menu_hidden) || [];
}

// Build the context menu with the items UIDM's settings leave in.
function buildMenus() {
  const hidden = menuHidden();
  menuBuiltFor = JSON.stringify(hidden);
  chrome.contextMenus.removeAll(() => {
    recentMenuIds = [];
    for (const [name, entries] of Object.entries(MENU_ITEMS)) {
      if (hidden.includes(name)) continue;
      for (const e of entries) {
        chrome.contextMenus.create({ id: e.id, title: e.title, contexts: e.contexts }, ignoreMenuError);
      }
    }
    scheduleRecentMenu();
  });
}

// The recent videos submenu's entries, and the update that rebuilds them.
let recentMenuIds = [];
let recentMenuTimer = 0;
let recentMenuUpdate = Promise.resolve();

function scheduleRecentMenu() {
  clearTimeout(recentMenuTimer);
  recentMenuTimer = setTimeout(() => {
    recentMenuUpdate = recentMenuUpdate.then(updateRecentMenu, updateRecentMenu);
  }, 300);
}

async function tabInfo(tabId) {
  try {
    const tab = await chrome.tabs.get(tabId);
    return { title: tab.title || "", url: tab.url || "" };
  } catch (_) {
    return { title: "", url: "" };
  }
}

// List the newest videos (the video button's, across tabs) under the
// recent videos item, and enable the items only when there is one.
async function updateRecentMenu() {
  const hidden = menuHidden();
  const recent = recentMedia(media, RECENT_VIDEOS);
  if (!hidden.includes("last_video")) {
    chrome.contextMenus.update("uidm-last-video", { enabled: recent.length > 0 }, ignoreMenuError);
  }
  if (hidden.includes("recent_videos")) return;
  for (const id of recentMenuIds) chrome.contextMenus.remove(id, ignoreMenuError);
  recentMenuIds = [];
  for (const { tabId, entry } of recent) {
    const { title } = await tabInfo(tabId);
    const name = (title || hostOf(entry.url)).slice(0, 60);
    const id = RECENT_PREFIX + entry.id;
    chrome.contextMenus.create(
      { id, parentId: "uidm-recent", title: `${name} (${mediaLabel(entry)})`, contexts: PAGE_CONTEXTS },
      ignoreMenuError,
    );
    recentMenuIds.push(id);
  }
  chrome.contextMenus.update("uidm-recent", { enabled: recent.length > 0 }, ignoreMenuError);
}

// Send a recent video to UIDM in its best quality, named after its page.
async function downloadRecent({ tabId, entry }) {
  const { title, url } = await tabInfo(tabId);
  return downloadMedia(tabId, { entry: entry.id, title }, url);
}

chrome.runtime.onInstalled.addListener(buildMenus);

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  // A private window's links and media stay out of UIDM, like its downloads.
  if (tab && tab.incognito) return;
  if (info.menuItemId === "uidm-last-video") {
    const [last] = recentMedia(media, 1);
    if (last) await downloadRecent(last);
    return;
  }
  if (String(info.menuItemId).startsWith(RECENT_PREFIX)) {
    const id = Number(String(info.menuItemId).slice(RECENT_PREFIX.length));
    const chosen = recentMedia(media, Infinity).find((r) => r.entry.id === id);
    if (chosen) await downloadRecent(chosen);
    return;
  }
  if (info.menuItemId === "uidm-all-links" || info.menuItemId === "uidm-selection-links") {
    const selection = info.menuItemId === "uidm-selection-links";
    await pickLinks(tab && tab.id, info.pageUrl || (tab && tab.url) || "", selection);
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
  // chrome-extension://<id>/ in Chrome and Edge, moz-extension://<uuid>/ in
  // Firefox.
  const ownPage = String(sender.url || "").startsWith(chrome.runtime.getURL(""));
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
