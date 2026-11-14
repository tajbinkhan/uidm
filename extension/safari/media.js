// Videos on web pages: which responses are video or audio, a list of them
// per tab, and the menu the video button shows.
//
// The service worker watches responses (webRequest) and keeps, for each tab,
// the direct video and audio files, the HLS (.m3u8), DASH (.mpd) and HDS
// (.f4m) playlists its pages loaded, and the YouTube, Facebook, Instagram or
// Vimeo video it shows. Those sites' players fetch pieces that are none of
// those, so their page is kept instead, and UDM reads the page with
// yt-dlp. On any other page where the browser saw no stream, the menu asks
// UDM to try the page with yt-dlp (otherVideoPage), and keeps it only
// when yt-dlp finds a video or a playlist there. The segments a player fetches one by one are
// left out: UDM fetches those itself from the playlist. These are plain
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
// Smaller direct files are sounds and adverts' beacons, not videos. UDM's
// `video_min_size_kb` overrides it.
const MEDIA_MIN_FILE_BYTES = 128 * 1024;

const HLS_MIME = new Set([
  "application/vnd.apple.mpegurl",
  "application/x-mpegurl",
  "audio/mpegurl",
  "audio/x-mpegurl",
]);
const DASH_MIME = new Set(["application/dash+xml"]);
// Adobe HDS manifests (.f4m), whose fragments are F4F files.
const HDS_MIME = new Set(["application/f4m+xml", "application/f4m", "video/f4m"]);
// Pieces of a stream rather than whole files. HDS fragments have no
// extension ("...Seg1-Frag3"), only their type.
const SEGMENT_MIME = new Set(["video/mp2t", "video/iso.segment", "audio/iso.segment", "video/f4f"]);
const SEGMENT_EXTENSIONS = new Set(["ts", "m4s", "m4f", "cmfv", "cmfa", "m4v_seg", "f4f"]);
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
  // Only for a whole file UDM found under a playlist's name: .ts segments
  // stay out (SEGMENT_EXTENSIONS).
  ts: "video",
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

// What a response is: { kind: "hls" | "dash" | "hds" | "file", mime, size },
// or null for anything that is not a video, audio file or stream playlist.
// `d` is a webRequest response: { url, statusCode, responseHeaders }.
// `rules` is UDM's settings, or null before they arrive: their
// `video_types` (with `m3u8`, `mpd` and `f4m` for streams) and `video_min_size_kb`
// narrow what counts.
function classifyMedia(d, rules) {
  if (!/^https?:\/\//i.test(d.url || "") || d.statusCode >= 400) return null;
  const mime = headerValue(d.responseHeaders, "content-type").split(";")[0].trim().toLowerCase();
  const ext = extensionOf(urlPath(d.url));
  const types = rules && Array.isArray(rules.video_types) ? rules.video_types : null;
  const offered = (type) => !types || !type || types.includes(type);
  if (HLS_MIME.has(mime) || ext === "m3u8") return offered("m3u8") ? { kind: "hls", mime, size: 0 } : null;
  if (DASH_MIME.has(mime) || ext === "mpd") return offered("mpd") ? { kind: "dash", mime, size: 0 } : null;
  if (HDS_MIME.has(mime) || ext === "f4m") return offered("f4m") ? { kind: "hds", mime, size: 0 } : null;
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

// Whether the response `d` is media a web player loaded that UDM takes at
// once, without the video button: UDM's `video_auto_capture` is on, and it
// is a whole file (not an HLS or DASH stream, nor a stream's segment) of
// one of `video_auto_types`, by its address or else its MIME type, that a
// page's <video> or <audio> element asked for (`d.type` "media"; players
// that feed the element pieces through scripts are left alone), from a
// site whose downloads are not excluded from capture. `taken` is the set
// of addresses this tab already handed over: each goes once.
function autoCaptureMedia(d, rules, taken) {
  if (!rules || !rules.video_auto_capture) return false;
  const types = Array.isArray(rules.video_auto_types) ? rules.video_auto_types : [];
  if (types.length === 0 || (d.type && d.type !== "media")) return false;
  if (taken && taken.has(d.url)) return false;
  const found = classifyMedia(d, { ...rules, video_types: types });
  if (!found || found.kind !== "file") return false;
  const ext = extensionOf(urlPath(d.url));
  const type = MEDIA_FILE_EXTENSIONS[ext] ? ext : mimeExtension(found.mime);
  if (!types.includes(type)) return false;
  const from = d.documentUrl || d.initiator || "";
  return !(rules.capture_excluded_sites || []).some(
    (site) => siteMatches(site, hostOf(d.url)) || (from && siteMatches(site, hostOf(from))),
  );
}

// The sites whose video pages UDM reads with yt-dlp, because their
// players name no file or playlist the browser could catch (YouTube's
// SABR, Facebook's and Instagram's signed pieces, Vimeo's player config).
// Each `match` takes a parsed address and gives the one address the video
// is known by (so the same video under two addresses is one entry), or
// null. `label` is the menu's heading for it.
const VIDEO_SITES = [
  {
    site: "youtube",
    hosts: ["youtu.be", "youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com"],
    label: () => uiText("youtubeVideo"),
    match(u) {
      let id = null;
      if (u.hostname === "youtu.be") id = u.pathname.slice(1).split("/")[0];
      else if (u.pathname === "/watch") id = u.searchParams.get("v");
      else id = (/^\/(?:shorts|live)\/([^/]+)/.exec(u.pathname) || [])[1];
      return id && /^[\w-]+$/.test(id) ? `https://www.youtube.com/watch?v=${id}` : null;
    },
  },
  {
    site: "facebook",
    hosts: ["facebook.com", "www.facebook.com", "m.facebook.com", "web.facebook.com", "fb.watch"],
    label: () => uiText("facebookVideo"),
    match(u) {
      if (u.hostname === "fb.watch") {
        const code = u.pathname.slice(1).split("/")[0];
        return /^[\w-]+$/.test(code) ? `https://fb.watch/${code}/` : null;
      }
      const id = (u.pathname.replace(/\/$/, "") === "/watch" && u.searchParams.get("v")) ||
        (/^\/[^/]+\/videos\/(?:[^/]+\/)?(\d+)/.exec(u.pathname) || [])[1];
      if (id && /^\d+$/.test(id)) return `https://www.facebook.com/watch/?v=${id}`;
      const reel = (/^\/reel\/(\d+)/.exec(u.pathname) || [])[1];
      return reel ? `https://www.facebook.com/reel/${reel}` : null;
    },
  },
  {
    site: "instagram",
    hosts: ["instagram.com", "www.instagram.com"],
    label: () => uiText("instagramVideo"),
    match(u) {
      const m = /^\/(p|reels?|tv)\/([\w-]+)/.exec(u.pathname);
      if (!m) return null;
      // A reel is the same post under /reel/ and /reels/.
      const kind = m[1] === "reels" ? "reel" : m[1];
      return `https://www.instagram.com/${kind}/${m[2]}/`;
    },
  },
  {
    site: "vimeo",
    hosts: ["vimeo.com", "www.vimeo.com", "player.vimeo.com"],
    label: () => uiText("vimeoVideo"),
    match(u) {
      if (u.hostname === "player.vimeo.com") {
        const id = (/^\/video\/(\d+)\/?$/.exec(u.pathname) || [])[1];
        if (!id) return null;
        // An unlisted video's player needs its hash.
        const h = u.searchParams.get("h");
        return `https://player.vimeo.com/video/${id}${h && /^\w+$/.test(h) ? `?h=${h}` : ""}`;
      }
      const m = /^\/(?:channels\/[\w-]+\/)?(\d+)(?:\/(\w+))?\/?$/.exec(u.pathname);
      if (!m) return null;
      // vimeo.com/<id>/<hash> is an unlisted video, which needs the hash.
      return `https://vimeo.com/${m[1]}${m[2] && /^[0-9a-f]+$/i.test(m[2]) ? `/${m[2]}` : ""}`;
    },
  },
];

// The site (an entry of VIDEO_SITES) whose video page `url` is, and the
// one address of its video, or null for any other page.
function videoPage(url) {
  let u;
  try {
    u = new URL(url);
  } catch (_) {
    return null;
  }
  if (!/^https?:$/.test(u.protocol)) return null;
  const host = u.hostname.toLowerCase();
  const site = VIDEO_SITES.find((s) => s.hosts.includes(host));
  const page = site && site.match(u);
  return page ? { site: site.site, url: page } : null;
}

// The video page at `url` as one address per video (a YouTube watch page,
// a Facebook, Instagram or Vimeo video), or null for any other page.
// Shorts and youtu.be links are the same videos as watch pages.
function videoPageUrl(url) {
  const page = videoPage(url);
  return page ? page.url : null;
}

// A YouTube page's title without the count of new notifications YouTube
// puts in front and the " - YouTube" after.
function pageTitle(title) {
  return String(title || "")
    .replace(/^\(\d+\+?\)\s*/, "")
    .replace(/\s+-\s+YouTube$/, "");
}

// The label of a host's registrable domain, which sites put after their
// pages' titles: "youtube" for www.youtube.com, "bbc" for www.bbc.co.uk.
// Without the public suffix list, a two-letter country code under a short
// generic second level (co.uk, com.au, ne.jp) counts as one suffix.
function siteLabel(host) {
  const labels = String(host || "").toLowerCase().replace(/\.$/, "").split(".").filter(Boolean);
  if (labels.length < 2) return labels[0] || "";
  const generic = /^(co|com|net|org|gov|edu|ac|ne|or|go)$/;
  const twoPart = labels.length >= 3 && labels.at(-1).length === 2 && generic.test(labels.at(-2));
  return labels.at(twoPart ? -3 : -2);
}

const titleWords = (s) => String(s || "").toLowerCase().match(/[\p{L}\p{N}]+/gu) || [];

// `title` without a trailing " - Site", " | Site", " — Site" (or "–", "·",
// "•") naming the site at `host`: its words run together are the domain's
// label ("The Verge" on theverge.com), or the first of them is ("BBC News"
// on bbc.co.uk), or they are the page's og:site_name `siteName`. A dash in
// the middle of a title is kept: only the last part can be the site's name,
// and only when it is.
function stripSiteName(title, host, siteName) {
  const text = String(title || "").trim();
  const m = /^(.*\S)\s+[-|\u2013\u2014\u00b7\u2022]\s+([^-|\u2013\u2014\u00b7\u2022]+)$/.exec(text);
  if (!m) return text;
  const words = titleWords(m[2]);
  const label = siteLabel(host);
  const named = titleWords(siteName).join("");
  const isSite =
    words.length > 0 &&
    ((label && (words.join("") === label || words[0] === label)) || (named && words.join("") === named));
  return isSite ? m[1] : text;
}

// The name a video from a page gets: the page's og:title, else its title,
// without the site's name after it (stripSiteName). `page` is { title,
// ogTitle, siteName } from the tab's top frame; `url` is the tab's address.
// A single-page site may leave its first page's og:title behind as it moves
// on, so og:title is only taken when the title holds it or there is none.
function videoName(page, url) {
  const title = String((page && page.title) || "").trim();
  const og = String((page && page.ogTitle) || "").trim();
  const chosen = og && (!title || title.toLowerCase().includes(og.toLowerCase())) ? og : title;
  return stripSiteName(chosen, hostOf(url || ""), page && page.siteName);
}

// The requests of one media address kept (the newest), each with the frame
// that made it, so it can be matched to the player that asked.
const REQUESTS_PER_MEDIA = 10;
// Players each tab started kept (the newest).
const STARTS_PER_TAB = 100;
// A player may fetch its playlist just before its element says it started
// loading (or the element's message arrives late): a playlist requested
// this soon before a player's start is that player's when no player was
// playing yet.
const START_GRACE_MS = 1500;

// Remember that player `element` (a number the frame's content script gave
// it) in frame `frameId` of tab `tabId` started loading or playing at `now`.
// `store` maps tab id -> [{ frameId, element, time }], oldest first.
function noteMediaStart(store, tabId, frameId, element, now) {
  if (!store.has(tabId)) store.set(tabId, []);
  const starts = store.get(tabId);
  starts.push({ frameId: frameId || 0, element, time: now });
  if (starts.length > STARTS_PER_TAB) starts.splice(0, starts.length - STARTS_PER_TAB);
}

// The player of frame `frameId` a request made at `time` belongs to: the one
// that started last before it, else the first to start within
// START_GRACE_MS after it. null when no player of that frame started.
function startedElement(starts, frameId, time) {
  let before = null;
  let after = null;
  for (const s of starts || []) {
    if (s.frameId !== (frameId || 0)) continue;
    if (s.time <= time) {
      if (!before || s.time >= before.time) before = s;
    } else if (s.time - time <= START_GRACE_MS && (!after || s.time < after.time)) {
      after = s;
    }
  }
  const found = before || after;
  return found ? found.element : null;
}

// The entries of a tab's list (`entries`) that belong to player `element`
// of frame `frameId`: the files and streams at one of its addresses
// (`srcs`: its currentSrc, src and <source> children, or an <object>'s or
// <embed>'s address), and the streams that frame asked for while this
// player was the one playing (see startedElement), which is how a player
// fed through a blob: address is told apart from the others on a feed. A
// video page (YouTube's) goes with them. When nothing belongs to the player,
// all of `entries`: a menu with the page's videos beats an empty one.
function elementEntries(entries, starts, frameId, element, srcs) {
  const addresses = new Set((srcs || []).filter((u) => typeof u === "string" && /^https?:/i.test(u)));
  const own = entries.filter(
    (e) =>
      e.kind !== "page" &&
      (addresses.has(e.url) ||
        (e.kind !== "file" &&
          element != null &&
          (e.requests || []).some((r) => r.frameId === (frameId || 0) && startedElement(starts, r.frameId, r.time) === element))),
  );
  if (own.length === 0) return entries;
  return [...own, ...entries.filter((e) => e.kind === "page")];
}

// Forget what tab `tabId` loaded before `before` (a time): a single-page
// site moved to another page without loading one. What came in after it is
// the new page's, and so is an address the new page asked for again.
// Returns whether anything went.
function forgetMedia(store, tabId, before) {
  const tab = store.get(tabId);
  if (!tab) return false;
  let gone = false;
  for (const [url, e] of tab) {
    if (Math.max(e.time, ...(e.requests || []).map((r) => r.time)) < before) {
      tab.delete(url);
      gone = true;
    }
  }
  if (tab.size === 0) store.delete(tabId);
  return gone;
}

let nextMediaId = 1;

// Remember media `found` (from classifyMedia) at `url` for tab `tabId`.
// `store` maps tab id -> Map(url -> entry). The same URL again keeps its
// entry, learning its size if it was unknown. Each request is noted with
// the frame `frameId` that made it (see elementEntries). `headers` are the
// [name, value] pairs the player's request sent, when the worker saw them;
// the latest request's are kept. Returns the entry.
function recordMedia(store, tabId, url, found, now, frameId, headers) {
  let tab = store.get(tabId);
  if (!tab) {
    tab = new Map();
    store.set(tabId, tab);
  }
  const request = { frameId: frameId || 0, time: now };
  let entry = tab.get(url);
  if (entry) {
    if (!entry.size && found.size) entry.size = found.size;
    if (headers && headers.length) entry.headers = headers;
    if (found.kind !== "page") {
      entry.requests.push(request);
      if (entry.requests.length > REQUESTS_PER_MEDIA) entry.requests.shift();
    }
    return entry;
  }
  entry = { id: nextMediaId++, url, kind: found.kind, mime: found.mime, size: found.size, time: now, requests: found.kind === "page" ? [] : [request] };
  if (headers && headers.length) entry.headers = headers;
  tab.set(url, entry);
  while (tab.size > MEDIA_PER_TAB) tab.delete(tab.keys().next().value);
  return entry;
}

// What the player's own request for media `entry` adds to a probe or an
// "add" message, so UDM asks the server the way the player did: the
// headers it sent (`request_headers`, an Origin or a site's token header
// among them; a server can refuse a stream asked for without them) and its
// Referer (`referrer`). Cookies go in their own field, from the cookie
// store, and pseudo-headers are not headers.
function mediaRequest(entry) {
  const out = {};
  const headers = ((entry && entry.headers) || []).filter(
    ([name]) => name && !name.startsWith(":") && name.toLowerCase() !== "cookie",
  );
  if (headers.length) out.request_headers = headers;
  const referer = headers.find(([name]) => name.toLowerCase() === "referer");
  if (referer && /^https?:\/\//i.test(referer[1])) out.referrer = referer[1];
  return out;
}

// A file's type for people: "MP4 video".
// The extension a saved media file gets: the address's when it is a media
// type, else the one the server's type names, else mp4 or mp3. A server can
// send "clip.exe" as video/mp4; the saved file must not end in .exe.
function mediaFileExtension(entry) {
  if (MEDIA_FILE_EXTENSIONS[entry.ext]) return entry.ext;
  const fromUrl = extensionOf(urlPath(entry.url));
  if (MEDIA_FILE_EXTENSIONS[fromUrl]) return fromUrl;
  const fromMime = mimeExtension(entry.mime);
  if (MEDIA_FILE_EXTENSIONS[fromMime]) return fromMime;
  return (entry.mime || "").startsWith("audio/") ? "mp3" : "mp4";
}

function fileLabel(entry) {
  const ext = mediaFileExtension(entry);
  const audio = (entry.mime || "").startsWith("audio/") || MEDIA_FILE_EXTENSIONS[ext] === "audio";
  if (!ext) return audio ? uiText("audioFile") : uiText("videoFile");
  return audio ? uiText("audioFileType", ext.toUpperCase()) : uiText("videoFileType", ext.toUpperCase());
}

// When UDM found that a stream entry is really a whole video or audio file
// (`info.file`: a page loaded an MP4 under a .m3u8 name or a playlist's
// type), the entry becomes that file, downloaded as one. Returns whether it
// did.
function adoptFile(entry, info) {
  const file = info && info.file;
  if (entry.kind === "page" || !file || !MEDIA_FILE_EXTENSIONS[file.ext]) return false;
  entry.kind = "file";
  entry.ext = file.ext;
  entry.mime = "";
  if (!entry.size && file.size) entry.size = file.size;
  return true;
}

// Remember that tab `tabId` shows the video page `url` (from videoPageUrl).
// These sites move between videos without loading a new page, so the tab's
// earlier video page goes.
function recordPage(store, tabId, url, now) {
  const tab = store.get(tabId);
  if (tab) for (const [key, e] of tab) if (e.kind === "page" && key !== url) tab.delete(key);
  const entry = recordMedia(store, tabId, url, { kind: "page", mime: "", size: 0 }, now);
  const page = videoPage(url);
  if (page) entry.site = page.site;
  return entry;
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
// A page's site is the entry's `site` (see recordPage), else its address's.
function streamLabel(entry) {
  if (entry.kind === "page") {
    const site = VIDEO_SITES.find((s) => s.site === (entry.site || (videoPage(entry.url) || {}).site));
    return site ? site.label() : uiText("pageVideo");
  }
  if (entry.kind === "hls") return uiText("hlsStream");
  if (entry.kind === "hds") return uiText("hdsStream");
  return uiText("dashStream");
}

// A media entry's kind for people: "MP4 video", "HLS stream".
function mediaLabel(entry) {
  return entry.kind === "file" ? fileLabel(entry) : streamLabel(entry);
}

// The sound-only formats the menu offers (UDM converts with ffmpeg).
const SOUND_FORMATS = ["m4a", "mp3", "opus", "flac"];
const SOUND_NAMES = { m4a: "M4A", mp3: "MP3", opus: "Opus", flac: "FLAC" };

// A YouTube video page that is part of a playlist (`list=` in its address):
// the playlist's own page, whose videos UDM can list. null otherwise.
function playlistPageUrl(url) {
  let u;
  try {
    u = new URL(url);
  } catch (_) {
    return null;
  }
  const site = VIDEO_SITES[0];
  if (!/^https?:$/.test(u.protocol) || !site.hosts.includes(u.hostname.toLowerCase())) return null;
  const list = u.searchParams.get("list");
  // Mixes ("RD...") are made for each viewer and have no page of their own.
  if (!list || !/^[\w-]+$/.test(list) || list.startsWith("RD")) return null;
  return `https://www.youtube.com/playlist?list=${list}`;
}

// A page that is none of VIDEO_SITES' videos, for yt-dlp to try: its
// http or https address without the part after "#", or null.
function otherVideoPage(url) {
  let u;
  try {
    u = new URL(url);
  } catch (_) {
    return null;
  }
  if (!/^https?:$/.test(u.protocol) || videoPage(url)) return null;
  u.hash = "";
  return u.toString();
}

// The video button's menu for a tab's `entries`, newest first. `infos` maps a
// stream's (or video page's) URL to what UDM said it offers (a MediaInfo). Each item is
// { entry, label, options: [{ label, size, video, audio }], subtitles, drm, error },
// where `subtitles` lists the stream's subtitle tracks as { id, label, on },
// `on` for the ones the stream marks as default.
// A stream that is one quality of another listed stream (an HLS variant
// playlist the player loaded after its master playlist) is left out, and so
// is an address UDM found is no stream at all, unless nothing else can be
// downloaded.
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
    const label = streamLabel(e);
    if (!info) {
      items.push({ entry: e.id, label, options: [], error: uiText("mediaNoAnswer") });
      continue;
    }
    if (info.drm || info.error) {
      items.push({ entry: e.id, label, options: [], drm: !!info.drm, error: info.error || "", notStream: !!info.not_stream });
      continue;
    }
    // A playlist or channel: its videos are picked in UDM's window.
    const listed = ((info.playlist || {}).entries || []).length;
    if (listed > 0) {
      const title = String(info.playlist.title || "").trim();
      items.push({
        entry: e.id,
        label: title ? uiText("playlistTitled", [title, String(listed)]) : uiText("playlistVideos", String(listed)),
        options: [{ label: uiText("chooseVideos"), size: null, playlist: true }],
      });
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
    // The sound alone (UDM needs ffmpeg for it): from the stream's audio
    // track when it has one, else from its smallest video.
    const sound = (info.audio || [])[0];
    const smallest = (info.video || []).at(-1);
    if (sound || smallest) {
      for (const format of SOUND_FORMATS) {
        options.push({
          label: uiText("soundOnly", SOUND_NAMES[format]),
          size: sound ? sound.size || null : null,
          video: sound ? undefined : (smallest && smallest.id) || undefined,
          audio: sound ? sound.id : undefined,
          audioOnly: format,
        });
      }
    }
    const subtitles = (info.subtitles || [])
      .filter((s) => s && typeof s.id === "string")
      .map((s) => ({ id: s.id, label: s.label || s.language || s.id, on: !!s.default }));
    items.push(subtitles.length ? { entry: e.id, label, options, subtitles } : { entry: e.id, label, options });
  }
  const usable = items.some((i) => i.options.length > 0);
  return items.filter((i) => !(usable && i.notStream)).map(({ notStream, ...i }) => i);
}

// Whether the video button shows on a page at `url` under UDM's `rules`.
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
  module.exports = { SOUND_FORMATS, playlistPageUrl, otherVideoPage, siteLabel, stripSiteName, videoName, noteMediaStart, startedElement, elementEntries, forgetMedia, START_GRACE_MS, adoptFile, autoCaptureMedia, mediaFileExtension, classifyMedia, recordMedia, recordPage, videoPage, videoPageUrl, pageTitle, recentMedia, mediaLabel, mediaMenu, mediaRequest, videoButtonAllowed, titleFileName, responseSize, uiText, MEDIA_PER_TAB };
}
