// Videos on web pages: which responses are video or audio, a list of them
// per tab, and the menu the video button shows.
//
// The service worker watches responses (webRequest) and keeps, for each tab,
// the direct video and audio files and the HLS (.m3u8) and DASH (.mpd)
// playlists its pages loaded. The segments a player fetches one by one are
// left out: UIDM fetches those itself from the playlist. These are plain
// functions: the worker loads them with importScripts, the tests with
// require. In the worker, rules.js is loaded first.

if (typeof module !== "undefined") {
  var { extensionOf, urlPath, hostOf, siteMatches } = require("./rules.js");
}

// Most media kept per tab; the oldest go first.
const MEDIA_PER_TAB = 50;
// Smaller direct files are sounds and adverts' beacons, not videos.
const MEDIA_MIN_FILE_BYTES = 128 * 1024;

const HLS_MIME = new Set([
  "application/vnd.apple.mpegurl",
  "application/x-mpegurl",
  "audio/mpegurl",
  "audio/x-mpegurl",
]);
const DASH_MIME = new Set(["application/dash+xml"]);
// Pieces of a stream rather than whole files.
const SEGMENT_MIME = new Set(["video/mp2t", "video/iso.segment", "audio/iso.segment"]);
const SEGMENT_EXTENSIONS = new Set(["ts", "m4s", "m4f", "cmfv", "cmfa", "m4v_seg"]);
const MEDIA_FILE_EXTENSIONS = {
  mp4: "video",
  m4v: "video",
  webm: "video",
  mkv: "video",
  mov: "video",
  ogv: "video",
  mp3: "audio",
  m4a: "audio",
  ogg: "audio",
  oga: "audio",
  opus: "audio",
  flac: "audio",
  wav: "audio",
};

function headerValue(headers, name) {
  const h = (headers || []).find((x) => String(x.name).toLowerCase() === name);
  return h ? String(h.value || "") : "";
}

// The whole file's size: from Content-Range on a partial response, else
// Content-Length. 0 when unknown.
function responseSize(statusCode, headers) {
  if (statusCode === 206) {
    const total = /\/(\d+)\s*$/.exec(headerValue(headers, "content-range"));
    return total ? Number(total[1]) : 0;
  }
  const length = Number(headerValue(headers, "content-length"));
  return Number.isFinite(length) && length > 0 ? length : 0;
}

// What a response is: { kind: "hls" | "dash" | "file", mime, size }, or null
// for anything that is not a video, audio file or stream playlist.
// `d` is a webRequest response: { url, statusCode, responseHeaders }.
function classifyMedia(d) {
  if (!/^https?:\/\//i.test(d.url || "") || d.statusCode >= 400) return null;
  const mime = headerValue(d.responseHeaders, "content-type").split(";")[0].trim().toLowerCase();
  const ext = extensionOf(urlPath(d.url));
  if (HLS_MIME.has(mime) || ext === "m3u8") return { kind: "hls", mime, size: 0 };
  if (DASH_MIME.has(mime) || ext === "mpd") return { kind: "dash", mime, size: 0 };
  if (SEGMENT_MIME.has(mime) || SEGMENT_EXTENSIONS.has(ext)) return null;
  const media = mime.startsWith("video/") || mime.startsWith("audio/");
  const generic = !mime || mime === "application/octet-stream" || mime === "binary/octet-stream";
  if (!media && !(generic && MEDIA_FILE_EXTENSIONS[ext])) return null;
  const size = responseSize(d.statusCode, d.responseHeaders);
  if (size > 0 && size < MEDIA_MIN_FILE_BYTES) return null;
  return { kind: "file", mime, size };
}

let nextMediaId = 1;

// Remember media `found` (from classifyMedia) at `url` for tab `tabId`.
// `store` maps tab id -> Map(url -> entry). The same URL again keeps its
// entry, learning its size if it was unknown. Returns the entry.
function recordMedia(store, tabId, url, found, now) {
  let tab = store.get(tabId);
  if (!tab) {
    tab = new Map();
    store.set(tabId, tab);
  }
  let entry = tab.get(url);
  if (entry) {
    if (!entry.size && found.size) entry.size = found.size;
    return entry;
  }
  entry = { id: nextMediaId++, url, kind: found.kind, mime: found.mime, size: found.size, time: now };
  tab.set(url, entry);
  while (tab.size > MEDIA_PER_TAB) tab.delete(tab.keys().next().value);
  return entry;
}

// A file's type for people: "MP4 video".
function fileLabel(entry) {
  const ext = extensionOf(urlPath(entry.url)) || (entry.mime.split("/")[1] || "").replace(/^x-/, "");
  const what = entry.mime.startsWith("audio/") || MEDIA_FILE_EXTENSIONS[ext] === "audio" ? "audio" : "video";
  return ext ? `${ext.toUpperCase()} ${what}` : what[0].toUpperCase() + what.slice(1);
}

// The video button's menu for a tab's `entries`, newest first. `infos` maps a
// stream's URL to what UIDM said it offers (a MediaInfo). Each item is
// { entry, label, options: [{ label, size, video, audio }], drm, error }.
// A stream that is one quality of another listed stream (an HLS variant
// playlist the player loaded after its master playlist) is left out.
function mediaMenu(entries, infos) {
  const parts = new Set();
  for (const info of infos.values()) {
    for (const o of [...(info.video || []), ...(info.audio || [])]) if (o.id) parts.add(o.id);
  }
  const items = [];
  for (const e of [...entries].sort((a, b) => b.time - a.time || b.id - a.id)) {
    if (e.kind === "file") {
      items.push({ entry: e.id, label: fileLabel(e), options: [{ label: "Download", size: e.size || null }] });
      continue;
    }
    if (parts.has(e.url)) continue;
    const info = infos.get(e.url);
    const label = e.kind === "hls" ? "HLS stream" : "DASH stream";
    if (!info) {
      items.push({ entry: e.id, label, options: [], error: "UIDM did not answer." });
      continue;
    }
    if (info.drm || info.error) {
      items.push({ entry: e.id, label, options: [], drm: !!info.drm, error: info.error || "" });
      continue;
    }
    // An audio choice only matters with several; the best video is first.
    const audio = (info.audio || []).length > 1 ? info.audio[0].id : undefined;
    const options = (info.video || []).map((o) => ({
      label: o.label,
      size: o.size || null,
      video: o.id || undefined,
      audio,
    }));
    if (options.length === 0 && (info.audio || []).length > 0) {
      options.push({ label: info.audio[0].label, size: info.audio[0].size || null, audio: info.audio[0].id });
    }
    items.push({ entry: e.id, label, options });
  }
  return items;
}

// Whether the video button shows on a page at `url` under UIDM's `rules`.
function videoButtonAllowed(rules, url) {
  if (!rules) return true;
  if (rules.video_button === false) return false;
  const host = hostOf(url);
  return !(rules.video_button_excluded_sites || []).some((site) => siteMatches(site, host));
}

// A page title made into a file name, without characters Windows forbids.
function titleFileName(title, ext) {
  const name = String(title || "")
    .replace(/[<>:"/\\|?*\u0000-\u001f]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/^\.+|\.+$/g, "")
    .slice(0, 120)
    .trim();
  return name ? `${name}.${ext}` : undefined;
}

if (typeof module !== "undefined") {
  module.exports = { classifyMedia, recordMedia, mediaMenu, videoButtonAllowed, titleFileName, responseSize, MEDIA_PER_TAB };
}
