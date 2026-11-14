// Tests for the capture rules, and for the service worker applying them.
// Run with: node --test extension/test

const test = require("node:test");
const assert = require("node:assert/strict");

const { isMagnet, isTorrentFile, catchesTorrents, extensionOf, siteMatches, downloadType, captureDecision, keyAction, parseKeys, trackKey, forceLeavesLink, clickApplies, urlMatches, selectionButtonAllowed } = require("../chromium/rules.js");

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
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
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
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
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
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  const item = { id: 7, url: "https://dl.net/a.zip", path: "C:\\dl\\a.zip", total: 10, done: 0, status: "confirming", error: null, resumable: true, connections: 0 };
  w.port.onMessage.fire({ op: "updated", item });
  const state = await w.popup({ type: "state" });
  assert.equal(state.items[0].status, "confirming");
  await w.popup({ type: "start", id: 7 });
  assert.equal(JSON.stringify(w.sent.at(-1)), JSON.stringify({ op: "confirm", id: 7, start: true }));
});

test("a taken name under ask waits in the popup and starts with a new name", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
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
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "udm-later-link", linkUrl: "https://dl.net/a.zip", pageUrl: "https://dl.net/" });
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "udm-link", linkUrl: "https://dl.net/b.zip", pageUrl: "https://dl.net/" });
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "udm-later-media", srcUrl: "https://dl.net/c.mp4", pageUrl: "https://dl.net/" });
  const adds = w.sent.filter((m) => m.op === "add").map((m) => [m.url, m.start]);
  assert.equal(JSON.stringify(adds), JSON.stringify([["https://dl.net/a.zip", false], ["https://dl.net/b.zip", undefined], ["https://dl.net/c.mp4", false]]));
});

test("the force key can leave pages, pictures and scripts alone", () => {
  const on = { capture_force_skips_pages: true };
  assert.ok(forceLeavesLink(on, "https://a.net/news/story.html"));
  assert.ok(forceLeavesLink(on, "https://a.net/img/logo.PNG?x=1"));
  assert.ok(forceLeavesLink(on, "https://a.net/app.js"));
  assert.ok(forceLeavesLink(on, "https://a.net/news/"));
  assert.ok(forceLeavesLink(on, "https://a.net"));
  assert.ok(!forceLeavesLink(on, "https://a.net/get.php/setup.exe"));
  assert.ok(!forceLeavesLink(on, "https://a.net/files/setup.exe"));
  assert.ok(!forceLeavesLink(on, "https://a.net/download"), "a name without a type may be a file");
  assert.ok(!forceLeavesLink({ capture_force_skips_pages: false }, "https://a.net/a.html"));
});

test("Insert and Del are followed while held and forgotten on blur", () => {
  const held = { insert: false, del: false };
  trackKey(held, "keydown", "Delete");
  assert.deepEqual(held, { insert: false, del: true });
  trackKey(held, "keydown", "Insert");
  trackKey(held, "keyup", "Delete");
  assert.deepEqual(held, { insert: true, del: false });
  trackKey(held, "keydown", "Delete");
  trackKey(held, "keydown", "a");
  assert.deepEqual(held, { insert: true, del: true }, "other keys change nothing");
  trackKey(held, "blur");
  assert.deepEqual(held, { insert: false, del: false });
});

test("held keys skip or force capture", () => {
  const keys = { capture_skip_key: "alt", capture_force_key: "ctrl" };
  assert.equal(keyAction(keys, { alt: true }), "skip");
  assert.equal(keyAction(keys, { ctrl: true }), "force");
  assert.equal(keyAction(keys, { shift: true }), "");
  assert.equal(keyAction(keys, {}), "");
  assert.equal(keyAction({ capture_skip_key: "none", capture_force_key: "none" }, { alt: true, ctrl: true }), "");
  assert.equal(keyAction(keys, { alt: true, ctrl: true }), "", "a combination is held exactly");

  const combos = { capture_skip_key: "alt+shift", capture_force_key: "ctrl+insert" };
  assert.equal(keyAction(combos, { alt: true, shift: true }), "skip");
  assert.equal(keyAction(combos, { alt: true }), "");
  assert.equal(keyAction(combos, { ctrl: true, insert: true }), "force");
  assert.equal(keyAction(combos, { ctrl: true }), "");
  assert.deepEqual(parseKeys("Shift+ctrl"), { alt: false, ctrl: true, shift: true, insert: false, del: false });
  assert.deepEqual(parseKeys(undefined), { alt: false, ctrl: false, shift: false, insert: false, del: false });

  // Del can force capture too, alone or with modifiers.
  const del = { capture_skip_key: "alt", capture_force_key: "del" };
  assert.equal(keyAction(del, { del: true }), "force");
  assert.equal(keyAction(del, { del: true, ctrl: true }), "", "held exactly");
  assert.equal(keyAction(del, { insert: true }), "");
  assert.equal(keyAction({ capture_force_key: "ctrl+delete" }, { ctrl: true, del: true }), "force");
  assert.deepEqual(parseKeys("ctrl+del"), { alt: false, ctrl: true, shift: false, insert: false, del: true });

  const left = { ...keys, capture_keys_left_click: true };
  assert.equal(keyAction(left, { alt: true, button: 0, mouse: true }), "skip");
  assert.equal(keyAction(left, { alt: true, button: 1, mouse: true }), "", "a middle click");
  assert.equal(keyAction(left, { alt: true, button: 0, mouse: false }), "", "Enter on a link");
  assert.equal(keyAction(keys, { alt: true, button: 1, mouse: true }), "skip", "any click without the setting");

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
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: { ...RULES, capture_skip_key: "alt", capture_force_key: "ctrl" } });
  await w.content({ type: "link:skip" }, 3, "https://dl.net/");
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/a.zip" }));
  assert.equal(w.browserCalls.length, 0, "the skip click kept it in the browser");
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/b.zip" }));
  assert.deepEqual(w.browserCalls.map((c) => c[0]), ["cancel", "erase"], "only one download is skipped");

  const answer = await w.content({ type: "link:force", url: "https://dl.net/page.html" }, 3, "https://dl.net/");
  assert.equal(answer.ok, true);
  const add = w.sent.filter((m) => m.op === "add").at(-1);
  assert.equal(add.url, "https://dl.net/page.html");
  assert.equal(add.referrer, "https://dl.net/");
});

test("magnet links and torrent files", () => {
  const hash = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
  assert.ok(isMagnet(`magnet:?xt=urn:btih:${hash}&dn=a`));
  assert.ok(isMagnet(`MAGNET:?dn=a&xt=urn:btih:${hash.toUpperCase()}`));
  assert.ok(isMagnet("magnet:?xt=urn:btih:YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK"), "base32");
  assert.ok(!isMagnet("magnet:?xt=urn:ed2k:31d6cfe0d16ae931b73c59d7e0c089c0"), "not BitTorrent");
  assert.ok(!isMagnet(`magnet:?xt=urn:btih:${hash}0`));
  assert.ok(!isMagnet(`https://x/?xt=urn:btih:${hash}`));
  assert.ok(isMagnet(`magnet:?dn=a&xt=urn%3Abtih%3A${hash}`), "percent-encoded, as the daemon reads it too");
  assert.ok(isMagnet(`magnet:?xt.1=urn:btih:${hash}`));
  assert.ok(!isMagnet("magnet:?xt=urn%3Abtih%3A%ZZ"));
  assert.ok(isTorrentFile({ url: "https://x/a.torrent" }));
  assert.ok(isTorrentFile({ url: "https://x/get?id=1", mime: "application/x-bittorrent; charset=binary" }));
  assert.ok(isTorrentFile({ url: "https://x/get?id=1", filename: "Album.TORRENT" }));
  assert.ok(!isTorrentFile({ url: "https://x/a.zip" }));
  assert.ok(catchesTorrents({ torrents: { catch_torrent_files: true } }));
  assert.ok(!catchesTorrents({ torrents: { catch_torrent_files: false } }));
  assert.ok(!catchesTorrents({}), "an older UDM takes no torrents");

  // A torrent file goes to UDM whatever its size and the captured types,
  // unless its site or address is excluded or the skip key is held.
  const rules = { ...RULES, torrents: { catch_torrent_files: true } };
  const torrent = { url: "https://dl.net/a.torrent", size: 2000 };
  assert.equal(captureDecision(rules, torrent).capture, true);
  assert.equal(captureDecision(rules, { ...torrent, key: "skip" }).capture, false);
  assert.equal(captureDecision(rules, { ...torrent, url: "https://cdn.example.com/a.torrent" }).capture, false);
  assert.equal(captureDecision(RULES, torrent).capture, false, "UDM's settings say no: small, and not a captured type");
});

test("the worker hands a clicked magnet link to UDM while capture is on", async () => {
  const w = loadWorker();
  const magnet = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=Album";
  assert.equal((await w.content({ type: "link:magnet", url: magnet }, 3, "https://site.net/")).ok, false, "not connected");
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: RULES });
  assert.ok(w.chrome.storage.written.some((o) => o.magnetCapture === true), "content scripts hear magnet links go to UDM");
  const answer = await w.content({ type: "link:magnet", url: magnet }, 3, "https://site.net/");
  assert.equal(answer.ok, true);
  const add = w.sent.filter((m) => m.op === "add").at(-1);
  assert.equal(add.url, magnet);
  const before = w.sent.length;
  assert.equal((await w.content({ type: "link:magnet", url: "https://site.net/a.zip" }, 3)).ok, false);
  // A page on a site the settings leave to the browser keeps its magnet links.
  assert.equal((await w.content({ type: "link:magnet", url: magnet }, 3, "https://cdn.example.com/")).ok, false);
  assert.equal(w.sent.length, before);
});

test("a .torrent download goes to UDM with its type, even without a request to tell it", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: { ...RULES, torrents: { catch_torrent_files: true } } });
  await w.chrome.downloads.onCreated.fire(
    download({ url: "https://dl.net/download.php?id=3", filename: "/home/u/Downloads/x.torrent", mime: "application/x-bittorrent" }),
  );
  const add = w.sent.filter((m) => m.op === "add").at(-1);
  assert.equal(add.url, "https://dl.net/download.php?id=3");
  assert.equal(add.content_type, "application/x-bittorrent");
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

  // Masks from UDM's default list, which carry no scheme.
  assert.ok(urlMatches("*.gstatic.com/*/sounds/*.mp3", "https://ssl.gstatic.com/chat/sounds/ding.mp3"));
  assert.ok(urlMatches("*.gvt1.com/edgedl/widevine-cdm/*.zip*", "https://redirector.gvt1.com/edgedl/widevine-cdm/4.10/win-x64.zip?cms_redirect=yes"));
  assert.ok(urlMatches("get*.opera.com/pub/opera/*/autoupdate/*.exe", "http://get.geo.opera.com/pub/opera/desktop/110/autoupdate/setup.exe"));

  const rules = { ...RULES, capture_excluded_urls: ["https://dl.net/keep/*"] };
  assert.equal(captureDecision(rules, { url: "https://dl.net/keep/a.zip" }).capture, false);
  assert.equal(captureDecision(rules, { url: "https://dl.net/take/a.zip" }).capture, true);
});

test("all links on a page go to the window's picker with cookies", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
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
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "udm-all-links", pageUrl: "https://dl.net/list" }, { id: 4 });
  const msg = w.sent.find((m) => m.op === "pick_links");
  assert.equal(msg.page, "https://dl.net/list");
  assert.equal(JSON.stringify(msg.links.map((l) => [l.url, l.cookies ?? null])), JSON.stringify([["https://dl.net/a.zip", "s=1"], ["https://cdn.org/b.png", null]]));
});

test("all links runs links.js in every frame first, and lists each link once", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: { capture_types: ["zip", "abc"], capture_min_size_kb: 0, capture_excluded_sites: [] } });
  const calls = [];
  w.chrome.scripting = {
    executeScript: async (o) => {
      calls.push(o);
      if (o.files) return [{ result: null }, { result: null }];
      // The top frame walked into a frame made from nothing, which the
      // browser also ran the script in.
      return [
        { result: [{ url: "https://dl.net/a.zip", text: "" }, { url: "ftp://files.net/b.iso", text: "B" }] },
        { result: [{ url: "https://dl.net/a.zip#top", text: "A" }] },
        { result: "not a list" },
      ];
    },
  };
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "udm-all-links", pageUrl: "https://dl.net/list" }, { id: 4 });
  assert.equal(JSON.stringify(calls.map((o) => [o.target.allFrames, o.files || null, o.args || null])), JSON.stringify([[true, ["links.js"], null], [true, null, [false, ["zip", "abc"]]]]));
  const msg = w.sent.find((m) => m.op === "pick_links");
  assert.equal(JSON.stringify(msg.links), JSON.stringify([{ url: "https://dl.net/a.zip", text: "A" }, { url: "ftp://files.net/b.iso", text: "B" }]));
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
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  let asked = null;
  w.chrome.scripting = {
    executeScript: async ({ args }) => {
      asked = args;
      return [{ result: [{ url: "https://dl.net/a.zip", text: "" }] }];
    },
  };
  const answer = await w.content({ type: "selection:links" }, 5, "https://dl.net/list");
  assert.equal(answer.ok, true);
  assert.equal(JSON.stringify(asked), "[true,[]]");
  assert.equal(w.sent.at(-1).op, "pick_links");
});

test("each browser's manifest has only the keys it understands", () => {
  const fs = require("node:fs");
  const path = require("node:path");
  const { firefoxManifest } = require("./harness.js");
  const chrome = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "chromium", "manifest.json"), "utf8"));
  // Chrome warns about background scripts in Manifest V3, and about keys it
  // does not know.
  assert.deepEqual(Object.keys(chrome.background), ["service_worker"]);
  assert.equal(chrome.browser_specific_settings, undefined);
  // Firefox runs no service worker and has no use for Chrome's key.
  const firefox = firefoxManifest();
  assert.deepEqual(Object.keys(firefox.background), ["scripts"]);
  assert.equal(firefox.background.scripts.at(-1), chrome.background.service_worker);
  assert.equal(firefox.key, undefined);
  assert.equal(firefox.browser_specific_settings.gecko.id, "udm@udm.local");
  assert.equal(firefox.version, chrome.version);
});

test("a download UDM cannot take comes with a notice saying why", async () => {
  const w = loadWorker();
  const shown = () => w.chrome.notifications.shown.map(([, o]) => o.message);
  // Not connected: the browser keeps what UDM would have taken.
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/a.zip", filename: "C:/dl/a.zip" }));
  assert.deepEqual(shown(), ["UDM is not running or not connected, so the browser downloads a.zip."]);
  assert.equal(w.chrome.notifications.shown[0][1].buttons[0].title, "Don't show again");

  // Connected, but UDM could not start it: it goes back, with the reason.
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: RULES });
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/b.zip" }));
  w.port.onMessage.fire({ op: "ack", id: 4, url: "https://dl.net/b.zip", status: "accepted" });
  w.port.onMessage.fire({ op: "failed", id: 4, url: "https://dl.net/b.zip", error: "server returned HTTP 403" });
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(shown().at(-1), "UDM could not start https://dl.net/b.zip (server returned HTTP 403), so the browser downloads it.");

  // "Don't show again" stops them for good.
  await w.chrome.notifications.onButtonClicked.fire("udm-takeover", 0);
  assert.equal(JSON.stringify(w.chrome.storage.written.at(-1)), JSON.stringify({ takeoverNotice: false }));
  w.port.onDisconnect.fire();
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/c.zip" }));
  assert.equal(shown().length, 2);
});

test("in Firefox, clicking the notice stops it", async () => {
  const w = loadWorker({ firefox: true });
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/a.zip" }));
  await new Promise((r) => setTimeout(r, 0));
  const [[id, options]] = w.chrome.notifications.shown;
  assert.ok(options.message.endsWith("Click this notice to stop showing it."), options.message);
  assert.equal(options.buttons, undefined);
  await w.chrome.notifications.onClicked.fire(id);
  assert.equal(JSON.stringify(w.chrome.storage.written.at(-1)), JSON.stringify({ takeoverNotice: false }));
});

test("the worker runs as a Firefox background script", async () => {
  const w = loadWorker({ firefox: true });
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  w.port.onMessage.fire({ op: "settings", settings: RULES });
  await w.chrome.downloads.onCreated.fire(download({ url: "https://dl.net/a.zip" }));
  assert.equal(w.sent.find((m) => m.op === "add").url, "https://dl.net/a.zip");
  // The popup, opened as a tab at moz-extension://, is still the popup.
  const state = await new Promise((resolve) =>
    w.chrome.runtime.onMessage.fire({ type: "state" }, { id: "udm", tab: { id: 9 }, url: "moz-extension://0b1c/popup.html" }, resolve),
  );
  assert.equal(state.connected, true);
});

test("a site crawl gets the browser's cookies for its start page", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  w.chrome.cookies.getAll = async () => [{ name: "session", value: "abc" }];
  w.port.onMessage.fire({ op: "cookies_wanted", request: 3, url: "https://site.net/members/" });
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(JSON.stringify(w.sent.at(-1)), JSON.stringify({ op: "cookies", request: 3, cookies: "session=abc" }));
});

test("a crawl's page is rendered in a background tab and the tab is closed", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "udm-daemon", version: "x" });
  const { event } = require("./harness.js");
  const removed = [];
  w.chrome.tabs = {
    ...w.chrome.tabs,
    onUpdated: event(),
    onCreated: event(),
    create: async ({ url, active }) => {
      assert.equal(active, false);
      setTimeout(() => {
        // The page's script opens a window, then the page finishes loading.
        w.chrome.tabs.onCreated.fire({ id: 12, openerTabId: 11, pendingUrl: "https://site.net/popup.html" });
        w.chrome.tabs.onCreated.fire({ id: 13, openerTabId: 99, pendingUrl: "https://other.net/" });
        w.chrome.tabs.onUpdated.fire(11, { status: "complete" });
      }, 0);
      return { id: 11, url };
    },
    remove: async (id) => removed.push(id),
  };
  w.chrome.scripting = {
    executeScript: async () => [{ result: { html: "<a href='/added-by-script.pdf'>", url: "https://site.net/app/" } }],
  };
  w.port.onMessage.fire({ op: "render_page", request: 5, url: "https://site.net/app" });
  const until = Date.now() + 5000;
  while (!w.sent.some((m) => m.op === "rendered") && Date.now() < until) await new Promise((r) => setTimeout(r, 50));
  const answer = w.sent.find((m) => m.op === "rendered");
  assert.equal(answer.request, 5);
  assert.equal(answer.url, "https://site.net/app/");
  assert.ok(answer.html.includes("added-by-script.pdf"));
  assert.equal(JSON.stringify(answer.popups), JSON.stringify(["https://site.net/popup.html"]));
  assert.equal(JSON.stringify(removed), "[12,11]", "the popup is closed, and the tab; another tab's window is left alone");
});
test("a form's file fields are named, and whether one sends a file", () => {
  const { formFileFields } = require("../chromium/rules.js");
  const input = (type, name, files = [], disabled = false) => ({ type, name, files, disabled });
  assert.equal(formFileFields([input("text", "q"), input("submit", "go")]), null);
  assert.equal(JSON.stringify(formFileFields([input("text", "q"), input("file", "doc"), input("FILE", "doc")])), '{"names":["doc"],"chosen":false}');
  assert.equal(formFileFields([input("file", "doc", [{}])]).chosen, true);
  assert.equal(formFileFields([input("file", "", [{}]), input("file", "x", [{}], true)]), null, "unnamed and disabled ones are not sent");
});

// A frame can get the content scripts twice (the manifest's and the worker's
// injectOpenTabs), and a second copy of a top-level `const` threw "has
// already been declared" on Gmail.
test("the content scripts can run twice in one frame", () => {
  const fs = require("node:fs");
  const path = require("node:path");
  const vm = require("node:vm");
  const dir = path.join(__dirname, "..", "chromium");
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, "manifest.json"), "utf8"));
  const files = manifest.content_scripts.flatMap((c) => c.js);
  assert.ok(files.includes("rules.js"));
  for (const file of files) {
    const source = fs.readFileSync(path.join(dir, file), "utf8");
    const top = source.split("\n").filter((line) => /^(const|let|class)\s/.test(line));
    assert.deepEqual(top, [], `${file} declares names a second copy cannot`);
  }
  // The files that need no page run twice for real.
  const world = vm.createContext({ globalThis: undefined });
  world.globalThis = world;
  for (const file of ["rules.js", "links.js"]) {
    const script = new vm.Script(fs.readFileSync(path.join(dir, file), "utf8"), { filename: file });
    script.runInContext(world);
    assert.doesNotThrow(() => script.runInContext(world), `${file} the second time`);
  }
});
