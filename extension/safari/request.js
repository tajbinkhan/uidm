// The whole request behind a browser download.
//
// The downloads API reports little of how a download was asked for: its
// address, its referrer, sometimes a size. IDM sends the server the same
// request the browser did, so the service worker follows the requests of
// frames with webRequest: the headers the browser sent, the redirects it
// followed and what the response said (its size, type and name). These
// functions keep that record, tell a response that is a download from a
// page, and read the browser's proxy setting, so UDM can repeat the
// request as the browser made it.

// How long a recorded request can match a download, and how many are kept.
const REQUEST_MATCH_MS = 60 * 1000;
const REQUEST_KEEP = 100;

function findHeader(headers, name) {
  const h = (headers || []).find((x) => String(x.name).toLowerCase() === name);
  return h ? String(h.value || "") : "";
}

// Types a browser shows itself rather than saves. A response of one is a
// download only when its Content-Disposition says "attachment".
function renderable(mime) {
  if (!mime) return true;
  if (/^(text|image|video|audio|font|multipart)\//.test(mime)) return true;
  return /^application\/(pdf|json|xml|xhtml\+xml|javascript|ecmascript|x-javascript|rss\+xml|atom\+xml|wasm|manifest\+json|ld\+json|(.+\+)?json|(.+\+)?xml)$/.test(mime);
}

// The file name a Content-Disposition header gives, if any: its RFC 5987
// `filename*` first, then `filename`.
function dispositionName(value) {
  const star = /filename\*\s*=\s*([^']*)'[^']*'([^;]+)/i.exec(value || "");
  if (star) {
    try {
      return decodeURIComponent(star[2].trim().replace(/^"|"$/g, ""));
    } catch (_) {
      // A broken encoding: the plain name below, if there is one.
    }
  }
  const plain = /filename\s*=\s*("((?:[^"\\]|\\.)*)"|[^;]+)/i.exec(value || "");
  if (!plain) return undefined;
  const name = plain[2] !== undefined ? plain[2].replace(/\\(.)/g, "$1") : plain[1].trim();
  return name.split(/[\\/]/).pop() || undefined;
}

// Whether webRequest response `d` (with its responseHeaders) is a file the
// browser will download rather than show: { filename, mime, size,
// attachment }, or null for a page, an error or a redirect. A download is a
// successful response marked as an attachment, or of a type the browser
// cannot show.
function downloadResponse(d) {
  const status = d.statusCode || 0;
  if (!/^https?:\/\//i.test(d.url || "") || status < 200 || status >= 300 || status === 204) return null;
  const headers = d.responseHeaders;
  const disposition = findHeader(headers, "content-disposition");
  const attachment = /^\s*attachment\b/i.test(disposition);
  const mime = findHeader(headers, "content-type").split(";")[0].trim().toLowerCase();
  if (!attachment && renderable(mime)) return null;
  const length = Number(findHeader(headers, "content-length"));
  return {
    filename: dispositionName(disposition),
    mime,
    size: Number.isFinite(length) && length > 0 ? length : 0,
    attachment,
  };
}

// --- The record of requests -------------------------------------------------
//
// `log` is a Map: request id -> { id, url, method, type, tabId, frameId,
// parentFrameId, time, headers, redirects, response }. `url` is where the
// request is now, after any redirects, `redirects` the addresses before it,
// and `response` what downloadResponse said of its answer, once it came.

// Note the request webRequest event `d` (onSendHeaders) sent.
function noteRequest(log, d, now) {
  pruneRequests(log, now);
  const record = log.get(d.requestId) || {
    id: d.requestId,
    redirects: [],
    response: null,
  };
  Object.assign(record, {
    url: d.url,
    method: d.method || "GET",
    type: d.type,
    tabId: d.tabId,
    frameId: d.frameId,
    parentFrameId: d.parentFrameId,
    time: now,
    headers: (d.requestHeaders || []).map((h) => [String(h.name), String(h.value ?? "")]),
  });
  // Kept oldest first, so pruning stops at the first recent one.
  log.delete(d.requestId);
  log.set(d.requestId, record);
  return record;
}

// Note that request `d.requestId` was sent on to `d.redirectUrl`
// (onBeforeRedirect).
function noteRedirect(log, d) {
  const record = log.get(d.requestId);
  if (!record || !d.redirectUrl) return;
  if (record.redirects.length < 20) record.redirects.push(d.url);
  record.url = d.redirectUrl;
}

// Note what the server answered request `d.requestId` (onHeadersReceived):
// the download it is, or null. Returns the record.
function noteResponse(log, d, now) {
  let record = log.get(d.requestId);
  if (!record) {
    // Its headers were not seen (a request already under way when the
    // worker started); its answer still counts.
    record = noteRequest(log, { ...d, requestHeaders: [] }, now);
  }
  record.url = d.url;
  record.response = downloadResponse(d);
  return record;
}

// The latest recent request that is now at `url`, or that went through it.
function findRequest(log, url, now) {
  let found = null;
  for (const r of log.values()) {
    if (now - r.time > REQUEST_MATCH_MS) continue;
    if (r.url === url || r.redirects.includes(url)) {
      if (!found || r.time >= found.time) found = r;
    }
  }
  return found;
}

function pruneRequests(log, now) {
  for (const [id, r] of log) {
    if (now - r.time > REQUEST_MATCH_MS || log.size > REQUEST_KEEP) log.delete(id);
    else break;
  }
}

// Headers that never go to UDM: cookies travel in their own field, and
// pseudo-headers are not headers.
const UNSENT_HEADERS = new Set(["cookie"]);

// What a recorded request adds to UDM's "add" message: the headers the
// browser sent, its redirects, and the response's size and type.
function requestDetails(record) {
  const out = {};
  if (!record) return out;
  const headers = (record.headers || []).filter(([name]) => !name.startsWith(":") && !UNSENT_HEADERS.has(name.toLowerCase()));
  if (headers.length) out.request_headers = headers;
  if (record.redirects && record.redirects.length) out.redirects = [...record.redirects];
  const r = record.response;
  if (r && r.size > 0) out.size = r.size;
  if (r && r.mime) out.content_type = r.mime;
  return out;
}

// The Referer header a recorded request carried, if any.
function requestReferer(record) {
  return record ? (record.headers || []).find(([name]) => name.toLowerCase() === "referer")?.[1] : undefined;
}

// --- Referrers ----------------------------------------------------------------

const isWebAddress = (url) => /^https?:\/\//i.test(url || "");

// The first web address among `candidates`. A frame made from nothing
// (about:blank, about:srcdoc) is no referrer: the page or frame that made
// it is, which comes later in the list.
function webReferrer(...candidates) {
  return candidates.find(isWebAddress);
}

// The address of the nearest frame, from `frameId` up through its parents,
// that has a web address; `frames` is webNavigation.getAllFrames' answer.
function frameReferrer(frames, frameId) {
  const byId = new Map((frames || []).map((f) => [f.frameId, f]));
  let frame = byId.get(frameId);
  for (let hops = 0; frame && hops < 50; hops++) {
    if (isWebAddress(frame.url)) return frame.url;
    frame = byId.get(frame.parentFrameId);
  }
  return undefined;
}

// --- Tabs opened only for a download -------------------------------------------

// How soon after a tab opened a download in it counts as the reason it opened.
const OPENED_FOR_DOWNLOAD_MS = 30 * 1000;

// Whether `tab` was opened just for the download at one of `urls`: another
// tab opened it (`opened`, as { time, opener, loaded } from tabs.onCreated)
// a moment ago, it never showed a page (`loaded`), and it still shows
// nothing or the download's own address. Such a tab is left blank once the
// download is taken, and IDM closes it.
function openedOnlyFor(tab, opened, urls, now) {
  if (!tab || !opened || opened.loaded || opened.opener == null) return false;
  if (now - opened.time > OPENED_FOR_DOWNLOAD_MS) return false;
  const at = [tab.url, tab.pendingUrl].filter(Boolean);
  return at.every((u) => u === "about:blank" || u === "about:newtab" || urls.includes(u));
}

// --- The browser's proxy -------------------------------------------------------

// A Chrome proxy server ({ scheme, host, port }) as `scheme://host:port`.
function chromeProxyUrl(p) {
  if (!p || !p.host) return undefined;
  const scheme = p.scheme || "http";
  const port = p.port || { http: 80, https: 443, socks4: 1080, socks5: 1080, quic: 443 }[scheme] || 80;
  const host = p.host.includes(":") && !p.host.startsWith("[") ? `[${p.host}]` : p.host;
  return `${scheme}://${host}:${port}`;
}

// A Firefox proxy ("host:port", maybe with a scheme) as `scheme://host:port`.
function firefoxProxyUrl(scheme, address, defaultPort) {
  const hostPort = String(address || "").trim().replace(/^[a-z0-9+.-]+:\/\//i, "").replace(/\/.*$/, "");
  const m = /^(\[[^\]]+\]|[^:\s]+)(?::(\d+))?$/.exec(hostPort);
  if (!m) return undefined;
  return `${scheme}://${m[1]}:${Number(m[2]) || defaultPort}`;
}

// The browser's proxy setting (chrome.proxy.settings' value, or Firefox's
// browser.proxy.settings') as UDM's `browser_proxy`: { mode, http, https,
// socks, pac_url, pac_script, bypass }, mode one of "direct", "fixed",
// "pac", "auto_detect" and "system". Undefined when it cannot be read.
function proxySetting(value) {
  if (!value) return undefined;
  const drop = (o) => {
    for (const k of Object.keys(o)) if (o[k] === undefined || (Array.isArray(o[k]) && !o[k].length)) delete o[k];
    return o;
  };
  if ("proxyType" in value) {
    // Firefox.
    const bypass = String(value.passthrough || "")
      .split(/[,\s]+/)
      .filter(Boolean);
    switch (value.proxyType) {
      case "none":
        return { mode: "direct" };
      case "system":
        return { mode: "system" };
      case "autoDetect":
        return { mode: "auto_detect" };
      case "autoConfig":
        return drop({ mode: "pac", pac_url: value.autoConfigUrl || undefined });
      case "manual": {
        const http = value.http ? firefoxProxyUrl("http", value.http, 80) : undefined;
        const ssl = value.httpProxyAll ? http : value.ssl ? firefoxProxyUrl("http", value.ssl, 80) : undefined;
        const socks = value.socks ? firefoxProxyUrl(value.socksVersion === 4 ? "socks4" : "socks5", value.socks, 1080) : undefined;
        return drop({ mode: "fixed", http, https: ssl, socks, bypass });
      }
      default:
        return undefined;
    }
  }
  switch (value.mode) {
    case "direct":
      return { mode: "direct" };
    case "system":
      return { mode: "system" };
    case "auto_detect":
      return { mode: "auto_detect" };
    case "pac_script": {
      const pac = value.pacScript || {};
      return drop({ mode: "pac", pac_url: pac.url || undefined, pac_script: pac.url ? undefined : pac.data || undefined });
    }
    case "fixed_servers": {
      const rules = value.rules || {};
      const single = chromeProxyUrl(rules.singleProxy);
      const fallback = chromeProxyUrl(rules.fallbackProxy);
      const socksOf = (u) => (u && u.startsWith("socks") ? u : undefined);
      // A SOCKS proxy for one scheme stays there; UDM sends that scheme
      // through it.
      return drop({
        mode: "fixed",
        http: single || chromeProxyUrl(rules.proxyForHttp) || fallback,
        https: single || chromeProxyUrl(rules.proxyForHttps) || fallback,
        socks: socksOf(single) || socksOf(fallback),
        bypass: Array.isArray(rules.bypassList) ? [...rules.bypassList] : undefined,
      });
    }
    default:
      return undefined;
  }
}

// --- The download bubble ---------------------------------------------------------

// Hides the browser's download bubble (Chrome's downloads.setUiOptions)
// while a download is taken from it, so it does not flash up for a file
// UDM downloads. `setEnabled(bool)` shows or hides it. Each hold() is
// matched by a release(); the bubble comes back when none are left, or
// after `holdMs` whatever happens, so it is never left hidden.
function bubbleGate(setEnabled, holdMs = 10000) {
  let holds = 0;
  let timer = null;
  const show = () => {
    clearTimeout(timer);
    timer = null;
    holds = 0;
    setEnabled(true);
  };
  return {
    hold() {
      if (holds++ === 0) setEnabled(false);
      clearTimeout(timer);
      timer = setTimeout(show, holdMs);
    },
    release() {
      if (holds === 0) return;
      if (--holds === 0) show();
    },
    get held() {
      return holds > 0;
    },
  };
}

if (typeof module !== "undefined") {
  module.exports = {
    downloadResponse,
    dispositionName,
    noteRequest,
    noteRedirect,
    noteResponse,
    findRequest,
    pruneRequests,
    requestDetails,
    requestReferer,
    webReferrer,
    frameReferrer,
    openedOnlyFor,
    proxySetting,
    bubbleGate,
    REQUEST_MATCH_MS,
  };
}
