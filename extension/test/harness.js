// A stand-in for the chrome API that runs the real background.js, for the
// extension's tests. `loadWorker()` returns the fake native port, what was
// posted to it, the browser calls made, and the fake chrome object.

const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const english = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "chromium", "_locales", "en", "messages.json"), "utf8"));

// chrome.i18n.getMessage as the browser does it, from the English messages:
// each $NAME$ in a message is its placeholder's content, in which $1..$9 are
// the substitutions (a string or an array of them), and $$ is a dollar sign.
// An unknown name gives "".
function getMessage(name, substitutions) {
  const m = english[name];
  if (!m) return "";
  const subs = substitutions == null ? [] : [].concat(substitutions).map(String);
  const placeholders = {};
  for (const [key, p] of Object.entries(m.placeholders || {})) placeholders[key.toLowerCase()] = p.content;
  const fill = (content) => content.replace(/\$(\d)/g, (_, i) => subs[i - 1] ?? "");
  return m.message.replace(/\$([A-Za-z0-9_@]+)\$|\$\$/g, (_, key) =>
    key === undefined ? "$" : fill(placeholders[key.toLowerCase()] ?? ""),
  );
}

function event() {
  const listeners = [];
  return {
    // The extra options each listener asked for, as webRequest takes them.
    options: [],
    addListener(f, _filter, extra) {
      listeners.push(f);
      this.options.push(extra || []);
    },
    removeListener: (f) => {
      const i = listeners.indexOf(f);
      if (i >= 0) listeners.splice(i, 1);
    },
    fire: (...args) => Promise.all([...listeners].map((f) => f(...args))),
    // What each listener returned, for a blocking webRequest event.
    call: (...args) => [...listeners].map((f) => f(...args)),
  };
}

// What chrome.proxy.settings.get reports; tests may set it before loading.
let proxyValue = { mode: "system" };
function setBrowserProxy(value) {
  proxyValue = value;
}

// With `firefox`, load it the way Firefox does: the manifest's helper
// scripts first, no importScripts, and moz-extension:// pages. `session` is
// what chrome.storage.session holds when the worker starts, or false for a
// browser without it (older Firefox). `globals` adds to the worker's globals
// (OffscreenCanvas and the like, which Node does not have).
//
// With `safari`, load the Safari copy as Safari does: no downloads API, no
// proxy settings, no notifications, safari-web-extension:// pages, and
// native messages through runtime.sendNativeMessage (see native.js). Each
// request it sends is in `native.requests`; `native.reply(request)` answers
// it (by default a poll with the frames tests queue in `native.frames`, at
// once), and `native.settle()` waits for the worker's requests to go out and
// come back.
// Its timers do not keep Node running.
function loadWorker({ firefox = false, safari = false, session = {}, globals = {} } = {}) {
  const sent = [];
  const browserCalls = [];
  const port = {
    onMessage: event(),
    onDisconnect: event(),
    postMessage: (m) => sent.push(m),
  };
  const native = {
    requests: [],
    frames: [],
    session: "host-1",
    reply: (request) => ({ session: native.session, frames: request.poll ? native.frames.splice(0) : [] }),
    settle: () => new Promise((resolve) => setTimeout(resolve, 20)),
  };
  const manifest = () => (safari ? safariManifest() : firefox ? firefoxManifest() : chromeManifest());
  const chrome = {
    runtime: {
      id: "udm",
      connectNative: () => port,
      ...(safari
        ? {
            sendNativeMessage: async (_host, request) => {
              native.requests.push(request);
              // What the worker sent goes in `sent`, as with a port.
              for (const m of request.frames || []) sent.push(m);
              return native.reply(request);
            },
          }
        : {}),
      getManifest: () => ({ ...manifest(), version: "test" }),
      getURL: (path) =>
        safari ? `safari-web-extension://0d1e/${path}` : firefox ? `moz-extension://0b1c/${path}` : `chrome-extension://udm/${path}`,
      onMessage: event(),
      onInstalled: event(),
      lastError: null,
    },
    storage: {
      // What the worker stored, in order.
      written: [],
      local: { get: async (d) => d, set: async (o) => chrome.storage.written.push(o) },
      ...(session === false
        ? {}
        : {
            session: {
              data: { ...session },
              get: async (d) => ({ ...d, ...chrome.storage.session.data }),
              set: async (o) => Object.assign(chrome.storage.session.data, o),
            },
          }),
      onChanged: event(),
    },
    ...(safari ? {} : { proxy: { settings: { get: async () => ({ value: proxyValue }) } } }),
    webRequest: {
      onBeforeRequest: event(),
      onBeforeSendHeaders: event(),
      onSendHeaders: event(),
      onBeforeRedirect: event(),
      onHeadersReceived: event(),
      // Chrome's "extraHeaders" option; Firefox has none.
      ...(firefox
        ? {}
        : {
            OnSendHeadersOptions: { REQUEST_HEADERS: "requestHeaders", EXTRA_HEADERS: "extraHeaders" },
            OnHeadersReceivedOptions: { RESPONSE_HEADERS: "responseHeaders", EXTRA_HEADERS: "extraHeaders" },
          }),
    },
    // The frames of each tab, as tests set them: tab id -> [{ frameId,
    // parentFrameId, url }].
    webNavigation: {
      frames: new Map(),
      onHistoryStateUpdated: event(),
      getAllFrames: async ({ tabId }) => chrome.webNavigation.frames.get(tabId) || null,
    },
    tabs: {
      onActivated: event(),
      onRemoved: event(),
      onUpdated: event(),
      onCreated: event(),
      query: async () => [],
      // The tabs get() finds, by id, the ids remove() closed, and what
      // create() opened.
      known: new Map(),
      removed: [],
      created: [],
      // Only Safari's: the others' tests never open a tab.
      ...(safari
        ? {
            create: async (o) => {
              chrome.tabs.created.push(o);
              return { id: 900 + chrome.tabs.created.length, ...o };
            },
          }
        : {}),
      get: async (id) => {
        if (!chrome.tabs.known.has(id)) throw new Error("no such tab");
        return chrome.tabs.known.get(id);
      },
      remove: async (id) => {
        chrome.tabs.removed.push(id);
      },
      // Messages to content scripts as [tabId, msg, options]; `reply`, when
      // a test sets it, answers them as the frame's content script would.
      messages: [],
      reply: null,
      sendMessage: async (tabId, msg, options) => {
        chrome.tabs.messages.push([tabId, msg, options]);
        return chrome.tabs.reply ? chrome.tabs.reply(msg, options) : undefined;
      },
    },
    downloads: safari ? undefined : {
      onCreated: event(),
      // The browser's download list, which search() looks in.
      items: [],
      search: async (q) =>
        chrome.downloads.items.filter((d) => (q.id === undefined || d.id === q.id) && (q.state === undefined || d.state === q.state)),
      cancel: async (id) => browserCalls.push(["cancel", id]),
      erase: async (q) => browserCalls.push(["erase", q.id]),
      download: async (o) => browserCalls.push(["download", o.url, o]),
      // Chrome's download bubble: each setUiOptions call's `enabled`.
      ...(firefox ? {} : { ui: [], setUiOptions: async (o) => chrome.downloads.ui.push(o.enabled) }),
    },
    // The cookie queries made, and the browser's cookie stores.
    cookies: {
      queries: [],
      stores: [{ id: "0", tabIds: [] }],
      getAll: async (q) => {
        chrome.cookies.queries.push(q);
        return [];
      },
      getAllCookieStores: async () => chrome.cookies.stores,
    },
    contextMenus: {
      // The menu as built: id -> its options.
      items: new Map(),
      removeAll: (cb) => {
        chrome.contextMenus.items.clear();
        if (cb) cb();
      },
      create: (o, cb) => {
        chrome.contextMenus.items.set(o.id, { ...o });
        if (cb) cb();
        return o.id;
      },
      update: (id, o, cb) => {
        Object.assign(chrome.contextMenus.items.get(id) || {}, o);
        if (cb) cb();
      },
      remove: (id, cb) => {
        chrome.contextMenus.items.delete(id);
        if (cb) cb();
      },
      onClicked: event(),
      // Firefox's menus.onShown, onHidden and refresh; Chrome has none.
      ...(firefox ? { onShown: event(), onHidden: event(), refreshed: 0, refresh: () => chrome.contextMenus.refreshed++ } : {}),
    },
    // The toolbar button: each setBadgeText and setIcon call, in order.
    action: {
      calls: [],
      setBadgeText: (o) => void chrome.action.calls.push(["badge", o]),
      setBadgeBackgroundColor: () => {},
      setIcon: (o) => void chrome.action.calls.push(["icon", o]),
    },
    notifications: safari ? undefined : {
      shown: [],
      // Firefox's notices take no buttons.
      create: async (id, options) => {
        if (firefox && options.buttons) throw new Error("buttons is unsupported by Firefox");
        chrome.notifications.shown.push([id, options]);
      },
      clear: async () => {},
      onClicked: event(),
      onButtonClicked: event(),
    },
    i18n: { getMessage, getUILanguage: () => "en-US" },
  };
  const dir = path.join(__dirname, "..", safari ? "safari" : firefox ? "firefox" : "chromium");
  // Safari's port polls for as long as the worker runs.
  const timeout = safari
    ? (f, ms, ...args) => {
        const t = setTimeout(f, ms, ...args);
        t.unref();
        return t;
      }
    : setTimeout;
  const context = vm.createContext({
    chrome,
    navigator: { userAgent: "test-agent" },
    console,
    setTimeout: timeout,
    clearTimeout,
    URL,
    TextEncoder,
    btoa,
    ...globals,
  });
  const run = (file) => vm.runInContext(fs.readFileSync(path.join(dir, file), "utf8"), context);
  if (firefox) {
    firefoxManifest().background.scripts.forEach(run);
  } else {
    context.importScripts = (...files) => files.forEach(run);
    run("background.js");
  }
  const popupUrl = chrome.runtime.getURL("popup.html");
  const popup = (msg) =>
    new Promise((resolve) => chrome.runtime.onMessage.fire(msg, { id: "udm", url: popupUrl }, resolve));
  // A message from the content script in tab `tabId`, on page `url`, from
  // its frame at `frameUrl` (the page itself unless given).
  // `incognito` makes the tab a private window's.
  const content = (msg, tabId, url = "https://site.net/watch", frameUrl = url, incognito = false) =>
    new Promise((resolve) =>
      chrome.runtime.onMessage.fire(msg, { id: "udm", tab: { id: tabId, url, incognito }, url: frameUrl }, resolve),
    );
  return { port, sent, browserCalls, chrome, popup, content, native };
}

function chromeManifest() {
  return JSON.parse(fs.readFileSync(path.join(__dirname, "..", "chromium", "manifest.json"), "utf8"));
}

// The Firefox copy's manifest.json, which installer/make-firefox.js makes
// from the Chromium one and firefox.json (firefox.test.js checks it is
// current).
function firefoxManifest() {
  return JSON.parse(fs.readFileSync(path.join(__dirname, "..", "firefox", "manifest.json"), "utf8"));
}

// The Safari copy's manifest.json, which installer/make-safari.js makes from
// the Chromium one and safari.json (safari.test.js checks it is current).
function safariManifest() {
  return JSON.parse(fs.readFileSync(path.join(__dirname, "..", "safari", "manifest.json"), "utf8"));
}

let nextDownloadId = 1;
function download(fields) {
  return { id: nextDownloadId++, url: "", referrer: "", filename: "", mime: "", totalBytes: 0, fileSize: 0, incognito: false, ...fields };
}

module.exports = { loadWorker, download, event, setBrowserProxy, firefoxManifest, safariManifest, getMessage };
