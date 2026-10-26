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

importScripts("rules.js");

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
// Downloads taken from the browser: url -> true until their ack arrives, then
// id -> url until they start, finish, or are handed back.
const awaitingAck = new Map();
const captured = new Map();

let captureEnabled = true;
// UIDM's settings, which carry the capture rules; null until they arrive.
let rules = null;
// "Skip next download" in the popup: the next download stays in the browser.
let skipNext = false;
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
      if (awaitingAck.delete(msg.url)) captured.set(msg.id, msg.url);
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
    case "updated":
      downloads.set(msg.item.id, msg.item);
      // Kept for later: it is UIDM's now, so no hand-back.
      if (msg.item.status === "waiting") captured.delete(msg.item.id);
      break;
    case "settings":
      rules = msg.settings;
      break;
    case "error":
      lastError = msg.message;
      break;
  }
  updateBadge();
}

// A download UIDM took but could not even start (the server refused it, say)
// goes back to the browser so the user does not lose it.
function handBackIfNeverStarted(id) {
  const url = captured.get(id);
  if (!url) return;
  captured.delete(id);
  send({ op: "remove", id });
  chrome.downloads.download({ url }).catch(() => {});
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

async function handOff(url, referrer, filename) {
  const cookies = await cookieHeader(url);
  const msg = { op: "add", url, user_agent: navigator.userAgent };
  if (referrer) msg.referrer = referrer;
  if (filename) msg.filename = filename;
  if (cookies) msg.cookies = cookies;
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
  const size = item.totalBytes > 0 ? item.totalBytes : item.fileSize > 0 ? item.fileSize : 0;
  const decision = captureDecision(rules, {
    url,
    referrer: item.referrer,
    filename: baseName(item.filename),
    mime: item.mime,
    size,
  });
  if (!decision.capture) return;

  // Take it from the browser before it writes much, then hand it over.
  try {
    await chrome.downloads.cancel(item.id);
    await chrome.downloads.erase({ id: item.id });
  } catch (_) {
    // Already finished or gone; nothing to take.
    return;
  }
  awaitingAck.set(url, true);
  const sent = await handOff(url, item.referrer, baseName(item.filename));
  if (!sent) {
    // The port dropped between the check and the send: give it back.
    awaitingAck.delete(url);
    chrome.downloads.download({ url }).catch(() => {});
  }
});

// --- Context menu -----------------------------------------------------------

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: "uidm-link", title: "Download with UIDM", contexts: ["link"] });
    chrome.contextMenus.create({
      id: "uidm-media",
      title: "Download with UIDM",
      contexts: ["image", "video", "audio"],
    });
  });
});

chrome.contextMenus.onClicked.addListener(async (info) => {
  const url = info.menuItemId === "uidm-link" ? info.linkUrl : info.srcUrl;
  if (!isWeb(url)) return;
  if (connected && (await handOff(url, info.pageUrl))) return;
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
    items: [...downloads.values()].sort((a, b) => b.id - a.id),
  };
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  // Only this extension's own pages talk to the worker.
  if (sender.id !== chrome.runtime.id) return;
  switch (msg.type) {
    case "state":
      break;
    case "setCapture":
      captureEnabled = !!msg.value;
      chrome.storage.local.set({ capture: captureEnabled });
      break;
    case "setSkipNext":
      skipNext = !!msg.value;
      break;
    case "add":
      if (isWeb(msg.url)) send({ op: "add", url: msg.url });
      break;
    case "start":
      if (Number.isInteger(msg.id)) send({ op: "confirm", id: msg.id, start: true });
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
