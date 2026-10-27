// A stand-in for the chrome API that runs the real background.js, for the
// extension's tests. `loadWorker()` returns the fake native port, what was
// posted to it, the browser calls made, and the fake chrome object.

const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");


function event() {
  const listeners = [];
  return {
    addListener: (f) => listeners.push(f),
    removeListener: (f) => {
      const i = listeners.indexOf(f);
      if (i >= 0) listeners.splice(i, 1);
    },
    fire: (...args) => Promise.all([...listeners].map((f) => f(...args))),
  };
}

// What chrome.proxy.settings.get reports; tests may set it before loading.
let proxyValue = { mode: "system" };
function setBrowserProxy(value) {
  proxyValue = value;
}

// With `firefox`, load it the way Firefox does: the manifest's helper
// scripts first, no importScripts, and moz-extension:// pages.
function loadWorker({ firefox = false } = {}) {
  const sent = [];
  const browserCalls = [];
  const port = {
    onMessage: event(),
    onDisconnect: event(),
    postMessage: (m) => sent.push(m),
  };
  const chrome = {
    runtime: {
      id: "uidm",
      connectNative: () => port,
      getManifest: () => ({ version: "test" }),
      getURL: (path) => (firefox ? `moz-extension://0b1c/${path}` : `chrome-extension://uidm/${path}`),
      onMessage: event(),
      onInstalled: event(),
      lastError: null,
    },
    storage: {
      local: { get: async (d) => d, set: async () => {} },
      onChanged: event(),
    },
    proxy: { settings: { get: async () => ({ value: proxyValue }) } },
    webRequest: {
      onBeforeRequest: event(),
      onBeforeSendHeaders: event(),
      onHeadersReceived: event(),
    },
    tabs: { onRemoved: event() },
    downloads: {
      onCreated: event(),
      cancel: async (id) => browserCalls.push(["cancel", id]),
      erase: async () => {},
      download: async (o) => browserCalls.push(["download", o.url, o]),
    },
    cookies: { getAll: async () => [] },
    contextMenus: { removeAll: () => {}, create: () => {}, onClicked: event() },
    action: { setBadgeText: () => {}, setBadgeBackgroundColor: () => {} },
  };
  const dir = path.join(__dirname, "..");
  const context = vm.createContext({
    chrome,
    navigator: { userAgent: "test-agent" },
    console,
    setTimeout,
    clearTimeout,
    URL,
    TextEncoder,
    btoa,
  });
  const run = (file) => vm.runInContext(fs.readFileSync(path.join(dir, file), "utf8"), context);
  if (firefox) {
    const manifest = JSON.parse(fs.readFileSync(path.join(dir, "manifest.json"), "utf8"));
    manifest.background.scripts.forEach(run);
  } else {
    context.importScripts = (...files) => files.forEach(run);
    run("background.js");
  }
  const popupUrl = chrome.runtime.getURL("popup.html");
  const popup = (msg) =>
    new Promise((resolve) => chrome.runtime.onMessage.fire(msg, { id: "uidm", url: popupUrl }, resolve));
  // A message from the content script in tab `tabId`, on page `url`.
  const content = (msg, tabId, url = "https://site.net/watch") =>
    new Promise((resolve) => chrome.runtime.onMessage.fire(msg, { id: "uidm", tab: { id: tabId, url }, url }, resolve));
  return { port, sent, browserCalls, chrome, popup, content };
}

let nextDownloadId = 1;
function download(fields) {
  return { id: nextDownloadId++, url: "", referrer: "", filename: "", mime: "", totalBytes: 0, fileSize: 0, incognito: false, ...fields };
}

module.exports = { loadWorker, download, event, setBrowserProxy };
