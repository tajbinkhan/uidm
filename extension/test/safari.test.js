// The Safari version of the extension (#268): extension/safari, the copy
// installer/make-safari.js makes from extension/chromium and
// extension/safari.json; the polling port that stands in for a native port
// there (native.js); and the worker in Safari, which has no downloads API,
// proxy settings or notifications and takes download links as they are
// clicked instead.

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { makeSafari, checkSafari, safariManifest, COPY } = require("../../installer/make-safari.js");
const { pollingPort, SAFARI_POLL_GAP_MS, SAFARI_REQUEST_TIMEOUT_MS } = require("../chromium/native.js");
const { clickTaken } = require("../chromium/rules.js");
const { loadWorker } = require("./harness.js");

const CHROMIUM = path.join(__dirname, "..", "chromium");

// --- The Safari copy -----------------------------------------------------------

test("extension/safari is the Safari copy of extension/chromium", () => {
  assert.equal(COPY, path.join(__dirname, "..", "safari"));
  assert.deepEqual(checkSafari(), [], "remake it with: node installer/make-safari.js");
});

test("a fresh Safari copy passes the check, and a changed file fails it", (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "udm-safari-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const out = path.join(dir, "safari");
  makeSafari(out);
  assert.deepEqual(checkSafari(out), []);
  for (const file of ["background.js", "native.js", "rules.js", "content.js", "popup.html"]) {
    assert.ok(fs.readFileSync(path.join(CHROMIUM, file)).equals(fs.readFileSync(path.join(out, file))), file);
  }
  fs.writeFileSync(path.join(out, "native.js"), "// old\n");
  assert.deepEqual(checkSafari(out), ["changed: native.js"]);
  const manifest = JSON.parse(fs.readFileSync(path.join(out, "manifest.json"), "utf8"));
  manifest.permissions.push("downloads");
  fs.writeFileSync(path.join(out, "manifest.json"), JSON.stringify(manifest));
  assert.deepEqual(checkSafari(out), ["changed: native.js", "changed: manifest.json key \"permissions\""]);
});

test("Safari's manifest asks only for what Safari has, and names its oldest Safari", () => {
  const safari = safariManifest();
  const chromium = JSON.parse(fs.readFileSync(path.join(CHROMIUM, "manifest.json"), "utf8"));
  // Safari has no downloads API, proxy settings or notifications, and
  // cannot block requests.
  for (const missing of ["downloads", "downloads.ui", "proxy", "notifications", "webRequestBlocking"]) {
    assert.equal(safari.permissions.includes(missing), false, missing);
  }
  for (const kept of ["nativeMessaging", "cookies", "contextMenus", "storage", "scripting", "webNavigation"]) {
    assert.ok(safari.permissions.includes(kept), kept);
  }
  assert.ok(safari.permissions.every((p) => chromium.permissions.includes(p)), "nothing Chrome does not ask for");
  assert.equal("key" in safari, false, "Chrome's key means nothing to Safari");
  assert.deepEqual(safari.browser_specific_settings, { safari: { strict_min_version: "16.4" } });
  // The same worker and content scripts as Chrome's, all of them there.
  assert.deepEqual(safari.background, chromium.background);
  assert.deepEqual(safari.content_scripts, chromium.content_scripts);
  assert.equal(safari.version, chromium.version);
  const worker = fs.readFileSync(path.join(COPY, "background.js"), "utf8");
  const imported = /importScripts\(([^)]*)\)/.exec(worker)[1].match(/"[^"]+"/g).map((s) => JSON.parse(s));
  assert.ok(imported.includes("native.js"));
  for (const file of [...imported, ...safari.content_scripts[0].js, safari.background.service_worker]) {
    assert.ok(fs.existsSync(path.join(COPY, file)), file);
  }
});

test("Firefox loads native.js too, before the worker", () => {
  const scripts = require("./harness.js").firefoxManifest().background.scripts;
  assert.ok(scripts.indexOf("native.js") >= 0 && scripts.indexOf("native.js") < scripts.indexOf("background.js"));
});

// --- The polling port -----------------------------------------------------------

// A clock whose timers run only when the test says: `advance(ms)` moves time
// on and runs the timers that fell due, then lets answers settle.
function fakeClock() {
  let now = 1000;
  let next = 1;
  const timers = new Map();
  const settle = async () => {
    for (let i = 0; i < 10; i++) await Promise.resolve();
  };
  const clock = {
    now: () => now,
    setTimeout: (f, ms) => {
      timers.set(next, { f, at: now + ms });
      return next++;
    },
    clearTimeout: (id) => timers.delete(id),
    advance: async (ms) => {
      const until = now + ms;
      for (let steps = 0; ; steps++) {
        if (steps > 10000) throw new Error("timers keep firing");
        await settle();
        const due = [...timers].filter(([, t]) => t.at <= until).sort((x, y) => x[1].at - y[1].at)[0];
        if (!due) break;
        timers.delete(due[0]);
        now = Math.max(now, due[1].at);
        due[1].f();
      }
      now = until;
      await settle();
    },
  };
  return clock;
}

// A port over a fake app extension: `requests` holds what it was sent, and
// `answer(request)` answers it; by default a poll gets `frames` once (then
// it is held, as a long poll is, until `release()`), and messages get
// nothing.
function fakePort() {
  const clock = fakeClock();
  const requests = [];
  const pending = [];
  const p = {
    clock,
    requests,
    frames: [],
    heard: [],
    closed: [],
    answer: (request) => {
      if (!request.poll) return Promise.resolve({ session: "a", frames: [] });
      if (p.frames.length) return Promise.resolve({ session: "a", frames: p.frames.splice(0) });
      return new Promise((resolve) => pending.push(resolve));
    },
    // Answer the polls held so far, with `reply`.
    release: (reply = { session: "a", frames: [] }) => pending.splice(0).forEach((resolve) => resolve(reply)),
    polls: () => requests.filter((r) => r.poll).length,
  };
  p.port = pollingPort((request) => {
    requests.push(request);
    return p.answer(request);
  }, clock);
  p.port.onMessage.addListener((m) => p.heard.push(m));
  p.port.onDisconnect.addListener((port) => p.closed.push(port.udmReason));
  return p;
}

test("a port's first request starts udm-host and carries what was posted; polls follow its answer", async () => {
  const p = fakePort();
  p.port.postMessage({ op: "hello" });
  p.port.postMessage({ op: "list" });
  assert.equal(p.requests.length, 0, "nothing goes until the worker's turn is over");
  p.frames.push({ op: "hello" }, { op: "downloads", items: [] }, "not a frame", null);
  await p.clock.advance(0);
  assert.deepEqual(p.requests.slice(0, 2), [{ frames: [{ op: "hello" }, { op: "list" }], start: true }, { poll: true }]);
  assert.deepEqual(p.heard, [{ op: "hello" }, { op: "downloads", items: [] }]);
  // The poll that brought frames is followed at once by one that waits.
  assert.equal(p.polls(), 2);
  assert.deepEqual(p.closed, []);

  // A port that was sent nothing still starts udm-host.
  const quiet = fakePort();
  await quiet.clock.advance(0);
  assert.deepEqual(quiet.requests, [{ frames: [], start: true }, { poll: true }]);
});

test("messages go out at once beside the poll that waits, and never carry start again", async () => {
  const p = fakePort();
  p.port.postMessage({ op: "hello" });
  await p.clock.advance(0);
  assert.equal(p.polls(), 1, "the poll is held");
  p.port.postMessage({ op: "add", url: "https://a/b.zip" });
  p.port.postMessage({ op: "list" });
  await p.clock.advance(0);
  assert.deepEqual(p.requests.at(-1), { frames: [{ op: "add", url: "https://a/b.zip" }, { op: "list" }] });
  assert.equal(p.polls(), 1, "still one poll out");
  // News ends the held poll; another goes out at once.
  p.release({ session: "a", frames: [{ op: "ack", id: 1 }] });
  await p.clock.advance(0);
  assert.deepEqual(p.heard, [{ op: "ack", id: 1 }]);
  assert.equal(p.polls(), 2);
});

test("a poll that comes back empty at once is not repeated within a second", async () => {
  const p = fakePort();
  p.answer = () => Promise.resolve({ session: "a", frames: [] });
  await p.clock.advance(0);
  assert.equal(p.polls(), 1);
  await p.clock.advance(SAFARI_POLL_GAP_MS - 1);
  assert.equal(p.polls(), 1);
  await p.clock.advance(1);
  assert.equal(p.polls(), 2);
  // One that says there is more is followed at once.
  let more = true;
  p.answer = (request) => {
    const reply = { session: "a", frames: [], more: !!request.poll && more };
    more = false;
    return Promise.resolve(reply);
  };
  await p.clock.advance(SAFARI_POLL_GAP_MS);
  assert.equal(p.polls(), 4, "the one that said more, then another at once");
});

test("a request with no answer closes the port, so it does not look connected", async () => {
  const p = fakePort();
  p.port.postMessage({ op: "hello" });
  await p.clock.advance(0);
  // The long poll is held; past the timeout it counts as lost.
  await p.clock.advance(SAFARI_REQUEST_TIMEOUT_MS - 1);
  assert.deepEqual(p.closed, []);
  await p.clock.advance(1);
  assert.deepEqual(p.closed, ["no answer from UDM"]);
});

test("the port closes, saying why, when udm-host is gone, starts again, or the browser fails", async () => {
  let p = fakePort();
  p.port.postMessage({ op: "hello" });
  await p.clock.advance(0);
  p.release({ session: "a", frames: [{ op: "hello" }], error: "UDM is not running" });
  await p.clock.advance(0);
  assert.deepEqual(p.heard, [{ op: "hello" }], "what came before the error still counts");
  assert.deepEqual(p.closed, ["UDM is not running"]);
  const asked = p.requests.length;
  await p.clock.advance(SAFARI_REQUEST_TIMEOUT_MS * 2);
  assert.equal(p.requests.length, asked, "no more requests");
  assert.deepEqual(p.closed, ["UDM is not running"], "and no timeout after closing");
  assert.throws(() => p.port.postMessage({ op: "list" }), /disconnected/);

  p = fakePort();
  p.port.postMessage({ op: "hello" });
  await p.clock.advance(0);
  p.release({ session: "b", frames: [{ op: "progress" }] });
  await p.clock.advance(0);
  assert.deepEqual(p.heard, [], "another udm-host's frames are not this port's");
  assert.deepEqual(p.closed, ["UDM's host started again"]);

  p = fakePort();
  p.answer = () => Promise.reject(new Error("No native handler"));
  p.port.postMessage({ op: "hello" });
  await p.clock.advance(0);
  assert.deepEqual(p.closed, ["No native handler"]);

  p = fakePort();
  p.answer = () => Promise.resolve(undefined);
  p.port.postMessage({ op: "hello" });
  await p.clock.advance(0);
  assert.deepEqual(p.closed, ["no answer from UDM"]);
});

test("disconnecting the port stops it without telling its own listeners", async () => {
  const p = fakePort();
  p.port.postMessage({ op: "hello" });
  await p.clock.advance(0);
  p.port.disconnect();
  p.release({ session: "a", frames: [{ op: "progress" }] });
  await p.clock.advance(SAFARI_REQUEST_TIMEOUT_MS * 2);
  assert.deepEqual(p.closed, []);
  assert.deepEqual(p.heard, []);
  assert.equal(p.polls(), 1);
});

// --- Links taken as they are clicked ---------------------------------------------

test("a click on a .torrent file is taken when UDM's settings take torrent files", () => {
  const rules = { capture_types: ["zip"], capture_excluded_sites: ["skip.net"], capture_excluded_urls: [], torrents: { catch_torrent_files: true } };
  assert.equal(clickTaken(rules, "https://cdn.net/a.torrent", null, "https://site.net/"), true, "not a captured type, but a torrent");
  assert.equal(clickTaken(rules, "https://cdn.net/get?id=3", "Album.torrent", "https://site.net/"), true);
  assert.equal(clickTaken(rules, "https://skip.net/a.torrent", null, ""), false, "excluded sites still win");
  const off = { ...rules, torrents: { catch_torrent_files: false } };
  assert.equal(clickTaken(off, "https://cdn.net/a.torrent", null, ""), false);
});

test("a click is taken only for a link to a file of a captured type", () => {
  const rules = { capture_types: ["zip", "pdf", "mp4", "rs"], capture_excluded_sites: ["skip.net"], capture_excluded_urls: [] };
  assert.equal(clickTaken(rules, "https://cdn.net/files/a.zip", null, "https://site.net/"), true);
  assert.equal(clickTaken(rules, "https://cdn.net/files/a.ZIP?x=1", null, "https://site.net/"), true);
  // Named by its download attribute.
  assert.equal(clickTaken(rules, "https://cdn.net/get?id=7", "Report.pdf", "https://site.net/"), true);
  // Not a type to take, a page, a picture, or no file name at all.
  assert.equal(clickTaken(rules, "https://cdn.net/a.exe", null, ""), false);
  assert.equal(clickTaken(rules, "https://docs.net/guide.md", null, ""), false);
  assert.equal(clickTaken(rules, "https://site.net/news/", null, ""), false);
  assert.equal(clickTaken(rules, "https://site.net/a.html", null, ""), false);
  assert.equal(clickTaken(rules, "https://cdn.net/get?id=7", null, ""), false);
  // Excluded sites, the link's or the page's.
  assert.equal(clickTaken(rules, "https://skip.net/a.zip", null, ""), false);
  assert.equal(clickTaken(rules, "https://cdn.net/a.zip", null, "https://www.skip.net/page"), false);
  // Not a web address.
  assert.equal(clickTaken(rules, "ftp://cdn.net/a.zip", null, ""), false);
  assert.equal(clickTaken(rules, "javascript:void(0)", null, ""), false);
  // Without rules, or with no types listed (where a browser download would
  // be taken whatever it is), no click is: a page such as a .rs, .md or .cfm
  // file would go to UDM.
  assert.equal(clickTaken(null, "https://cdn.net/a.zip", null, ""), false);
  assert.equal(clickTaken({ capture_types: [] }, "https://cdn.net/a.zip", null, ""), false);
  assert.equal(clickTaken({}, "https://forge.net/src/main.rs", null, ""), false);
  assert.equal(clickTaken({ capture_types: [] }, "https://site.net/index.cfm", null, ""), false);
});

// --- The worker in Safari ------------------------------------------------------------

const SETTINGS = { capture_types: ["zip"], capture_min_size_kb: 0, capture_excluded_sites: ["skip.net"], capture_excluded_urls: [] };

// A Safari worker that has heard UDM's hello and settings.
async function safariWorker() {
  const w = loadWorker({ safari: true });
  w.native.frames.push({ op: "hello", name: "udm-daemon", version: "x" }, { op: "settings", settings: SETTINGS });
  await w.native.settle();
  return w;
}

// Values from the worker come from another realm: compare them as JSON.
const json = (v) => JSON.parse(JSON.stringify(v));
const lastAdd = (w) => w.sent.filter((m) => m.op === "add").at(-1);
// The Safari worker's timers do not keep Node running: wait for `promise`
// with a timer of the test's own.
async function held(promise) {
  const keep = setInterval(() => {}, 50);
  try {
    return await promise;
  } finally {
    clearInterval(keep);
  }
}
const shared = (w) => json(w.chrome.storage.written.filter((o) => "linkCapture" in o).map((o) => o.linkCapture));

test("in Safari the worker starts without the APIs Safari lacks and talks to udm-host by polling", async () => {
  const w = loadWorker({ safari: true });
  assert.equal(w.chrome.downloads, undefined);
  assert.equal(w.chrome.notifications, undefined);
  assert.equal(w.chrome.proxy, undefined);
  await w.native.settle();
  // The port connectNative returns is never used.
  assert.equal(w.port.onMessage.options.length, 0);
  assert.deepEqual(json(w.native.requests[0].frames.map((m) => m.op)), ["hello", "list", "get_settings"]);
  assert.equal(w.native.requests[0].start, true, "a new port starts a new udm-host");
  assert.equal(w.native.requests[1].poll, true);
  const status = await w.popup({ type: "state" });
  assert.equal(status.connected, false);

  w.native.frames.push({ op: "hello", name: "udm-daemon", version: "x" });
  await new Promise((r) => setTimeout(r, 1100));
  assert.equal((await w.popup({ type: "state" })).connected, true);
});

test("in Safari udm-host gone shows as disconnected, with its reason", async () => {
  const w = loadWorker({ safari: true });
  w.native.reply = () => ({ session: "host-1", frames: [], error: "UDM is not running" });
  await w.native.settle();
  const status = await w.popup({ type: "state" });
  assert.equal(status.connected, false);
  assert.equal(status.lastError, "UDM is not running");
});

test("in Safari the worker shares whether and which links to take, and takes a clicked one", async () => {
  const w = await safariWorker();
  assert.deepEqual(shared(w).at(-1), {
    on: true,
    rules: { capture_types: ["zip"], capture_excluded_sites: ["skip.net"], capture_excluded_urls: [], torrents: { catch_torrent_files: false } },
  });

  const reply = await w.content({ type: "link:capture", url: "https://cdn.net/a.zip", download: "../Big file.zip" }, 4, "https://site.net/list");
  assert.deepEqual(json(reply), { ok: true });
  await w.native.settle();
  const add = lastAdd(w);
  assert.deepEqual([add.url, add.referrer, add.page, add.filename], ["https://cdn.net/a.zip", "https://site.net/list", "https://site.net/list", "Big file.zip"]);
  assert.equal("proxy" in add || "browser_proxy" in add, false, "Safari tells no proxy");

  // Capture off in the popup: shared, and a click that came anyway stays.
  await w.popup({ type: "setCapture", value: false });
  assert.equal(shared(w).at(-1).on, false);
  const before = w.sent.length;
  assert.deepEqual(json(await w.content({ type: "link:capture", url: "https://cdn.net/b.zip" }, 4)), { ok: false });
  assert.equal(w.sent.length, before);
  await w.popup({ type: "setCapture", value: true });

  // The rules say no, or "Skip next download" keeps one.
  assert.deepEqual(json(await w.content({ type: "link:capture", url: "https://skip.net/b.zip" }, 4)), { ok: false });
  await w.popup({ type: "setSkipNext", value: true });
  assert.deepEqual(json(await w.content({ type: "link:capture", url: "https://cdn.net/c.zip" }, 4)), { ok: false });
  assert.equal((await w.popup({ type: "state" })).skipNext, false);
  assert.deepEqual(json(await w.content({ type: "link:capture", url: "https://cdn.net/c.zip" }, 4)), { ok: true });
});

test("in Safari a link UDM refuses opens in a background tab, and a menu click without UDM too", async () => {
  const w = await safariWorker();
  await w.content({ type: "link:capture", url: "https://cdn.net/a.zip" }, 4, "https://site.net/");
  w.native.frames.push({ op: "rejected", url: "https://cdn.net/a.zip", message: "no" });
  await new Promise((r) => setTimeout(r, 1100));
  assert.deepEqual(json(w.chrome.tabs.created), [{ url: "https://cdn.net/a.zip", active: false }]);

  const away = loadWorker({ safari: true });
  away.native.reply = () => ({ session: "host-1", frames: [], error: "UDM is not running" });
  await away.native.settle();
  await away.chrome.contextMenus.onClicked.fire({ menuItemId: "udm-link", linkUrl: "https://cdn.net/b.zip", pageUrl: "https://site.net/" }, { id: 1 });
  assert.deepEqual(json(away.chrome.tabs.created), [{ url: "https://cdn.net/b.zip", active: false }]);
  assert.equal(shared(away).at(-1).on, false);
});

test("in Safari nothing is taken on a click before UDM's rules are here, or with no types listed", async () => {
  const w = loadWorker({ safari: true });
  // A worker that starts again clears what the last one shared.
  assert.deepEqual(shared(w)[0], { on: false, rules: null });
  w.native.frames.push({ op: "hello", name: "udm-daemon", version: "x" });
  await w.native.settle();
  assert.equal((await w.popup({ type: "state" })).connected, true);
  assert.equal(shared(w).at(-1).on, false, "connected, but no rules yet");

  w.native.frames.push({ op: "settings", settings: { ...SETTINGS, capture_types: [] } });
  await new Promise((r) => setTimeout(r, 1100));
  assert.deepEqual(shared(w).at(-1), {
    on: true,
    rules: { capture_types: [], capture_excluded_sites: ["skip.net"], capture_excluded_urls: [], torrents: { catch_torrent_files: false } },
  });
  // The worker checks the link again: with no types listed, none is taken.
  const before = w.sent.length;
  assert.deepEqual(json(await w.content({ type: "link:capture", url: "https://forge.net/src/main.rs" }, 4)), { ok: false });
  assert.deepEqual(json(await w.content({ type: "link:capture", url: "https://cdn.net/a.zip" }, 4)), { ok: false });
  assert.equal(w.sent.length, before);
});

test("in Safari a click that wakes the worker waits for UDM to connect", async () => {
  const w = loadWorker({ safari: true });
  const answer = w.content({ type: "link:capture", url: "https://cdn.net/a.zip" }, 4, "https://site.net/");
  await new Promise((r) => setTimeout(r, 200));
  w.native.frames.push({ op: "hello", name: "udm-daemon", version: "x" }, { op: "settings", settings: SETTINGS });
  assert.deepEqual(json(await held(answer)), { ok: true });
  await w.native.settle();
  assert.equal(lastAdd(w).url, "https://cdn.net/a.zip");
});

test("in Safari a clicked link UDM never acknowledged goes back to the browser when the connection drops", async () => {
  const w = await safariWorker();
  assert.deepEqual(json(await w.content({ type: "link:capture", url: "https://cdn.net/a.zip" }, 4, "https://site.net/")), { ok: true });
  assert.deepEqual(json(await w.content({ type: "link:capture", url: "https://cdn.net/b.zip" }, 4, "https://site.net/")), { ok: true });
  w.native.frames.push({ op: "ack", id: 7, url: "https://cdn.net/b.zip" });
  await new Promise((r) => setTimeout(r, 1100));
  w.native.reply = () => ({ session: "host-1", frames: [], error: "UDM is not running" });
  await new Promise((r) => setTimeout(r, 1100));
  assert.equal((await w.popup({ type: "state" })).connected, false);
  // Only the one UDM never acknowledged.
  assert.deepEqual(json(w.chrome.tabs.created), [{ url: "https://cdn.net/a.zip", active: false }]);
});

test("in Safari a link clicked in a frame made from nothing has the page as its referrer", async () => {
  const w = await safariWorker();
  await w.content({ type: "link:capture", url: "https://cdn.net/a.zip" }, 4, "https://site.net/page", "about:blank");
  await w.native.settle();
  assert.deepEqual([lastAdd(w).referrer, lastAdd(w).page], ["https://site.net/page", "https://site.net/page"]);
});

test("in Safari the worker uses the browser namespace's sendNativeMessage when there is one", async () => {
  const calls = [];
  const browser = {
    runtime: {
      sendNativeMessage: async (host, request) => {
        calls.push([host, request]);
        return { session: "s", frames: [] };
      },
    },
  };
  const w = loadWorker({ safari: true, globals: { browser } });
  await w.native.settle();
  assert.equal(w.native.requests.length, 0);
  assert.equal(calls[0][0], "com.udm.host");
  assert.equal(calls[0][1].start, true);
});

test("in Safari the popup lists no browser downloads", async () => {
  const w = await safariWorker();
  assert.deepEqual(json(await w.popup({ type: "browserDownloads" })), { items: [] });
  assert.deepEqual(json(await w.popup({ type: "sendBrowserDownload", id: 3 })), { ok: false });
});

test("Chrome's worker never shares link capture or takes a clicked link", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: SETTINGS });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(shared(w), []);
  const before = w.sent.length;
  assert.deepEqual(json(await w.content({ type: "link:capture", url: "https://cdn.net/a.zip" }, 4)), { ok: false });
  assert.equal(w.sent.length, before);
});
