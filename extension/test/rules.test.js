// Tests for the capture rules, and for the service worker applying them.
// Run with: node --test extension/test

const test = require("node:test");
const assert = require("node:assert/strict");

const { extensionOf, siteMatches, downloadType, captureDecision, keyAction, clickApplies, urlMatches, selectionButtonAllowed } = require("../rules.js");

const RULES = {
  capture_types: ["zip", "pdf", "exe", "mp4"],
  capture_min_size_kb: 100,
  capture_excluded_sites: ["*.example.com", "files.test.org"],
};

test("extension of a name", () => {
  assert.equal(extensionOf("Report.PDF"), "pdf");
  assert.equal(extensionOf("C:\\dl\\a.tar.gz"), "gz");
  assert.equal(extensionOf("README"), "");
  assert.equal(extensionOf(".bashrc"), "");
  assert.equal(extensionOf("trailing."), "");
  assert.equal(extensionOf(undefined), "");
});

test("site patterns", () => {
  assert.ok(siteMatches("*.example.com", "example.com"));
  assert.ok(siteMatches("*.example.com", "cdn.example.com"));
  assert.ok(!siteMatches("*.example.com", "notexample.com"));
  assert.ok(siteMatches("example.com", "a.b.example.com"));
  assert.ok(!siteMatches("example.com", "example.com.evil.net"));
  assert.ok(siteMatches("dl*.mirror.net", "dl3.mirror.net"));
  assert.ok(!siteMatches("dl*.mirror.net", "www.mirror.net"));
  assert.ok(siteMatches("Example.com:8080", "example.com"));
  assert.ok(!siteMatches("", "example.com"));
  assert.ok(!siteMatches("example.com", ""));
});

test("type comes from the name, then the MIME type, then the URL", () => {
  assert.equal(downloadType({ filename: "a.zip", mime: "application/pdf", url: "https://x/b.exe" }), "zip");
  assert.equal(downloadType({ filename: "", mime: "application/pdf; charset=binary", url: "https://x/get.php" }), "pdf");
  assert.equal(downloadType({ filename: "", mime: "", url: "https://x/dl/movie.MP4?sig=1" }), "mp4");
  assert.equal(downloadType({ filename: "", mime: "", url: "https://x/download" }), "");
});

test("no rules yet: everything is captured", () => {
  assert.equal(captureDecision(null, { url: "https://x/a.iso" }).capture, true);
});

test("file types", () => {
  const take = (item) => captureDecision(RULES, { url: "https://dl.net/x", ...item }).capture;
  assert.equal(take({ filename: "setup.EXE" }), true);
  assert.equal(take({ filename: "photo.jpg" }), false);
  assert.equal(take({ mime: "video/mp4" }), true);
  assert.equal(take({ url: "https://dl.net/file.zip" }), true);
  assert.equal(take({ url: "https://dl.net/page.html" }), false);
  // Nothing says what it is yet: a generic binary is taken, anything else stays.
  assert.equal(take({ url: "https://dl.net/get?id=3", mime: "application/octet-stream" }), true);
  assert.equal(take({ url: "https://dl.net/get?id=3", mime: "text/html" }), false);
  // An empty list takes every type.
  assert.equal(captureDecision({ ...RULES, capture_types: [] }, { url: "https://dl.net/a.jpg" }).capture, true);
});

test("minimum size", () => {
  const take = (size) => captureDecision(RULES, { url: "https://dl.net/a.zip", size }).capture;
  assert.equal(take(50 * 1024), false);
  assert.equal(take(100 * 1024), true);
  assert.equal(take(5 * 1024 * 1024), true);
  // Size not known yet: only the name decides.
  assert.equal(take(0), true);
  assert.equal(take(undefined), true);
  assert.equal(captureDecision({ ...RULES, capture_min_size_kb: 0 }, { url: "https://dl.net/a.zip", size: 1 }).capture, true);
});

test("excluded sites, by download URL or referring page", () => {
  const take = (url, referrer) => captureDecision(RULES, { url, referrer }).capture;
  assert.equal(take("https://cdn.example.com/a.zip"), false);
  assert.equal(take("https://example.com/a.zip"), false);
  assert.equal(take("https://files.test.org/a.zip"), false);
  assert.equal(take("https://mirror.net/a.zip", "https://www.example.com/page"), false);
  assert.equal(take("https://mirror.net/a.zip", "https://other.org/"), true);
  assert.equal(take("https://test.org/a.zip"), true);
});

// --- The service worker with a stand-in chrome API (see harness.js) ---------

const { loadWorker, download } = require("./harness.js");

test("the worker asks for the rules and applies them to browser downloads", async () => {
  const w = loadWorker();
  assert.ok(w.sent.some((m) => m.op === "get_settings"), "asks for settings on connect");
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: RULES });

  const adds = () => w.sent.filter((m) => m.op === "add").map((m) => m.url);

  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/a.zip", totalBytes: 5 << 20 }));
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/b.jpg" }));
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/c.zip", totalBytes: 10 * 1024 }));
  await w.chrome.downloads.onCreated.fire(download({ url: "https://cdn.example.com/d.zip" }));
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/e", mime: "application/pdf" }));
  assert.deepEqual(adds(), ["https://dl.net/a.zip", "https://dl.net/e"]);
  // Only taken downloads are canceled in the browser.
  assert.equal(w.browserCalls.filter((c) => c[0] === "cancel").length, 2);

  // New rules apply at once.
  w.port.onMessage.fire({ op: "settings", settings: { ...RULES, capture_types: [] } });
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/f.jpg" }));
  assert.equal(adds().at(-1), "https://dl.net/f.jpg");
});

test("skip next download leaves exactly one download in the browser", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: RULES });

  const state = await w.popup({ type: "setSkipNext", value: true });
  assert.equal(state.skipNext, true);
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/1.zip" }));
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/2.zip" }));
  assert.deepEqual(
    w.sent.filter((m) => m.op === "add").map((m) => m.url),
    ["https://dl.net/2.zip"],
  );
  assert.equal((await w.popup({ type: "state" })).skipNext, false);
});

test("downloads waiting for the save-as dialog show in the popup and can be started", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  const item = { id: 7, url: "https://dl.net/a.zip", path: "C:\\dl\\a.zip", total: 10, done: 0, status: "confirming", error: null, resumable: true, connections: 0 };
  w.port.onMessage.fire({ op: "updated", item });
  const state = await w.popup({ type: "state" });
  assert.equal(state.items[0].status, "confirming");
  await w.popup({ type: "start", id: 7 });
  assert.equal(JSON.stringify(w.sent.at(-1)), JSON.stringify({ op: "confirm", id: 7, start: true }));
});

test("a taken name under ask waits in the popup and starts with a new name", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: { file_exists: "ask" } });
  w.port.onMessage.fire({ op: "added", id: 8, url: "https://dl.net/a.zip" });
  w.port.onMessage.fire({ op: "name_taken", id: 8, url: "https://dl.net/a.zip", path: "C:\\dl\\a.zip", total: 10 });
  const state = await w.popup({ type: "state" });
  assert.equal(state.items[0].status, "confirming");
  assert.equal(state.items[0].path, "C:\\dl\\a.zip");
  await w.popup({ type: "start", id: 8 });
  assert.equal(
    JSON.stringify(w.sent.at(-1)),
    JSON.stringify({ op: "confirm", id: 8, start: true, file_exists: "rename" }),
  );
});

test("download later from the context menu adds without starting", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "uidm-later-link", linkUrl: "https://dl.net/a.zip", pageUrl: "https://dl.net/" });
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "uidm-link", linkUrl: "https://dl.net/b.zip", pageUrl: "https://dl.net/" });
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "uidm-later-media", srcUrl: "https://dl.net/c.mp4", pageUrl: "https://dl.net/" });
  const adds = w.sent.filter((m) => m.op === "add").map((m) => [m.url, m.start]);
  assert.equal(JSON.stringify(adds), JSON.stringify([["https://dl.net/a.zip", false], ["https://dl.net/b.zip", undefined], ["https://dl.net/c.mp4", false]]));
});

test("held keys skip or force capture", () => {
  const keys = { capture_skip_key: "alt", capture_force_key: "ctrl" };
  assert.equal(keyAction(keys, { alt: true }), "skip");
  assert.equal(keyAction(keys, { ctrl: true }), "force");
  assert.equal(keyAction(keys, { shift: true }), "");
  assert.equal(keyAction(keys, {}), "");
  assert.equal(keyAction({ capture_skip_key: "none", capture_force_key: "none" }, { alt: true, ctrl: true }), "");
  assert.equal(keyAction(keys, { alt: true, ctrl: true }), "skip", "skipping wins");

  const jpg = { url: "https://dl.net/a.jpg" };
  assert.equal(captureDecision(RULES, jpg).capture, false, "no key: jpg is not a captured type");
  assert.equal(captureDecision(RULES, { ...jpg, key: "force" }).capture, true);
  const zip = { url: "https://dl.net/a.zip" };
  assert.equal(captureDecision(RULES, zip).capture, true);
  assert.equal(captureDecision(RULES, { ...zip, key: "skip" }).capture, false);

  assert.ok(clickApplies({ time: 1000 }, 3000));
  assert.ok(!clickApplies({ time: 1000 }, 7000), "too long after the click");
  assert.ok(!clickApplies(null, 1000));
});

test("the worker keeps a skip-key download and sends a force-key link", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: { ...RULES, capture_skip_key: "alt", capture_force_key: "ctrl" } });
  await w.content({ type: "link:skip" }, 3, "https://dl.net/");
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/a.zip" }));
  assert.equal(w.browserCalls.length, 0, "the skip click kept it in the browser");
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/b.zip" }));
  assert.deepEqual(w.browserCalls.map((c) => c[0]), ["cancel"], "only one download is skipped");

  const answer = await w.content({ type: "link:force", url: "https://dl.net/page.html" }, 3, "https://dl.net/");
  assert.equal(answer.ok, true);
  const add = w.sent.filter((m) => m.op === "add").at(-1);
  assert.equal(add.url, "https://dl.net/page.html");
  assert.equal(add.referrer, "https://dl.net/");
});

test("address exceptions with wildcards", () => {
  assert.ok(urlMatches("https://example.com/files/*", "https://example.com/files/a.zip"));
  assert.ok(urlMatches("https://example.com/files/*", "HTTPS://Example.com/files/sub/b.iso?x=1"));
  assert.ok(!urlMatches("https://example.com/files/*", "https://example.com/other/a.zip"));
  assert.ok(urlMatches("*.example.com/*.pdf", "https://cdn.example.com/docs/report.pdf"));
  assert.ok(!urlMatches("*.example.com/*.pdf", "https://cdn.example.com/docs/report.zip"));
  assert.ok(urlMatches("example.com/a.zip", "http://example.com/a.zip"), "no scheme matches either");
  assert.ok(!urlMatches("https://example.com/a.zip", "http://example.com/a.zip"));
  assert.ok(urlMatches("https://x.net/a(1).zip", "https://x.net/a(1).zip"), "other characters are literal");
  assert.ok(!urlMatches("", "https://x.net/a.zip"));

  const rules = { ...RULES, capture_excluded_urls: ["https://dl.net/keep/*"] };
  assert.equal(captureDecision(rules, { url: "https://dl.net/keep/a.zip" }).capture, false);
  assert.equal(captureDecision(rules, { url: "https://dl.net/take/a.zip" }).capture, true);
});

test("all links on a page go to the window's picker with cookies", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  w.chrome.cookies.getAll = async ({ url }) => (url.startsWith("https://dl.net") ? [{ name: "s", value: "1" }] : []);
  w.chrome.scripting = {
    executeScript: async ({ target }) => {
      assert.equal(target.allFrames, true);
      return [
        { result: [{ url: "https://dl.net/a.zip", text: "A" }, { url: "javascript:void(0)", text: "x" }] },
        { result: [{ url: "https://cdn.org/b.png", text: "", image: true }] },
      ];
    },
  };
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "uidm-all-links", pageUrl: "https://dl.net/list" }, { id: 4 });
  const msg = w.sent.find((m) => m.op === "pick_links");
  assert.equal(msg.page, "https://dl.net/list");
  assert.equal(JSON.stringify(msg.links.map((l) => [l.url, l.cookies ?? null])), JSON.stringify([["https://dl.net/a.zip", "s=1"], ["https://cdn.org/b.png", null]]));
});

test("where the selection button shows", () => {
  assert.ok(selectionButtonAllowed(null, "https://x.net/"), "on everywhere by default");
  assert.ok(selectionButtonAllowed({ selection_button: "everywhere" }, "https://x.net/"));
  assert.ok(!selectionButtonAllowed({ selection_button: "off" }, "https://x.net/"));
  const sites = { selection_button: "sites", selection_button_sites: ["*.example.com"] };
  assert.ok(selectionButtonAllowed(sites, "https://dl.example.com/list"));
  assert.ok(!selectionButtonAllowed(sites, "https://x.net/"));
});

test("the selection button asks for the selection's links only", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  let asked = null;
  w.chrome.scripting = {
    executeScript: async ({ args }) => {
      asked = args;
      return [{ result: [{ url: "https://dl.net/a.zip", text: "" }] }];
    },
  };
  const answer = await w.content({ type: "selection:links" }, 5, "https://dl.net/list");
  assert.equal(answer.ok, true);
  assert.equal(JSON.stringify(asked), "[true]");
  assert.equal(w.sent.at(-1).op, "pick_links");
});

test("the worker runs as a Firefox background script", async () => {
  const w = loadWorker({ firefox: true });
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: RULES });
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/a.zip" }));
  assert.equal(w.sent.find((m) => m.op === "add").url, "https://dl.net/a.zip");
  // The popup, opened as a tab at moz-extension://, is still the popup.
  const state = await new Promise((resolve) =>
    w.chrome.runtime.onMessage.fire({ type: "state" }, { id: "uidm", tab: { id: 9 }, url: "moz-extension://0b1c/popup.html" }, resolve),
  );
  assert.equal(state.connected, true);
});

test("a site crawl gets the browser's cookies for its start page", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  w.chrome.cookies.getAll = async () => [{ name: "session", value: "abc" }];
  w.port.onMessage.fire({ op: "cookies_wanted", request: 3, url: "https://site.net/members/" });
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(JSON.stringify(w.sent.at(-1)), JSON.stringify({ op: "cookies", request: 3, cookies: "session=abc" }));
});