// Tests for the video button's media list (media.js) and the worker's side
// of it. Run with: node --test extension/test/*.test.js

const test = require("node:test");
const assert = require("node:assert/strict");

const { classifyMedia, recordMedia, recordPage, videoPageUrl, pageTitle, recentMedia, mediaMenu, videoButtonAllowed, titleFileName, MEDIA_PER_TAB } = require("../media.js");
const { loadWorker } = require("./harness.js");

const same = (a, b) => assert.equal(JSON.stringify(a), JSON.stringify(b));
const response = (url, type, extra = {}) => ({
  url,
  statusCode: extra.status || 200,
  responseHeaders: [
    { name: "Content-Type", value: type },
    ...Object.entries(extra.headers || {}).map(([name, value]) => ({ name, value })),
  ],
});

test("responses are sorted into streams, files and the rest", () => {
  assert.equal(classifyMedia(response("https://v.net/master.m3u8?t=1", "application/vnd.apple.mpegurl")).kind, "hls");
  assert.equal(classifyMedia(response("https://v.net/a/playlist", "application/x-mpegURL")).kind, "hls");
  assert.equal(classifyMedia(response("https://v.net/manifest.mpd", "text/plain")).kind, "dash");
  assert.equal(classifyMedia(response("https://v.net/x", "application/dash+xml; charset=utf-8")).kind, "dash");
  const file = classifyMedia(response("https://v.net/movie.mp4", "video/mp4", { headers: { "Content-Length": "5000000" } }));
  same([file.kind, file.size], ["file", 5000000]);
  // A player reading a file in ranges reports the whole size.
  const ranged = classifyMedia(
    response("https://v.net/movie.webm", "video/webm", { status: 206, headers: { "Content-Range": "bytes 0-999/7340032" } }),
  );
  assert.equal(ranged.size, 7340032);
  assert.equal(classifyMedia(response("https://v.net/song.mp3", "application/octet-stream")).kind, "file");

  // Segments, small files, errors and everything else are not listed.
  assert.equal(classifyMedia(response("https://v.net/seg1.ts", "video/mp2t")), null);
  assert.equal(classifyMedia(response("https://v.net/chunk-5.m4s", "video/mp4")), null);
  assert.equal(classifyMedia(response("https://v.net/beep.mp4", "video/mp4", { headers: { "Content-Length": "4000" } })), null);
  assert.equal(classifyMedia(response("https://v.net/movie.mp4", "video/mp4", { status: 404 })), null);
  assert.equal(classifyMedia(response("https://v.net/page", "text/html")), null);
  assert.equal(classifyMedia(response("blob:https://v.net/1", "video/mp4")), null);
});

test("each tab keeps a bounded list, one entry per URL", () => {
  const store = new Map();
  const a = recordMedia(store, 1, "https://v.net/a.mp4", { kind: "file", mime: "video/mp4", size: 0 }, 1);
  const again = recordMedia(store, 1, "https://v.net/a.mp4", { kind: "file", mime: "video/mp4", size: 900000 }, 2);
  assert.equal(a.id, again.id);
  assert.equal(again.size, 900000, "a later response teaches the size");
  for (let i = 0; i < MEDIA_PER_TAB + 5; i++) {
    recordMedia(store, 2, `https://v.net/${i}.mp4`, { kind: "file", mime: "video/mp4", size: 0 }, i);
  }
  assert.equal(store.get(2).size, MEDIA_PER_TAB);
  assert.ok(!store.get(2).has("https://v.net/0.mp4"), "the oldest go first");
  assert.equal(store.get(1).size, 1);
});

test("the menu lists qualities and hides a master's own playlists", () => {
  const entries = [
    { id: 1, url: "https://v.net/master.m3u8", kind: "hls", mime: "", size: 0, time: 1 },
    { id: 2, url: "https://v.net/720/index.m3u8", kind: "hls", mime: "", size: 0, time: 2 },
    { id: 3, url: "https://v.net/clip.mp4", kind: "file", mime: "video/mp4", size: 3000000, time: 3 },
    { id: 4, url: "https://drm.net/m.mpd", kind: "dash", mime: "", size: 0, time: 0 },
  ];
  const infos = new Map([
    [
      "https://v.net/master.m3u8",
      {
        kind: "hls",
        video: [
          { id: "https://v.net/720/index.m3u8", label: "720p, 3.0 Mbps", size: 7500000 },
          { id: "https://v.net/360/index.m3u8", label: "360p, 900 kbps", size: 2250000 },
        ],
        audio: [],
        drm: false,
      },
    ],
    ["https://v.net/720/index.m3u8", { kind: "hls", video: [{ id: "", label: "Default" }], audio: [] }],
    ["https://drm.net/m.mpd", { kind: "dash", video: [], audio: [], drm: true, error: "protected" }],
  ]);
  const menu = mediaMenu(entries, infos);
  same(
    menu.map((i) => [i.entry, i.label]),
    [
      [3, "MP4 video"],
      [1, "HLS stream"],
      [4, "DASH stream"],
    ],
  );
  same(menu[0].options, [{ label: "Download", size: 3000000 }]);
  same(
    menu[1].options.filter((o) => !o.audioOnly).map((o) => [o.label, o.video]),
    [
      ["720p, 3.0 Mbps", "https://v.net/720/index.m3u8"],
      ["360p, 900 kbps", "https://v.net/360/index.m3u8"],
    ],
  );
  assert.equal(menu[2].drm, true);
  assert.equal(menu[2].options.length, 0, "nothing to choose for a protected stream");
});

test("the button can be off everywhere or on some sites", () => {
  assert.equal(videoButtonAllowed(null, "https://a.com/"), true);
  assert.equal(videoButtonAllowed({ video_button: true, video_button_excluded_sites: ["*.tube.net"] }, "https://www.tube.net/v"), false);
  assert.equal(videoButtonAllowed({ video_button: true, video_button_excluded_sites: ["tube.net"] }, "https://other.net/v"), true);
  assert.equal(videoButtonAllowed({ video_button: false }, "https://other.net/v"), false);
});

test("page titles become safe file names", () => {
  assert.equal(titleFileName('My "great" video: part 1/2', "mp4"), "My great video part 1 2.mp4");
  assert.equal(titleFileName("  ...  ", "mp4"), undefined);
  assert.equal(titleFileName("x".repeat(300), "mp4").length, 124);
});

function connected(settings = {}) {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  w.port.onMessage.fire({
    op: "settings",
    settings: { capture_types: [], capture_min_size_kb: 0, capture_excluded_sites: [], video_button: true, video_button_excluded_sites: [], ...settings },
  });
  return w;
}

const tick = () => new Promise((r) => setTimeout(r, 0));

const seen = (w, tabId, url, type, headers) =>
  w.chrome.webRequest.onHeadersReceived.fire({ tabId, type: "xmlhttprequest", ...response(url, type, { headers }) });

test("the settings choose the video types and the smallest file", () => {
  const rules = { video_types: ["mp4", "m3u8"], video_min_size_kb: 1024 };
  const mp4 = (size) => response("https://v.net/movie.mp4", "video/mp4", { headers: { "Content-Length": String(size) } });
  assert.equal(classifyMedia(mp4(2 << 20), rules).kind, "file");
  assert.equal(classifyMedia(mp4(500 << 10), rules), null, "under the minimum");
  assert.equal(classifyMedia(mp4(500 << 10)).kind, "file", "the default minimum is 128 KB");
  assert.equal(classifyMedia(response("https://v.net/a.webm", "video/webm"), rules), null, "not a listed type");
  assert.equal(classifyMedia(response("https://v.net/clip", "video/webm"), rules), null, "the type from the MIME type");
  assert.equal(classifyMedia(response("https://v.net/clip", "video/x-unknown"), rules).kind, "file", "an unknown type is kept");
  assert.equal(classifyMedia(response("https://v.net/master.m3u8", "application/vnd.apple.mpegurl"), rules).kind, "hls");
  assert.equal(classifyMedia(response("https://v.net/a.mpd", "application/dash+xml"), rules), null, "DASH is left out");
  assert.equal(classifyMedia(response("https://v.net/a.mpd", "application/dash+xml"), { video_types: [] }), null);
});

test("the newest media across tabs come first", () => {
  const store = new Map();
  recordMedia(store, 1, "https://a.net/1.mp4", { kind: "file", mime: "video/mp4", size: 0 }, 100);
  recordMedia(store, 2, "https://b.net/2.m3u8", { kind: "hls", mime: "", size: 0 }, 300);
  recordMedia(store, 1, "https://a.net/3.mp4", { kind: "file", mime: "video/mp4", size: 0 }, 200);
  same(recentMedia(store, 2).map((r) => [r.tabId, r.entry.url]), [[2, "https://b.net/2.m3u8"], [1, "https://a.net/3.mp4"]]);
  assert.equal(recentMedia(new Map(), 10).length, 0);
});

test("the context menu has the items the settings keep, and the last videos", async () => {
  const w = connected();
  const items = () => [...w.chrome.contextMenus.items.keys()];
  w.port.onMessage.fire({ op: "settings", settings: { browser_menu_hidden: ["all_links", "download_later"] } });
  for (const gone of ["uidm-all-links", "uidm-later-link", "uidm-later-media"]) assert.ok(!items().includes(gone), gone);
  for (const kept of ["uidm-link", "uidm-media", "uidm-selection-links", "uidm-last-video", "uidm-recent"]) {
    assert.ok(items().includes(kept), kept);
  }
  await new Promise((r) => setTimeout(r, 350));
  assert.equal(w.chrome.contextMenus.items.get("uidm-last-video").enabled, false, "no video yet");

  await seen(w, 7, "https://v.net/old.mp4", "video/mp4", { "Content-Length": "9000000" });
  await new Promise((r) => setTimeout(r, 5));
  await seen(w, 8, "https://v.net/new.mp4", "video/mp4", { "Content-Length": "9000000" });
  await new Promise((r) => setTimeout(r, 350));
  assert.equal(w.chrome.contextMenus.items.get("uidm-last-video").enabled, true);
  const children = [...w.chrome.contextMenus.items.values()].filter((i) => i.parentId === "uidm-recent");
  same(children.map((c) => c.title), ["v.net (MP4 video)", "v.net (MP4 video)"]);

  await w.chrome.contextMenus.onClicked.fire({ menuItemId: "uidm-last-video" });
  assert.equal(w.sent.filter((m) => m.op === "add").at(-1).url, "https://v.net/new.mp4");
  await w.chrome.contextMenus.onClicked.fire({ menuItemId: children[1].id });
  assert.equal(w.sent.filter((m) => m.op === "add").at(-1).url, "https://v.net/old.mp4");

  // Showing everything again brings the items back.
  w.port.onMessage.fire({ op: "settings", settings: { browser_menu_hidden: [] } });
  assert.ok(items().includes("uidm-all-links"));
});

test("a tab's video goes to UIDM in the quality picked", async () => {
  const w = connected();
  await seen(w, 7, "https://v.net/master.m3u8", "application/vnd.apple.mpegurl");
  await seen(w, 8, "https://other.net/elsewhere.mp4", "video/mp4", { "Content-Length": "9000000" });

  const asked = w.content({ type: "video:menu" }, 7);
  await tick();
  // The worker asks UIDM what the stream offers.
  const probe = w.sent.find((m) => m.op === "probe_media");
  assert.equal(probe.download.url, "https://v.net/master.m3u8");
  assert.equal(probe.download.referrer, "https://site.net/watch");
  w.port.onMessage.fire({
    op: "media_info",
    request: probe.request,
    info: { kind: "hls", video: [{ id: "https://v.net/1080.m3u8", label: "1080p", size: 9 }], audio: [] },
  });
  const menu = await asked;
  assert.equal(menu.items.length, 1, "only this tab's media");
  const item = menu.items[0];
  assert.equal(item.options[0].video, "https://v.net/1080.m3u8");

  // A second menu uses the answer it has.
  await w.content({ type: "video:menu" }, 7);
  assert.equal(w.sent.filter((m) => m.op === "probe_media").length, 1);

  const ok = await w.content({ type: "video:download", entry: item.entry, video: item.options[0].video, title: "Cats: the movie" }, 7);
  assert.equal(ok.ok, true);
  const add = w.sent.find((m) => m.op === "add");
  same(
    [add.url, add.filename, add.media, add.referrer],
    ["https://v.net/master.m3u8", "Cats the movie.mp4", { kind: "hls", video: "https://v.net/1080.m3u8" }, "https://site.net/watch"],
  );

  // Another tab cannot pick this tab's entry.
  const denied = await w.content({ type: "video:download", entry: item.entry, title: "x" }, 8);
  assert.equal(denied.ok, false);
  // A new page in the tab starts its list over.
  await w.chrome.webRequest.onBeforeRequest.fire({ tabId: 7, type: "main_frame", method: "GET", url: "https://site.net/next" });
  assert.equal((await w.content({ type: "video:menu" }, 7)).items.length, 0);
});

test("YouTube pages are one address per video", () => {
  const watch = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
  assert.equal(videoPageUrl(watch), watch);
  assert.equal(videoPageUrl("https://m.youtube.com/watch?v=jNQXAC9IVRw&list=PL1&t=4"), watch);
  assert.equal(videoPageUrl("https://www.youtube.com/shorts/jNQXAC9IVRw"), watch);
  assert.equal(videoPageUrl("https://youtu.be/jNQXAC9IVRw?si=x"), watch);
  for (const url of ["https://www.youtube.com/", "https://www.youtube.com/@jawed", "https://notyoutube.com/watch?v=x", "nonsense", "https://www.youtube.com/watch?v=a%22b"]) {
    assert.equal(videoPageUrl(url), null, url);
  }
  assert.equal(pageTitle("(3) Me at the zoo - YouTube"), "Me at the zoo");
  assert.equal(pageTitle("A - B - YouTube"), "A - B");

  // Moving to another video replaces the tab's page, not its other media.
  const store = new Map();
  recordMedia(store, 1, "https://v.net/a.mp4", { kind: "file", mime: "video/mp4", size: 0 }, 1);
  recordPage(store, 1, watch, 2);
  recordPage(store, 1, "https://www.youtube.com/watch?v=other", 3);
  same([...store.get(1).keys()], ["https://v.net/a.mp4", "https://www.youtube.com/watch?v=other"]);
});

test("a YouTube video goes to UIDM as a page yt-dlp reads", async () => {
  const w = connected();
  const page = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
  const asked = w.content({ type: "video:menu" }, 7, "https://www.youtube.com/watch?v=jNQXAC9IVRw&t=3");
  await tick();
  const probe = w.sent.find((m) => m.op === "probe_media");
  assert.equal(probe.download.url, page);
  w.port.onMessage.fire({
    op: "media_info",
    request: probe.request,
    info: { kind: "page", video: [{ id: "137", label: "1080p", size: 9 }], audio: [{ id: "140", label: "en" }] },
  });
  const menu = await asked;
  same(menu.items.map((i) => i.label), ["YouTube video"]);
  const item = menu.items[0];
  same(item.options.map((o) => o.label), ["1080p", "Sound only (M4A)", "Sound only (MP3)"]);

  await w.content({ type: "video:download", entry: item.entry, video: "137", title: "(2) Me at the zoo - YouTube" }, 7);
  const add = w.sent.find((m) => m.op === "add");
  same([add.url, add.filename, add.media], [page, "Me at the zoo.mp4", { kind: "page", video: "137" }]);

  // Another video in the same tab, without a page load, replaces it.
  await w.chrome.tabs.onUpdated.fire(7, { url: "https://www.youtube.com/shorts/abc" }, { incognito: false });
  const next = w.content({ type: "video:menu" }, 7, "https://www.youtube.com/shorts/abc");
  await tick();
  const second = w.sent.filter((m) => m.op === "probe_media").at(-1);
  assert.equal(second.download.url, "https://www.youtube.com/watch?v=abc");
  w.port.onMessage.fire({ op: "media_info", request: second.request, info: { kind: "page", video: [{ id: "18", label: "360p" }], audio: [] } });
  assert.equal((await next).items.length, 1);
});

test("content scripts cannot use the popup's commands", async () => {
  const w = connected();
  const answer = await Promise.race([
    w.content({ type: "add", url: "https://evil.net/x.exe" }, 3),
    new Promise((r) => setTimeout(() => r("no answer"), 50)),
  ]);
  assert.equal(answer, "no answer");
  assert.equal(w.sent.filter((m) => m.op === "add").length, 0);
});

test("the button follows the settings, and the popup changes them", async () => {
  const w = connected({ video_button_excluded_sites: ["*.quiet.net"] });
  assert.equal((await w.content({ type: "video:enabled" }, 1, "https://www.quiet.net/v")).enabled, false);
  assert.equal((await w.content({ type: "video:enabled" }, 1, "https://loud.net/v")).enabled, true);

  await w.popup({ type: "setVideoButton", site: "loud.net", siteOn: false });
  let set = w.sent.filter((m) => m.op === "set_settings").at(-1);
  same(set.settings.video_button_excluded_sites, ["*.quiet.net", "loud.net"]);
  await w.popup({ type: "setVideoButton", site: "www.quiet.net", siteOn: true });
  set = w.sent.filter((m) => m.op === "set_settings").at(-1);
  same(set.settings.video_button_excluded_sites, []);
  await w.popup({ type: "setVideoButton", on: false });
  set = w.sent.filter((m) => m.op === "set_settings").at(-1);
  assert.equal(set.settings.video_button, false);
});

test("a stream offers its sound alone, from its audio track or its smallest video", () => {
  const entries = [
    { id: 1, url: "https://v.net/a.mpd", kind: "dash", time: 2 },
    { id: 2, url: "https://v.net/b.m3u8", kind: "hls", time: 1 },
  ];
  const infos = new Map([
    ["https://v.net/a.mpd", { video: [{ id: "v1", label: "1080p" }], audio: [{ id: "a1", label: "English", size: 900 }] }],
    ["https://v.net/b.m3u8", { video: [{ id: "big", label: "720p" }, { id: "small", label: "240p" }], audio: [] }],
  ]);
  const menu = mediaMenu(entries, infos);
  const sound = (item) => item.options.filter((o) => o.audioOnly);
  same(sound(menu[0]).map((o) => [o.label, o.audio, o.video, o.size]), [["Sound only (M4A)", "a1", undefined, 900], ["Sound only (MP3)", "a1", undefined, 900]]);
  same(sound(menu[1]).map((o) => [o.audioOnly, o.video]), [["m4a", "small"], ["mp3", "small"]]);
});

test("choosing the sound alone sends an audio_only stream", async () => {
  const w = loadWorker();
  w.port.onMessage.fire({ op: "hello", name: "uidm-daemon", version: "x" });
  w.chrome.webRequest.onHeadersReceived.fire({
    tabId: 7,
    url: "https://v.net/master.m3u8",
    type: "xmlhttprequest",
    responseHeaders: [{ name: "Content-Type", value: "application/vnd.apple.mpegurl" }],
  });
  const menu = await new Promise((resolve) => {
    const asked = w.content({ type: "video:menu" }, 7);
    setTimeout(() => {
      const probe = w.sent.find((m) => m.op === "probe_media");
      w.port.onMessage.fire({ op: "media_info", request: probe.request, info: { kind: "hls", video: [{ id: "v", label: "720p" }], audio: [] } });
    }, 0);
    asked.then(resolve);
  });
  const item = menu.items[0];
  const mp3 = item.options.find((o) => o.audioOnly === "mp3");
  await w.content({ type: "video:download", entry: item.entry, video: mp3.video, audioOnly: "mp3", title: "Talk" }, 7);
  const add = w.sent.find((m) => m.op === "add");
  assert.equal(add.filename, "Talk.mp3");
  assert.equal(add.media.audio_only, "mp3");
});