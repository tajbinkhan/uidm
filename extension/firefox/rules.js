// Capture rules: which downloads the browser starts are taken by UDM.
//
// The rules come from UDM's settings (`capture_types`, `capture_min_size_kb`,
// `capture_excluded_sites` and `capture_excluded_urls`), so they are edited in one place, the
// window's Options. These are plain functions: the service worker loads them
// with importScripts, and the tests load them with require.
//
// The page loads this file as a content script too, and a frame can get it
// twice: from the manifest and from the worker's injectOpenTabs. So its
// top-level names are `var` and `function`, which a second copy may declare
// again; a `const` or `let` there would throw "has already been declared".

// Types for downloads whose name is not known yet when the browser reports
// them, by the MIME type the server sent.
var MIME_EXTENSIONS = {
  "application/pdf": "pdf",
  "application/zip": "zip",
  "application/x-zip-compressed": "zip",
  "application/vnd.rar": "rar",
  "application/x-rar-compressed": "rar",
  "application/x-7z-compressed": "7z",
  "application/x-tar": "tar",
  "application/gzip": "gz",
  "application/x-gzip": "gz",
  "application/x-bzip2": "bz2",
  "application/x-xz": "xz",
  "application/msword": "doc",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document": "docx",
  "application/vnd.ms-excel": "xls",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet": "xlsx",
  "application/vnd.ms-powerpoint": "ppt",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation": "pptx",
  "application/rtf": "rtf",
  "application/vnd.oasis.opendocument.text": "odt",
  "text/plain": "txt",
  "audio/mpeg": "mp3",
  "audio/flac": "flac",
  "audio/wav": "wav",
  "audio/x-wav": "wav",
  "audio/mp4": "m4a",
  "audio/aac": "aac",
  "audio/ogg": "ogg",
  "application/x-msdownload": "exe",
  "application/vnd.microsoft.portable-executable": "exe",
  "application/x-msi": "msi",
  "application/vnd.android.package-archive": "apk",
  "video/mp4": "mp4",
  "video/x-matroska": "mkv",
  "video/x-msvideo": "avi",
  "video/quicktime": "mov",
  "video/webm": "webm",
  "video/x-ms-wmv": "wmv",
  "video/ogg": "ogv",
  "video/x-flv": "flv",
  "video/3gpp": "3gp",
  "video/x-m4v": "m4v",
  "audio/opus": "opus",
  "audio/x-flac": "flac",
  "audio/x-m4a": "m4a",
  "application/x-iso9660-image": "iso",
  "application/x-bittorrent": "torrent",
};

// MIME types that say only "a file": taken when the type list is not empty
// but the name says nothing yet, because that is what most real downloads
// look like before their name is known.
var GENERIC_MIME = new Set(["application/octet-stream", "binary/octet-stream", "application/force-download"]);

// The file type a MIME type stands for, such as "video/mp4" -> "mp4", or "".
function mimeExtension(mime) {
  return MIME_EXTENSIONS[String(mime || "").split(";")[0].trim().toLowerCase()] || "";
}

// "report.PDF" -> "pdf"; "README" and ".bashrc" -> "".
function extensionOf(name) {
  const base = String(name || "").split(/[\\/]/).pop();
  const dot = base.lastIndexOf(".");
  if (dot <= 0 || dot === base.length - 1) return "";
  return base.slice(dot + 1).toLowerCase();
}

function urlPath(url) {
  try {
    return new URL(url).pathname;
  } catch (_) {
    return "";
  }
}

function hostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch (_) {
    return "";
  }
}

// Whether `host` belongs to the site `pattern`. "example.com" and
// "*.example.com" both cover example.com and every subdomain; any other "*"
// matches within the host name. A port in the pattern is ignored.
function siteMatches(pattern, host) {
  if (!host) return false;
  const p = String(pattern || "").trim().toLowerCase().replace(/:\d*$/, "");
  if (!p) return false;
  if (p.startsWith("*.") && !p.slice(2).includes("*")) {
    const base = p.slice(2);
    return host === base || host.endsWith("." + base);
  }
  if (p.includes("*")) {
    const re = new RegExp("^" + p.split("*").map((s) => s.replace(/[.+?^${}()|[\]\\-]/g, "\\$&")).join("[a-z0-9.-]*") + "$");
    return re.test(host);
  }
  return host === p || host.endsWith("." + p);
}

// Whether `url` is the address `pattern`, where `*` matches any run of
// characters. Case does not matter, and a pattern without a scheme matches
// either scheme.
function urlMatches(pattern, url) {
  const p = String(pattern || "").trim().toLowerCase();
  const u = String(url || "").trim().toLowerCase();
  if (!p || !u) return false;
  const re = new RegExp("^" + p.split("*").map((s) => s.replace(/[.+?^${}()|[\]\\/-]/g, "\\$&")).join(".*") + "$");
  return re.test(u) || (!/^[a-z]+:\/\//.test(p) && re.test(u.replace(/^[a-z]+:\/\//, "")));
}

// Whether selecting text with links on the page at `url` shows the
// "Download with UDM" button: UDM's `selection_button` is "off",
// "everywhere" or "sites" (those in `selection_button_sites`).
function selectionButtonAllowed(rules, url) {
  const mode = (rules && rules.selection_button) || "everywhere";
  if (mode === "off") return false;
  if (mode === "everywhere") return true;
  const host = hostOf(url);
  return (rules.selection_button_sites || []).some((site) => siteMatches(site, host));
}

// The file type of a download: from its name, else its MIME type, else its
// URL path. "" when none says.
function downloadType(item) {
  const mime = String(item.mime || "").split(";")[0].trim().toLowerCase();
  return extensionOf(item.filename) || MIME_EXTENSIONS[mime] || extensionOf(urlPath(item.url));
}

// A key combination as UDM stores it ("ctrl+shift", "none") made into
// { alt, ctrl, shift, insert, del }. Del is stored "del" ("delete" also
// reads).
var KEY_NAMES = ["alt", "ctrl", "shift", "insert", "del"];
function parseKeys(combo) {
  const parts = String(combo || "none").toLowerCase().split("+").map((p) => p.trim()).map((p) => (p === "delete" ? "del" : p));
  const keys = {};
  for (const k of KEY_NAMES) keys[k] = parts.includes(k);
  return keys;
}

// What the keys held on a link click ask for, by UDM's `capture_skip_key`
// and `capture_force_key`: "skip", "force" or "". `keys` is the click's
// { alt, ctrl, shift, insert, del, button, mouse }: `mouse` is false for a click
// made with Enter. A combination must be held exactly, no more and no less.
// With `capture_keys_left_click`, only a left mouse click counts.
function keyAction(rules, keys) {
  if (!rules || !keys) return "";
  if (rules.capture_keys_left_click && !(keys.button === 0 && keys.mouse !== false)) return "";
  const held = (combo) => {
    const want = parseKeys(combo);
    const any = KEY_NAMES.some((k) => want[k]);
    return any && KEY_NAMES.every((k) => want[k] === !!keys[k]);
  };
  if (held(rules.capture_skip_key)) return "skip";
  if (held(rules.capture_force_key)) return "force";
  return "";
}

// Insert and Del are no modifiers, so a click does not say whether they are
// down: the content script follows them with `trackKey` on every keydown,
// keyup and blur of the window. `held` is { insert, del }; `type` is the
// event's type and `key` its `key`. A blur forgets both, since the key may be
// let go while another window has the focus.
function trackKey(held, type, key) {
  if (type === "blur") {
    held.insert = false;
    held.del = false;
  } else if (type === "keydown" || type === "keyup") {
    const down = type === "keydown";
    if (key === "Insert") held.insert = down;
    if (key === "Delete" || key === "Del") held.del = down;
  }
  return held;
}

// As in IDM, the skip or force keys held down count not only for a link
// clicked meanwhile but for any download their tab starts while they are
// down: a form sent, a script's download. The content script reports what
// the held keys ask for ("skip", "force" or "") to the service worker when
// it changes, and again every HELD_REFRESH_MS while a key stays down (the
// key's own repeats). The worker forgets a report HELD_EXPIRE_MS old, so a
// release it never heard of (a frame that went away) cannot keep the keys
// "held" for good.
var HELD_REFRESH_MS = 10000;
var HELD_EXPIRE_MS = 30000;

// Whether the content script sends `action` now: it differs from what it
// last sent, or it is a held key's that is due to be refreshed. `state` is
// { sent, time }, what was last sent and when; it is updated on a send.
function heldReport(state, action, now) {
  const same = action === state.sent;
  if (same && !(action && now - state.time >= HELD_REFRESH_MS)) return false;
  state.sent = action;
  state.time = now;
  return true;
}

// Whether the worker's last report for a tab, { action, time }, still holds.
function heldApplies(entry, now) {
  return !!entry && !!entry.action && now - entry.time >= 0 && now - entry.time <= HELD_EXPIRE_MS;
}

// The toolbar icon of a tab with capture off is UDM's own icon in grey:
// `data` is an ImageData's RGBA bytes, turned to their brightness in place,
// a little lighter so the icon reads as "off".
function greyPixels(data) {
  for (let i = 0; i + 3 < data.length; i += 4) {
    const y = Math.round(0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2]);
    data[i] = data[i + 1] = data[i + 2] = Math.min(255, Math.round(y * 0.6 + 90));
  }
  return data;
}

// The context menu items UDM's settings leave out in this browser: Chrome,
// Edge and the other Chromium browsers have `browser_menu_hidden_chromium`,
// Firefox `browser_menu_hidden_firefox`. An older UDM sends one
// `browser_menu_hidden` for both.
function hiddenMenuItems(rules, firefox) {
  if (!rules) return [];
  const own = firefox ? rules.browser_menu_hidden_firefox : rules.browser_menu_hidden_chromium;
  if (Array.isArray(own)) return own;
  return Array.isArray(rules.browser_menu_hidden) ? rules.browser_menu_hidden : [];
}

// A link's `download` attribute as a file name for UDM: the page chose it,
// so only its last part counts (no folders), without the characters Windows
// forbids, and none when it is empty (a bare `download` keeps the server's).
function linkFileName(name) {
  const base = String(name || "")
    .split(/[\\/]/)
    .pop()
    .replace(/[<>:"|?*\u0000-\u001f]/g, "_")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/^\.+|[. ]+$/g, "")
    .slice(0, 200);
  return base || undefined;
}

// A link's description for UDM's list: its title, else its text, on one
// line and not too long; none when it has neither.
function linkDescription(title, text) {
  const clean = (s) => String(s || "").replace(/\s+/g, " ").trim();
  const description = (clean(title) || clean(text)).slice(0, 300);
  return description || undefined;
}

// Web pages, pictures and scripts, which the force key leaves alone when
// `capture_force_skips_pages` is on.
var PAGE_EXTENSIONS = new Set([
  "htm", "html", "shtml", "xhtml", "php", "asp", "aspx", "jsp", "cgi", "pl",
  "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "svg", "ico",
  "js", "mjs", "css",
]);

// Whether a forced click on a link to `url` should open it as usual: the
// setting is on and the link is a page, a picture or a script. A link with
// no file name ("/", "/news/") is a page.
function forceLeavesLink(rules, url) {
  if (!rules || rules.capture_force_skips_pages === false) return false;
  const path = urlPath(url);
  const ext = extensionOf(path);
  return ext ? PAGE_EXTENSIONS.has(ext) : path === "" || path.endsWith("/");
}

// Whether `url` is a magnet link UDM can download as a torrent: `magnet:?`
// with a BitTorrent info hash (40 hexadecimal or 32 base32 characters).
function isMagnet(url) {
  const s = String(url || "");
  return /^magnet:\?/i.test(s) && /[?&]xt=urn:btih:([0-9a-f]{40}|[a-z2-7]{32})(&|$)/i.test(s);
}

// Whether UDM's settings take `.torrent` files as torrents (its
// `torrents.catch_torrent_files`, on unless turned off).
function catchesTorrents(rules) {
  return !!(rules && rules.torrents && rules.torrents.catch_torrent_files !== false);
}

// Whether a download, { url, filename, mime }, is a `.torrent` file: by its
// type or its name.
function isTorrentFile(item) {
  const mime = String(item.mime || "").split(";")[0].trim().toLowerCase();
  return mime === "application/x-bittorrent" || extensionOf(item.filename) === "torrent" || extensionOf(urlPath(item.url)) === "torrent";
}

// Whether a click on a link to `url` is a download to take as it is clicked,
// where the browser tells extensions nothing of its downloads (Safari; see
// shareLinkCapture in background.js). `download` is the link's `download`
// attribute (or null), `page` the address of the page clicked in, and
// `rules` the capture rules the worker shares. What a link leads to is not
// known until the browser follows it, so only a link that names a file goes,
// by its `download` attribute or its address, and only when that file's
// type is one of the captured types (never a page, picture or script):
// without rules, or with no types listed, no click is taken, where a
// download the browser reports would be. Then the excluded sites and
// addresses decide.
function clickTaken(rules, url, download, page) {
  if (!rules || !/^https?:\/\//i.test(String(url || ""))) return false;
  const types = rules.capture_types || [];
  const filename = linkFileName(download) || "";
  const type = extensionOf(filename) || extensionOf(urlPath(url));
  const torrent = type === "torrent" && catchesTorrents(rules);
  if (!torrent && (!type || PAGE_EXTENSIONS.has(type) || !types.includes(type))) return false;
  return captureDecision(rules, { url, referrer: page || "", filename, mime: "", size: 0 }).capture;
}

// A click with the skip key held keeps the next download in the browser if
// it starts this soon after (its link may redirect elsewhere first).
var CLICK_WINDOW_MS = 5000;

function clickApplies(click, now) {
  return !!click && now - click.time >= 0 && now - click.time <= CLICK_WINDOW_MS;
}

// Decide whether UDM takes `item`, a download the browser just reported:
// { url, referrer, filename, mime, size, key }. `key` is a held key's
// action ("skip" or "force"), which overrides every other rule. `size` is 0 or missing when the
// browser does not know it yet; then only the name and site are checked.
// `rules` is UDM's settings, or null before they arrive (take everything).
// Returns { capture, reason }; `reason` says why a download stays.
function captureDecision(rules, item) {
  if (item.key === "skip") return { capture: false, reason: "the skip key was held" };
  if (item.key === "force") return { capture: true, reason: "" };
  if (!rules) return { capture: true, reason: "" };

  for (const site of rules.capture_excluded_sites || []) {
    if (siteMatches(site, hostOf(item.url)) || siteMatches(site, hostOf(item.referrer))) {
      return { capture: false, reason: `excluded site ${site}` };
    }
  }
  for (const address of rules.capture_excluded_urls || []) {
    if (urlMatches(address, item.url)) return { capture: false, reason: `excluded address ${address}` };
  }
  // A `.torrent` file goes to UDM, which downloads its torrent, whatever
  // its size (they are small) and the captured types.
  if (catchesTorrents(rules) && isTorrentFile(item)) return { capture: true, reason: "" };

  const minBytes = (rules.capture_min_size_kb || 0) * 1024;
  if (minBytes > 0 && item.size > 0 && item.size < minBytes) {
    return { capture: false, reason: `smaller than ${rules.capture_min_size_kb} KB` };
  }

  const types = rules.capture_types || [];
  if (types.length === 0) return { capture: true, reason: "" };
  const type = downloadType(item);
  if (type) {
    return types.includes(type)
      ? { capture: true, reason: "" }
      : { capture: false, reason: `.${type} is not a captured type` };
  }
  const mime = String(item.mime || "").split(";")[0].trim().toLowerCase();
  return GENERIC_MIME.has(mime)
    ? { capture: true, reason: "" }
    : { capture: false, reason: "unknown file type" };
}

// The file fields of a form being sent, from its `elements`: { names,
// chosen }, the names of its named file inputs and whether any has a file.
// The service worker rebuilds a multipart form's download without them, or
// leaves it in the browser when a file goes along (its bytes cannot be
// read back from the request). Null for a form without file fields.
function formFileFields(elements) {
  const files = Array.from(elements || []).filter((e) => e && String(e.type).toLowerCase() === "file" && e.name && !e.disabled);
  if (!files.length) return null;
  return { names: [...new Set(files.map((e) => String(e.name)))], chosen: files.some((e) => e.files && e.files.length > 0) };
}

// The video button's side of a page (content.js), kept here as plain
// functions the tests can call.

// Media file types and playlists an <object> or <embed> may play.
var PLUGIN_MEDIA_EXTENSIONS = new Set([
  "mp4", "m4v", "webm", "mkv", "mov", "ogv", "avi", "wmv", "flv", "3gp",
  "mp3", "m4a", "aac", "ogg", "oga", "opus", "flac", "wav", "m3u8", "mpd", "f4m",
]);

// Whether an <object> or <embed> with type `type` at address `url` (its
// `data` or `src`) is a media player: its type names video, audio or a
// stream playlist, its address is a media file or playlist, or it is an
// embedded YouTube or Vimeo player. Other plugins (PDF viewers, widgets)
// get no video button.
function mediaPlugin(type, url) {
  const mime = String(type || "").split(";")[0].trim().toLowerCase();
  if (/^(video|audio)\//.test(mime) || /mpegurl$|dash\+xml$/.test(mime)) return true;
  if (!/^https?:\/\//i.test(String(url || ""))) return false;
  if (PLUGIN_MEDIA_EXTENSIONS.has(extensionOf(urlPath(url)))) return true;
  return /^https?:\/\/(?:www\.)?(?:youtube(?:-nocookie)?\.com\/(?:v|embed)\/|player\.vimeo\.com\/video\/)/i.test(url);
}

// The element that holds the player `el` with its controls and overlays:
// its furthest ancestor (below <body>) not much bigger than it. A custom
// player draws its controls, poster and click catcher over the video in
// such a box, and they do not cover it.
function playerContainer(el) {
  const r = el.getBoundingClientRect();
  const area = Math.max(1, r.width * r.height);
  let box = el;
  for (let p = el.parentElement; p && p.parentElement && !/^(body|html)$/i.test(p.tagName); p = p.parentElement) {
    const q = p.getBoundingClientRect();
    if (q.width * q.height > area * 1.5) break;
    box = p;
  }
  return box;
}

// Points of rectangle `r` to look at for what covers it: its centre and
// four more around it, kept inside a `width` x `height` viewport.
function samplePoints(r, width, height) {
  const at = (fx, fy) => [
    Math.min(width - 1, Math.max(0, r.left + r.width * fx)),
    Math.min(height - 1, Math.max(0, r.top + r.height * fy)),
  ];
  return [at(0.5, 0.5), at(0.25, 0.25), at(0.75, 0.25), at(0.25, 0.75), at(0.75, 0.75)];
}

// Whether the player `el` is covered by something else on the page (a
// dialog, a sticky banner, another tab's panel): at every point of `points`
// the topmost element (`hitAt(x, y)`, document.elementFromPoint) is outside
// the player's `container` and is not one of its ancestors. UDM's own
// button (`ignore`) covers nothing; points where nothing is (off screen)
// say nothing.
function playerCovered(el, container, points, hitAt, ignore) {
  let looked = 0;
  for (const [x, y] of points) {
    const hit = hitAt(x, y);
    if (!hit || hit === ignore) continue;
    looked++;
    if (container.contains(hit) || hit.contains(el)) return false;
  }
  return looked > 0;
}

if (typeof module !== "undefined") {
  module.exports = { isMagnet, catchesTorrents, isTorrentFile, mediaPlugin, playerContainer, samplePoints, playerCovered, mimeExtension, extensionOf, urlPath, hostOf, siteMatches, downloadType, captureDecision, keyAction, parseKeys, trackKey, hiddenMenuItems, linkFileName, linkDescription, forceLeavesLink, clickTaken, clickApplies, urlMatches, selectionButtonAllowed, formFileFields, heldReport, heldApplies, greyPixels, HELD_REFRESH_MS, HELD_EXPIRE_MS };

}
