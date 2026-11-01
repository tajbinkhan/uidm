// Videos on web pages: which responses are video or audio, a list of them
// per tab, and the menu the video button shows.
//
// The service worker watches responses (webRequest) and keeps, for each tab,
// the direct video and audio files and the HLS (.m3u8) and DASH (.mpd)
// playlists its pages loaded, and the YouTube video it shows. YouTube's
// player streams in POST requests that are none of those, so its page is
// kept instead, and UIDM reads the page with yt-dlp. The segments a player fetches one by one are
// left out: UIDM fetches those itself from the playlist. These are plain
// functions: the worker loads them with importScripts, the tests with
// require. In the worker, rules.js is loaded first.

if (typeof module !== "undefined") {
  var { mimeExtension, extensionOf, urlPath, hostOf, siteMatches } = require("./rules.js");
}

// The menu's text in the browser's language. The tests load this file with
// no browser around it, and get the English from _locales instead: each
// $NAME$ is its placeholder's $1..$9 from `subs`.
function uiText(name, subs) {
  if (typeof chrome !== "undefined" && chrome.i18n) return chrome.i18n.getMessage(name, subs);
  const m = require("./_locales/en/messages.json")[name];
  if (!m) return "";
  const args = [].concat(subs ?? []);
  const fill = (p) => m.placeholders[p.toLowerCase()].content.replace(/\$(\d)/g, (_, i) => args[i - 1]);
  return m.message.replace(/\$(\w+)\$/g, (_, p) => fill(p));
}

// Most media kept per tab; the oldest go first.
const MEDIA_PER_TAB = 50;
// Smaller direct files are sounds and adverts' beacons, not videos. UIDM's
// `video_min_size_kb` overrides it.
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
  avi: "video",
  wmv: "video",
  flv: "video",
  "3gp": "video",
  aac: "audio",
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
// `rules` is UIDM's settings, or null before they arrive: their
// `video_types` (with `m3u8` and `mpd` for streams) and `video_min_size_kb`
// narrow what counts.
function classifyMedia(d, rules) {
  if (!/^https?:\/\//i.test(d.url || "") || d.statusCode >= 400) return null;
  const mime = headerValue(d.responseHeaders, "content-type").split(";")[0].trim().toLowerCase();
  const ext = extensionOf(urlPath(d.url));
  const types = rules && Array.isArray(rules.video_types) ? rules.video_types : null;
  const offered = (type) => !types || !type || types.includes(type);
  if (HLS_MIME.has(mime) || ext === "m3u8") return offered("m3u8") ? { kind: "hls", mime, size: 0 } : null;
  if (DASH_MIME.has(mime) || ext === "mpd") return offered("mpd") ? { kind: "dash", mime, size: 0 } : null;
  if (SEGMENT_MIME.has(mime) || SEGMENT_EXTENSIONS.has(ext)) return null;
  const media = mime.startsWith("video/") || mime.startsWith("audio/");
  const generic = !mime || mime === "application/octet-stream" || mime === "binary/octet-stream";
  if (!media && !(generic && MEDIA_FILE_EXTENSIONS[ext])) return null;
  // A type the list leaves out is not offered; one nothing names is.
  if (!offered(MEDIA_FILE_EXTENSIONS[ext] ? ext : mimeExtension(mime))) return null;
  const size = responseSize(d.statusCode, d.responseHeaders);
  const kb = rules && Number.isFinite(rules.video_min_size_kb) ? rules.video_min_size_kb : null;
  const minBytes = kb === null ? MEDIA_MIN_FILE_BYTES : kb * 1024;
  if (size > 0 && size < minBytes) return null;
  return { kind: "file", mime, size };
}

// The YouTube video page at `url` as one address per video
// (https://www.youtube.com/watch?v=ID), or null for any other page. Shorts
// and youtu.be links are the same videos.
function videoPageUrl(url) {
  let u;
  try {
    u = new URL(url);
  } catch (_) {
    return null;
  }
  if (!/^https?:$/.test(u.protocol)) return null;
  const host = u.hostname.toLowerCase();
  let id = null;
  if (host === "youtu.be") id = u.pathname.slice(1).split("/")[0];
  else if (["youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com"].includes(host)) {
    if (u.pathname === "/watch") id = u.searchParams.get("v");
    else id = (/^\/(?:shorts|live)\/([^/]+)/.exec(u.pathname) || [])[1];
  }
  return id && /^[\w-]+$/.test(id) ? `https://www.youtube.com/watch?v=${id}` : null;
}

// A YouTube page's title without the count of new notifications YouTube
// puts in front and the " - YouTube" after.
function pageTitle(title) {
  return String(title || "")
    .replace(/^\(\d+\+?\)\s*/, "")
    .replace(/\s+-\s+YouTube$/, "");
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
// The extension a saved media file gets: the address's when it is a media
// type, else the one the server's type names, else mp4 or mp3. A server can
// send "clip.exe" as video/mp4; the saved file must not end in .exe.
function mediaFileExtension(entry) {
  const fromUrl = extensionOf(urlPath(entry.url));
  if (MEDIA_FILE_EXTENSIONS[fromUrl]) return fromUrl;
  const fromMime = mimeExtension(entry.mime);
  if (MEDIA_FILE_EXTENSIONS[fromMime]) return fromMime;
  return (entry.mime || "").startsWith("audio/") ? "mp3" : "mp4";
}

function fileLabel(entry) {
  const ext = mediaFileExtension(entry);
  const audio = entry.mime.startsWith("audio/") || MEDIA_FILE_EXTENSIONS[ext] === "audio";
  if (!ext) return audio ? uiText("audioFile") : uiText("videoFile");
  return audio ? uiText("audioFileType", ext.toUpperCase()) : uiText("videoFileType", ext.toUpperCase());
}

// Remember that tab `tabId` shows the video page `url` (from videoPageUrl).
// YouTube moves between videos without loading a new page, so the tab's
// earlier video page goes.
function recordPage(store, tabId, url, now) {
  const tab = store.get(tabId);
  if (tab) for (const [key, e] of tab) if (e.kind === "page" && key !== url) tab.delete(key);
  return recordMedia(store, tabId, url, { kind: "page", mime: "", size: 0 }, now);
}

// The `n` newest media entries across `store` (tab id -> Map(url ->
// entry)), newest first, as { tabId, entry }: what the "last video" and
// "last 10 videos" menu items offer.
function recentMedia(store, n) {
  const all = [];
  for (const [tabId, tab] of store) for (const entry of tab.values()) all.push({ tabId, entry });
  all.sort((a, b) => b.entry.time - a.entry.time || b.entry.id - a.entry.id);
  return all.slice(0, n);
}

// A stream's or page's kind for people: "HLS stream", "YouTube video".
function streamLabel(kind) {
  if (kind === "page") return uiText("youtubeVideo");
  return kind === "hls" ? uiText("hlsStream") : uiText("dashStream");
}

// A media entry's kind for people: "MP4 video", "HLS stream".
function mediaLabel(entry) {
  return entry.kind === "file" ? fileLabel(entry) : streamLabel(entry.kind);
}

// The video button's menu for a tab's `entries`, newest first. `infos` maps a
// stream's (or video page's) URL to what UIDM said it offers (a MediaInfo). Each item is
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
      items.push({ entry: e.id, label: fileLabel(e), options: [{ label: uiText("download"), size: e.size || null }] });
      continue;
    }
    if (parts.has(e.url)) continue;
    const info = infos.get(e.url);
    const label = streamLabel(e.kind);
    if (!info) {
      items.push({ entry: e.id, label, options: [], error: uiText("mediaNoAnswer") });
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
    // The sound alone (UIDM needs ffmpeg for it): from the stream's audio
    // track when it has one, else from its smallest video.
    const sound = (info.audio || [])[0];
    const smallest = (info.video || []).at(-1);
    if (sound || smallest) {
      for (const format of ["m4a", "mp3"]) {
        options.push({
          label: uiText("soundOnly", format.toUpperCase()),
          size: sound ? sound.size || null : null,
          video: sound ? undefined : (smallest && smallest.id) || undefined,
          audio: sound ? sound.id : undefined,
          audioOnly: format,
        });
      }
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
  module.exports = { mediaFileExtension, classifyMedia, recordMedia, recordPage, videoPageUrl, pageTitle, recentMedia, mediaLabel, mediaMenu, videoButtonAllowed, titleFileName, responseSize, uiText, MEDIA_PER_TAB };
}
