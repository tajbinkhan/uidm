// The links of a page, for "Download all links" and "Download selected
// links" (and the selection button, and whether Firefox shows the latter).
//
// As IDM does, a page's links are more than its <a> and <area> elements:
// its frames' addresses, addresses in its inline scripts and in images'
// onclick handlers, the text selected in a text box, the images inside a
// selection, and the links of frames made from nothing (about:blank or
// srcdoc), which the browser runs no script of UDM's in.
//
// The worker injects this file into each frame before asking it for its
// links, and content.js loads it too, so it may run twice in one frame: it
// only declares functions and `var`s, which a second run just declares
// again. The extraction works on plain text, and the walker only on a few
// DOM calls, so the tests run it in Node with a small fake document.

// The most script text read in one page, so a page with megabytes of
// inline scripts does not hang the frame.
var LINKS_SCRIPT_BYTES = 1024 * 1024;
// The most links one frame gives, as the worker keeps to MAX_PAGE_LINKS.
var LINKS_MAX = 5000;
// How deep frames made from nothing are walked into.
var LINKS_FRAME_DEPTH = 3;
// File types a quoted relative path in a script or handler must have to be
// taken for a link (everything else there is mostly junk: routes, CSS
// classes, template strings), besides UDM's captured types. These are the
// defaults of UDM's categories.
var LINKS_FILE_TYPES = [
  "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "odt",
  "mp3", "flac", "wav", "m4a", "aac", "ogg", "exe", "msi", "apk", "msix", "mp4", "mkv", "avi", "mov", "webm", "wmv",
  "iso", "img", "dmg", "deb", "rpm", "bin",
];
// An image's onclick handler mostly opens its larger picture, so there
// pictures count too.
var LINKS_IMAGE_TYPES = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff"];

// `raw` as an absolute http, https, ftp or ftps address against `base`, without
// its fragment (which never reaches the server, so `a#1` and `a#2` are one
// file); "" when it is none.
function resolveLink(raw, base) {
  const s = String(raw == null ? "" : raw).trim();
  if (!s) return "";
  let u;
  try {
    u = base ? new URL(s, base) : new URL(s);
  } catch (_) {
    return "";
  }
  if (!/^(https?|ftps?):$/.test(u.protocol)) return "";
  u.hash = "";
  return u.href;
}

// An address's end, as text: the punctuation a sentence puts after it.
function trimLinkEnd(s) {
  return s.replace(/[).,;:!?\]}'"]+$/, "");
}

// The absolute addresses in free text, such as a selection.
function linksFromText(text, base) {
  const out = [];
  for (const m of String(text || "").matchAll(/\b(?:https?|ftps?):\/\/[^\s<>"'`]+/gi)) {
    const url = resolveLink(trimLinkEnd(m[0]), base);
    if (url) out.push(url);
  }
  return out;
}

// The addresses in script code: absolute ones anywhere (escaped slashes, as
// JSON writes them, undone), and quoted relative paths that end in one of
// `types` (lowercase, without the dot) or LINKS_FILE_TYPES.
function linksFromScriptText(text, base, types) {
  const code = String(text || "").replace(/\\\//g, "/");
  const out = linksFromText(code, base);
  const known = new Set([...LINKS_FILE_TYPES, ...(types || []).map((t) => String(t).toLowerCase())]);
  for (const m of code.matchAll(/(["'`])((?:\.{0,2}\/)?[\w\-.~%/]+\.([A-Za-z0-9]{1,6}))(?:[?#][^"'`\s]*)?\1/g)) {
    if (/^[a-z][a-z0-9+.-]*:/i.test(m[2]) || !known.has(m[3].toLowerCase())) continue;
    const url = resolveLink(m[0].slice(1, -1), base);
    if (url) out.push(url);
  }
  return out;
}

// The addresses in an image's onclick handler (`window.open('big.jpg')`
// and the like), which is script code too.
function linksFromOnclick(attr, base, types) {
  return linksFromScriptText(attr, base, [...(types || []), ...LINKS_IMAGE_TYPES]);
}

// The largest image of a srcset ("a.jpg 1x, b.jpg 2x" or widths): the one
// a download wants. "" when it lists none.
function bestSrcset(srcset, base) {
  let best = "";
  let bestSize = -1;
  for (const part of String(srcset || "").split(/,\s+/)) {
    const [raw, descriptor = "1x"] = part.trim().split(/\s+/);
    const size = parseFloat(descriptor) * (/w$/i.test(descriptor) ? 1 : 10000);
    const url = resolveLink(raw, base);
    if (url && size > bestSize) {
      best = url;
      bestSize = size;
    }
  }
  return best;
}

// `links` ({ url, text, image }) with each address once, in the order first
// seen, at most `max` of them. A link keeps the first text that says
// something, and is an image only when it was found as nothing else (a
// link to an image is still a link).
function dedupeLinks(links, max = LINKS_MAX) {
  const seen = new Map();
  for (const l of links) {
    const url = resolveLink(l && l.url);
    if (!url) continue;
    const had = seen.get(url);
    const text = typeof l.text === "string" ? l.text.slice(0, 200) : "";
    if (had) {
      if (!had.text && text) had.text = text;
      if (had.image && !l.image) delete had.image;
      continue;
    }
    if (seen.size >= max) continue;
    const link = { url, text };
    if (l.image) link.image = true;
    seen.set(url, link);
  }
  return [...seen.values()];
}

// The links of document `doc` as { url, text, image }, each once. With
// `selection`, only what the selection holds: the links, frames, scripts and
// images it touches, the addresses in its text, or in the text selected in
// a text box. `types` are file types a relative path in a script may have
// (UDM's captured types). Frames made from nothing are walked into, as
// they are part of the page and no script of UDM's runs in them.
function collectPageLinks(doc, { selection = false, types = [], max = LINKS_MAX, depth = 0 } = {}) {
  const out = [];
  if (!doc) return out;
  const base = doc.baseURI;
  const text = (s) => String(s || "").replace(/\s+/g, " ").trim().slice(0, 200);
  const add = (url, label, image) => {
    if (url) out.push(image ? { url, text: text(label), image: true } : { url, text: text(label) });
  };
  const attr = (el, name) => (el.getAttribute ? el.getAttribute(name) : null);

  let chosen = () => true;
  if (selection) {
    // A selection in a text box is not the document's: its text is.
    const box = doc.activeElement;
    if (box && /^(TEXTAREA|INPUT)$/i.test(box.tagName || "")) {
      let start = null;
      try {
        start = box.selectionStart;
      } catch (_) {
        // An <input> kind without a text selection (a checkbox).
      }
      if (start != null && box.selectionEnd > start) {
        for (const url of linksFromText(String(box.value || "").slice(start, box.selectionEnd), base)) add(url, "");
      }
    }
    const sel = doc.getSelection ? doc.getSelection() : null;
    if (sel && !sel.isCollapsed && sel.rangeCount) {
      const ranges = Array.from({ length: sel.rangeCount }, (_, i) => sel.getRangeAt(i));
      chosen = (node) => ranges.some((r) => r.intersectsNode(node));
      for (const url of linksFromText(String(sel), base)) add(url, "");
    } else {
      chosen = () => false;
    }
  }

  for (const a of doc.querySelectorAll("a[href], area[href]")) {
    if (chosen(a)) add(resolveLink(attr(a, "href"), base), a.textContent || attr(a, "alt"));
  }
  const empty = [];
  for (const f of doc.querySelectorAll("iframe, frame")) {
    const src = attr(f, "src");
    const url = resolveLink(src, base);
    if (url && chosen(f)) add(url, attr(f, "title") || attr(f, "name"));
    if (!url && (!src || /^about:/i.test(src.trim()))) empty.push(f);
  }
  let budget = LINKS_SCRIPT_BYTES;
  for (const s of doc.querySelectorAll("script")) {
    if (budget <= 0) break;
    if (attr(s, "src") != null || !chosen(s)) continue;
    const code = String(s.textContent || "").slice(0, budget);
    budget -= code.length;
    for (const url of linksFromScriptText(code, base, types)) add(url, "");
  }
  for (const img of doc.querySelectorAll("img")) {
    if (!chosen(img)) continue;
    const alt = attr(img, "alt");
    const onclick = attr(img, "onclick");
    if (onclick) for (const url of linksFromOnclick(onclick, base, types)) add(url, alt);
    add(resolveLink(img.currentSrc || attr(img, "src"), base), alt, true);
    add(bestSrcset(attr(img, "srcset"), base), alt, true);
  }

  if (depth < LINKS_FRAME_DEPTH) {
    for (const f of empty) {
      let inner = null;
      try {
        inner = f.contentDocument; // null, or a throw, when another site's
      } catch (_) {
        inner = null;
      }
      if (inner) out.push(...collectPageLinks(inner, { selection, types, max, depth: depth + 1 }));
    }
  }
  return depth === 0 ? dedupeLinks(out, max) : out;
}

// Whether the selection of `doc` holds anything collectPageLinks would
// collect from it, for the selection button and Firefox's menu.
function selectionHasPageLinks(doc, types) {
  return collectPageLinks(doc, { selection: true, types, max: 1 }).length > 0;
}

if (typeof module !== "undefined") {
  module.exports = { resolveLink, linksFromText, linksFromScriptText, linksFromOnclick, bestSrcset, dedupeLinks, collectPageLinks, selectionHasPageLinks, LINKS_SCRIPT_BYTES, LINKS_MAX };
}
