// Capture rules: which downloads the browser starts are taken by UIDM.
//
// The rules come from UIDM's settings (`capture_types`, `capture_min_size_kb`,
// `capture_excluded_sites` and `capture_excluded_urls`), so they are edited in one place, the
// window's Options. These are plain functions: the service worker loads them
// with importScripts, and the tests load them with require.

// Types for downloads whose name is not known yet when the browser reports
// them, by the MIME type the server sent.
const MIME_EXTENSIONS = {
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
  "application/x-iso9660-image": "iso",
};

// MIME types that say only "a file": taken when the type list is not empty
// but the name says nothing yet, because that is what most real downloads
// look like before their name is known.
const GENERIC_MIME = new Set(["application/octet-stream", "binary/octet-stream", "application/force-download"]);

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
// "Download with UIDM" button: UIDM's `selection_button` is "off",
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

// What a key held on a link click asks for, by UIDM's `capture_skip_key`
// and `capture_force_key`: "skip", "force" or "". `keys` is
// { alt, ctrl, shift }.
function keyAction(rules, keys) {
  if (!rules || !keys) return "";
  const held = (k) => (k === "alt" && keys.alt) || (k === "ctrl" && keys.ctrl) || (k === "shift" && keys.shift);
  if (held(rules.capture_skip_key)) return "skip";
  if (held(rules.capture_force_key)) return "force";
  return "";
}

// A click with the skip key held keeps the next download in the browser if
// it starts this soon after (its link may redirect elsewhere first).
const CLICK_WINDOW_MS = 5000;

function clickApplies(click, now) {
  return !!click && now - click.time >= 0 && now - click.time <= CLICK_WINDOW_MS;
}

// Decide whether UIDM takes `item`, a download the browser just reported:
// { url, referrer, filename, mime, size, key }. `key` is a held key's
// action ("skip" or "force"), which overrides every other rule. `size` is 0 or missing when the
// browser does not know it yet; then only the name and site are checked.
// `rules` is UIDM's settings, or null before they arrive (take everything).
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

if (typeof module !== "undefined") {
  module.exports = { extensionOf, urlPath, hostOf, siteMatches, downloadType, captureDecision, keyAction, clickApplies, urlMatches, selectionButtonAllowed };
}
