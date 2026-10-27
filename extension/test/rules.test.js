// Tests for the capture rules, and for the service worker applying them.
// Run with: node --test extension/test

const test = require("node:test");
const assert = require("node:assert/strict");

const { extensionOf, siteMatches, downloadType, captureDecision } = require("../rules.js");

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
