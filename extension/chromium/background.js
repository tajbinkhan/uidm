// UDM service worker.
//
// Keeps one native-messaging port to the UDM host (which relays to the
// daemon), takes downloads away from the browser and hands them to UDM,
// adds a "Download with UDM" context menu, and serves the popup, which can
// also hand UDM a download the browser already started.
//
// A download is only taken from the browser while UDM is connected, capture
// is on, and it passes the capture rules from UDM's settings (see rules.js);
// otherwise the browser downloads it as usual. If UDM cannot even start a
// download it took, it is handed back to the browser. A download is seen
// when its response headers arrive: Firefox lets the worker cancel it then,
// Chrome only lets it look, so there it is taken as it appears, with the
// browser's whole request (see request.js) and its bubble kept hidden.
//
// It also notes the videos each tab loads (see media.js) for the video
// button that content.js shows over playing videos, and collects a page's
// links for the window's link picker.
//
// Safari has no downloads API, no proxy settings and no notifications, and
// reaches udm-host through UDM.app's app extension (see native.js). There
// the worker takes download links as they are clicked instead (see
// shareLinkCapture), and leaves out what needs the rest.

// Chrome runs this as a service worker and it loads its helpers itself;
// Firefox runs it as a background script after the helpers the manifest
// lists, and has no importScripts.
if (typeof importScripts === "function") importScripts("rules.js", "links.js", "post.js", "media.js", "request.js", "native.js");

const HOST = "com.udm.host";
const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 30000;

let port = null;
let connected = false; // true once the daemon has answered hello
let lastError = "";
let reconnectDelay = RECONNECT_MIN_MS;
let reconnectTimer = null;

// Mirror of the daemon's download list, by id.
const downloads = new Map();
// Downloads taken from the browser: url -> { post, filename } until their
// ack arrives (`post` is null for a GET, else the rebuilt POST), then
// id -> { url, post, filename } until they start, finish, or are handed back.
const awaitingAck = new Map();
const captured = new Map();

// Recent requests in frames and scripts that sent a body (form
// submissions, XMLHttpRequest and fetch), oldest first, with the Content-Type
// each sent. A download a form started is matched to its request here (see
// post.js).
const posts = [];
// The file fields of forms recently sent, as their pages reported them
// (content.js): { url, names, chosen, time }, oldest first.
const formFiles = [];
// The requests of frames: what the browser sent, where it was redirected
// and what came back, by request id (see request.js). A download is matched
// to its request here, for the whole request to go to UDM.
const requestLog = new Map();
// Tabs other tabs opened lately: tab id -> { time, opener, loaded }, for
// closing a tab that was opened only for a download UDM took.
const openedTabs = new Map();
// Private-window tabs. Firefox marks webRequest events from them, Chrome
// does not, so they are followed here: what happens in them stays out of
// UDM and out of the "recent videos" menus of normal windows, unless
// UDM's `capture_private` is on (see privateAllowed).
const privateTabs = new Set();
const isPrivate = (tabId) => privateTabs.has(tabId);

// Whether what `x` stands for may go to UDM: `x` is a tab, a webRequest
// event's details or a browser download, and one from a private window
// goes only with UDM's "Capture downloads in private windows too"
// (`capture_private`, off by default and before the settings arrive).
// Every check for a private window goes through here.
function privateAllowed(x) {
  const inPrivate = !!x && (!!x.incognito || isPrivate(x.tabId));
  return !inPrivate || !!(rules && rules.capture_private);
}
if (chrome.tabs) {
  const note = (tab) => tab && tab.incognito && privateTabs.add(tab.id);
  chrome.tabs.query({}).then((tabs) => tabs.forEach(note)).catch(() => {});
  chrome.tabs.onCreated.addListener(note);
  chrome.tabs.onUpdated.addListener((_id, _change, tab) => note(tab));
  chrome.tabs.onRemoved.addListener((id) => privateTabs.delete(id));
  chrome.tabs.onCreated.addListener((tab) => {
    if (!tab || tab.openerTabId == null) return;
    const now = Date.now();
    for (const [id, o] of openedTabs) if (now - o.time > 60000) openedTabs.delete(id);
    openedTabs.set(tab.id, { time: now, opener: tab.openerTabId, loaded: false });
  });
  // A tab that showed a page has history of its own: it stays.
  chrome.tabs.onUpdated.addListener((id, change, tab) => {
    const o = openedTabs.get(id);
    if (o && change.status === "complete" && tab && isWeb(tab.url)) o.loaded = true;
  });
  chrome.tabs.onRemoved.addListener((id) => openedTabs.delete(id));
}

// What the browser lets the worker see of requests. Chrome hides some
// headers (Cookie, Referer, ...) unless asked with "extraHeaders", an
// option Firefox does not know; Firefox can hold a response until the
// worker decides ("blocking", with the webRequestBlocking permission), which
// Chrome keeps from extensions like this one.
const hasOption = (options, name) => !!options && Object.values(options).includes(name);
const EXTRA_HEADERS =
  chrome.webRequest && hasOption(chrome.webRequest.OnSendHeadersOptions, "extraHeaders") ? ["extraHeaders"] : [];
const EXTRA_RESPONSE_HEADERS =
  chrome.webRequest && hasOption(chrome.webRequest.OnHeadersReceivedOptions, "extraHeaders") ? ["extraHeaders"] : [];
const BLOCKING = ((chrome.runtime.getManifest && chrome.runtime.getManifest().permissions) || []).includes("webRequestBlocking");

if (chrome.webRequest) {
  const frames = { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] };
  const isFrame = (d) => d.type === "main_frame" || d.type === "sub_frame";
  const bodies = { urls: ["<all_urls>"], types: ["main_frame", "sub_frame", "xmlhttprequest"] };
  chrome.webRequest.onBeforeRequest.addListener(
    (d) => {
      if (d.method !== "POST" || !privateAllowed(d)) return;
      const now = Date.now();
      prunePosts(posts, now);
      posts.push({ id: d.requestId, url: d.url, type: d.type, time: now, requestBody: d.requestBody });
    },
    bodies,
    ["requestBody"],
  );
  chrome.webRequest.onBeforeSendHeaders.addListener(
    (d) => {
      if (d.method !== "POST") return;
      const type = (d.requestHeaders || []).find((h) => h.name.toLowerCase() === "content-type");
      const record = posts.find((r) => r.id === d.requestId);
      if (record && type) record.contentType = type.value;
    },
    bodies,
    ["requestHeaders"],
  );
  // The headers as they left, after other extensions changed them.
  if (chrome.webRequest.onSendHeaders) {
    chrome.webRequest.onSendHeaders.addListener(
      (d) => {
        if (!isFrame(d) || !privateAllowed(d)) return;
        noteRequest(requestLog, d, Date.now());
      },
      frames,
      ["requestHeaders", ...EXTRA_HEADERS],
    );
  }
  if (chrome.webRequest.onBeforeRedirect) {
    chrome.webRequest.onBeforeRedirect.addListener((d) => isFrame(d) && noteRedirect(requestLog, d), frames);
  }
  chrome.webRequest.onHeadersReceived.addListener(
    (d) => {
      if (!isFrame(d) || !privateAllowed(d)) return undefined;
      const record = noteResponse(requestLog, d, Date.now());
      return record.response ? atHeaders(d, record) : undefined;
    },
    frames,
    ["responseHeaders", ...EXTRA_RESPONSE_HEADERS, ...(BLOCKING ? ["blocking"] : [])],
  );
}

// Video and audio each tab loaded: tab id -> Map(url -> entry). A tab's list
// starts over when it goes to another page.
const media = new Map();
// The web players' files each tab handed to UDM without the video button
// (`video_auto_capture`): tab id -> Set(url), so each goes once, and again
// only after the tab goes to another page.
const playerTaken = new Map();
// When each tab's players started loading or playing, as their content
// scripts say: tab id -> [{ frameId, element, time }] (see noteMediaStart),
// to tell which player asked for which stream.
const mediaStarts = new Map();
// What a single-page site loaded this long before it moved to another page
// is kept: the new page's player may have asked for its stream just before
// the browser said the page changed.
const PAGE_MOVE_GRACE_MS = 1000;

// Hand the file a web player loaded (webRequest details `d`) to UDM, as a
// click on it in the video button's menu would, but named by its address.
async function takeFromPlayer(d) {
  if (!playerTaken.has(d.tabId)) playerTaken.set(d.tabId, new Set());
  playerTaken.get(d.tabId).add(d.url);
  const download = { op: "add", url: d.url, user_agent: navigator.userAgent };
  // A player's script may ask for its file with a POST; it goes the same.
  if (d.method === "POST") {
    const posted = postById(posts, d.requestId);
    const post = posted ? rebuildPost(posted) : { leave: "no body" };
    if (post.leave) return;
    download.method = "POST";
    download.body = post.body;
    if (post.contentType) download.headers = { "Content-Type": post.contentType };
  }
  const cookies = await cookieHeader(d.url, d);
  if (cookies) download.cookies = cookies;
  const page = chrome.tabs && chrome.tabs.get ? await chrome.tabs.get(d.tabId).then((t) => t.url, () => null) : null;
  const from = webReferrer(d.documentUrl, d.originUrl, d.initiator, page);
  if (from) download.referrer = from;
  if (isWeb(page)) download.page = page;
  await addBrowserProxy(download);
  send(download);
}
// The headers players' requests sent, by request id, until their answers
// say whether they are media (see recordMedia).
const mediaRequests = new Map();
if (chrome.webRequest) {
  const players = { urls: ["<all_urls>"], types: ["media", "xmlhttprequest", "object", "other"] };
  if (chrome.webRequest.onSendHeaders) {
    chrome.webRequest.onSendHeaders.addListener(
      (d) => {
        if (d.tabId < 0 || !privateAllowed(d)) return;
        noteRequest(mediaRequests, d, Date.now());
      },
      players,
      ["requestHeaders", ...EXTRA_HEADERS],
    );
  }
  chrome.webRequest.onHeadersReceived.addListener(
    (d) => {
      if (d.tabId < 0 || !privateAllowed(d)) return;
      const found = classifyMedia(d, rules);
      if (found) {
        const sent = mediaRequests.get(d.requestId);
        recordMedia(media, d.tabId, d.url, found, Date.now(), d.frameId, sent && sent.headers);
        scheduleRecentMenu();
      }
      if (connected && captureEnabled && tabCaptures(d.tabId) && autoCaptureMedia(d, rules, playerTaken.get(d.tabId))) takeFromPlayer(d);
    },
    players,
    ["responseHeaders"],
  );
  chrome.webRequest.onBeforeRequest.addListener(
    (d) => {
      if (d.tabId >= 0) playerTaken.delete(d.tabId);
      if (d.tabId >= 0) mediaStarts.delete(d.tabId);
      if (d.tabId >= 0 && media.delete(d.tabId)) scheduleRecentMenu();
    },
    { urls: ["<all_urls>"], types: ["main_frame"] },
  );
}
if (chrome.tabs) {
  chrome.tabs.onRemoved.addListener((tabId) => {
    playerTaken.delete(tabId);
    mediaStarts.delete(tabId);
    if (media.delete(tabId)) scheduleRecentMenu();
  });
}

// A single-page site (a feed, YouTube) moving to another page without
// loading one: the tab's list starts over as on a page load, keeping only
// what came in just before (PAGE_MOVE_GRACE_MS), and its frames hide their
// video buttons, whose players belong to the old page.
function pageMoved(tabId) {
  const since = Date.now() - PAGE_MOVE_GRACE_MS;
  playerTaken.delete(tabId);
  const starts = (mediaStarts.get(tabId) || []).filter((s) => s.time >= since);
  if (starts.length) mediaStarts.set(tabId, starts);
  else mediaStarts.delete(tabId);
  if (forgetMedia(media, tabId, since)) scheduleRecentMenu();
  if (chrome.tabs && chrome.tabs.sendMessage) chrome.tabs.sendMessage(tabId, { type: "video:reset" }).catch(() => {});
}
if (chrome.webNavigation && chrome.webNavigation.onHistoryStateUpdated) {
  chrome.webNavigation.onHistoryStateUpdated.addListener((d) => {
    if (d.frameId !== 0 || d.tabId < 0) return;
    pageMoved(d.tabId);
    // The page moved to may be a video yt-dlp reads (YouTube, Facebook,
    // Instagram, Vimeo), which tabs.onUpdated may have told before this.
    const page = d.url && privateAllowed({ tabId: d.tabId }) && videoPageUrl(d.url);
    if (page) {
      recordPage(media, d.tabId, page, Date.now());
      scheduleRecentMenu();
    }
  });
}
// A video on a site yt-dlp reads, which moves between videos without
// loading a page.
if (chrome.tabs) {
  chrome.tabs.onUpdated.addListener((tabId, change, tab) => {
    const page = change.url && privateAllowed(tab) && videoPageUrl(change.url);
    if (page) {
      recordPage(media, tabId, page, Date.now());
      scheduleRecentMenu();
    }
  });
}

// What UDM said each stream offers, by URL, and the probes waiting for an
// answer, by request id.
const mediaInfos = new Map();
const probes = new Map();
let nextProbe = 1;
const PROBE_TIMEOUT_MS = 20000;
// yt-dlp takes longer: it reads the page and the player's code first.
const PAGE_PROBE_TIMEOUT_MS = 60000;

// Pages yt-dlp found nothing on, by address, with when: they are not asked
// about again for PAGE_MISS_MS, so opening the menu again starts no new
// yt-dlp. And the probes still waiting, by address, so two clicks share one.
const pageMisses = new Map();
const PAGE_MISS_MS = 10 * 60 * 1000;
const probesOut = new Map();

// Ask UDM what the stream at `url` offers, sending the page's cookies
// along, and `sent`, what the player's own request carried (mediaRequest).
// `page` says it is a web page for yt-dlp to read, whatever its site.
// Resolves to a MediaInfo, or null without an answer.
function probeStream(url, referrer, timeout = PROBE_TIMEOUT_MS, from = null, sent = {}, page = false) {
  if (mediaInfos.has(url)) return Promise.resolve(mediaInfos.get(url));
  if (!probesOut.has(url)) {
    const asked = askProbe(url, referrer, timeout, from, sent, page).finally(() => probesOut.delete(url));
    probesOut.set(url, asked);
  }
  return probesOut.get(url);
}

async function askProbe(url, referrer, timeout, from, sent, page) {
  const request = nextProbe++;
  const download = { url, user_agent: navigator.userAgent };
  if (page) download.media = { kind: "page" };
  const cookies = await cookieHeader(url, from);
  if (cookies) download.cookies = cookies;
  if (sent.referrer || referrer) download.referrer = sent.referrer || referrer;
  if (sent.request_headers) download.request_headers = sent.request_headers;
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      probes.delete(request);
      resolve(null);
    }, timeout);
    probes.set(request, (info) => {
      clearTimeout(timer);
      // Failures are not kept: the next click asks again.
      if (!info.error || info.drm) mediaInfos.set(url, info);
      resolve(info);
    });
    if (!send({ op: "probe_media", request, download })) {
      clearTimeout(timer);
      probes.delete(request);
      resolve(null);
    }
  });
}

// The video button's menu for tab `tabId` (`tab`), for the player `player`
// ({ frameId, element, srcs }) whose button was clicked: its own streams
// when the worker can tell which they are (elementEntries), else all the
// tab's. Pages in a private window are recorded only when they may go to
// UDM, so otherwise they stay out of the "last videos" menus.
async function videoMenu(tabId, referrer, tab, player = {}) {
  const page = privateAllowed(tab) && videoPageUrl(referrer);
  if (page) recordPage(media, tabId, page, Date.now());
  if (privateAllowed(tab)) await otherPages(tabId, referrer, tab, !page);
  const entries = elementEntries(
    [...(media.get(tabId) || new Map()).values()],
    mediaStarts.get(tabId),
    player.frameId,
    player.element,
    player.srcs,
  );
  const streams = entries.filter((e) => e.kind !== "file");
  const infos = new Map();
  await Promise.all(
    streams.map(async (e) => {
      const timeout = e.kind === "page" ? PAGE_PROBE_TIMEOUT_MS : PROBE_TIMEOUT_MS;
      const info = await probeStream(e.url, referrer, timeout, tabContext(tab), mediaRequest(e), e.kind === "page");
      if (info && !adoptFile(e, info)) infos.set(e.url, info);
    }),
  );
  return mediaMenu(entries, infos);
}

// Pages yt-dlp may read that the browser saw no stream on, asked about
// only when the video button's menu opens (never on a page load, so no
// yt-dlp runs for pages nobody asked about): the playlist a YouTube video
// is part of, and, when `any` and the tab has nothing else to offer, the
// page itself. Each is kept in the tab's list only when yt-dlp finds a
// video or a playlist there; a miss is not asked about again for a while.
async function otherPages(tabId, referrer, tab, any) {
  const known = media.get(tabId) || new Map();
  const wanted = [];
  const list = playlistPageUrl(referrer);
  if (list && !known.has(list)) wanted.push({ url: list, site: "playlist" });
  const other = any && known.size === 0 && otherVideoPage(referrer);
  if (other && !known.has(other)) wanted.push({ url: other, site: "other" });
  const now = Date.now();
  await Promise.all(
    wanted
      .filter((w) => !(pageMisses.get(w.url) > now - PAGE_MISS_MS))
      .map(async (w) => {
        const info = await probeStream(w.url, referrer, PAGE_PROBE_TIMEOUT_MS, tabContext(tab), {}, true);
        const found = info && !info.error && ((info.video || []).length || (info.audio || []).length || (info.playlist || {}).entries);
        if (!found) {
          pageMisses.set(w.url, Date.now());
          return;
        }
        const entry = recordMedia(media, tabId, w.url, { kind: "page", mime: "", size: 0 }, Date.now());
        entry.site = w.site;
        scheduleRecentMenu();
      }),
  );
}

// Send the playlist UDM found at media entry `entryId` of tab `tabId`
// to UDM's window, which lists its videos to pick from.
function sendPlaylist(tabId, entryId) {
  const entry = [...(media.get(tabId) || new Map()).values()].find((e) => e.id === entryId && e.kind === "page");
  const playlist = entry && (mediaInfos.get(entry.url) || {}).playlist;
  if (!playlist || !connected) return false;
  const entries = (playlist.entries || [])
    .filter((v) => v && isWeb(v.url))
    .slice(0, 1000)
    .map((v) => ({ url: v.url, title: String(v.title || "").slice(0, 500), ...(typeof v.duration === "number" ? { duration: v.duration } : {}) }));
  return send({ op: "pick_videos", page: entry.url, playlist: { title: String(playlist.title || "").slice(0, 500), entries }, user_agent: navigator.userAgent });
}

// What the top frame of tab `tabId` calls its page (pageNames, in
// content.js), for a video downloaded from one of its frames, which cannot
// read the top page itself. Without an answer, the tab's title.
async function topPageNames(tabId, tab) {
  try {
    const names = await chrome.tabs.sendMessage(tabId, { type: "page:names" }, { frameId: 0 });
    if (names && typeof names.title === "string") return names;
  } catch (_) {}
  return { title: (tab && tab.title) || "" };
}

// Send media entry `entryId` of tab `tabId` to UDM, in the chosen quality,
// named after its page (`msg.title`, `msg.ogTitle`, `msg.siteName`; see
// videoName). `referrer` is the document that played it, `page` the tab's
// own address.
async function downloadMedia(tabId, msg, referrer, page = null) {
  const entry = [...(media.get(tabId) || new Map()).values()].find((e) => e.id === msg.entry);
  if (!entry || !connected) return false;
  const download = { op: "add", url: entry.url, user_agent: navigator.userAgent };
  const cookies = await cookieHeader(entry.url, { tabId, incognito: isPrivate(tabId) });
  if (cookies) download.cookies = cookies;
  const sent = mediaRequest(entry);
  if (sent.referrer || referrer) download.referrer = sent.referrer || referrer;
  if (sent.request_headers) download.request_headers = sent.request_headers;
  if (isWeb(page)) download.page = page;
  await addBrowserProxy(download);
  const names = { title: msg.title, ogTitle: msg.ogTitle, siteName: msg.siteName };
  // YouTube's count of new notifications goes before its title.
  if (entry.kind === "page") names.title = pageTitle(names.title);
  const title = videoName(names, page || referrer);
  if (entry.kind === "file") {
    download.filename = titleFileName(title, mediaFileExtension(entry));
  } else {
    const soundOnly = SOUND_FORMATS.includes(msg.audioOnly) ? msg.audioOnly : null;
    download.filename = titleFileName(title, soundOnly || "mp4");
    download.media = { kind: entry.kind };
    if (typeof msg.video === "string") download.media.video = msg.video;
    if (typeof msg.audio === "string") download.media.audio = msg.audio;
    if (soundOnly) download.media.audio_only = soundOnly;
    // Subtitle ids come from what UDM offered; UDM fetches only tracks
    // the stream itself lists.
    if (Array.isArray(msg.subtitles) && !soundOnly) {
      download.media.subtitles = msg.subtitles.filter((id) => typeof id === "string").slice(0, 64);
    }
  }
  if (!download.filename) delete download.filename;
  return send(download);
}

// A content script's request. Pages are untrusted, so a content script can
// only ask about its own tab and choose among the media the worker saw.
function onContentMessage(msg, sender, sendResponse) {
  // Nothing from a private window goes to UDM unless UDM's settings let
  // it: its buttons stay off and a forced link opens in the browser.
  if (!privateAllowed(sender.tab)) {
    sendResponse({ enabled: false, connected: false, ok: false, items: [] });
    return false;
  }
  const tabId = sender.tab.id;
  const pageUrl = sender.url || sender.tab.url || "";
  switch (msg && msg.type) {
    case "video:enabled":
      sendResponse({
        enabled: videoButtonAllowed(rules, pageUrl),
        view: (rules && rules.video_view) || "full",
        protectedNote: !rules || rules.video_protected_note !== false,
      });
      return false;
    case "video:menu":
      if (!connected) {
        sendResponse({ connected: false, items: [] });
        return false;
      }
      videoMenu(tabId, pageUrl, sender.tab, {
        frameId: sender.frameId || 0,
        element: Number.isInteger(msg.element) ? msg.element : null,
        srcs: Array.isArray(msg.srcs) ? msg.srcs.filter((u) => typeof u === "string").slice(0, 20) : [],
      }).then((items) => sendResponse({ connected: true, items }));
      return true;
    case "video:playlist":
      // The menu's "Choose videos": the window lists the playlist.
      sendResponse({ ok: Number.isInteger(msg.entry) && sendPlaylist(tabId, msg.entry) });
      return false;
    case "video:started":
      // A player of this frame started loading or playing (see content.js).
      if (Number.isInteger(msg.element)) noteMediaStart(mediaStarts, tabId, sender.frameId || 0, msg.element, Date.now());
      sendResponse({ ok: true });
      return false;
    case "video:download": {
      const text = (v) => (typeof v === "string" ? v.slice(0, 1000) : "");
      const own = { title: text(msg.title), ogTitle: text(msg.ogTitle), siteName: text(msg.siteName) };
      // A frame's own page is not what the video is called: the tab's is.
      const names = sender.frameId ? topPageNames(tabId, sender.tab) : Promise.resolve(own);
      names
        .then((n) => downloadMedia(tabId, { ...msg, ...n }, pageUrl, sender.tab.url))
        .then((ok) => sendResponse({ ok }));
      return true;
    }
    case "selection:enabled":
      sendResponse({ enabled: connected && selectionButtonAllowed(rules, pageUrl), view: (rules && rules.video_view) || "full" });
      return false;
    case "selection:links":
      // Only the selection's links, from every frame of this tab.
      pickLinks(tabId, sender.tab.url || pageUrl, true).then((result) => sendResponse({ ok: noticePick(result, true) }));
      return true;
    case "form:files":
      // The file fields of a form this page is sending (see content.js).
      noteFormFiles(msg);
      sendResponse({ ok: true });
      return false;
    case "link:skip":
      // The skip key was held: the download this click starts stays.
      skipClick = { time: Date.now() };
      sendResponse({ ok: true });
      return false;
    case "keys:held":
      // The skip or force keys went down or up in this tab (see heldReport).
      noteHeldKeys(tabId, msg.action);
      sendResponse({ ok: true });
      return false;
    case "link:capture":
      // A click on a link to a file, where the browser has no downloads API
      // (see shareLinkCapture).
      if (chrome.downloads) {
        sendResponse({ ok: false });
        return false;
      }
      captureClickedLink(msg.url, linkFileName(msg.download), sender).then((ok) => sendResponse({ ok: !!ok }));
      return true;
    case "link:magnet":
      // A magnet link clicked (see content.js): UDM downloads its torrent,
      // unless capture is off, everywhere or in this tab.
      if (!connected || !captureEnabled || !tabCaptures(tabId) || !isMagnet(msg.url)) {
        sendResponse({ ok: false });
        return false;
      }
      sendResponse({ ok: send(isWeb(sender.tab.url) ? { op: "add", url: msg.url, page: sender.tab.url } : { op: "add", url: msg.url }) });
      return false;
    case "link:force":
      // The force key was held: the link goes to UDM whatever it is,
      // unless capture is off, everywhere or in this tab.
      if (!connected || !captureEnabled || !tabCaptures(tabId) || !isWeb(msg.url)) {
        sendResponse({ ok: false });
        return false;
      }
      // The clicked link's document (maybe a frame) is the referrer, or the
      // nearest one with an address when it has none (about:blank); the
      // tab's address is the page it came from.
      (isWeb(pageUrl) ? Promise.resolve(undefined) : framesReferrer(tabId, sender.frameId))
        .then((parent) =>
          handOff(msg.url, webReferrer(pageUrl, parent, sender.tab.url), undefined, false, null, null, sender.tab.url, null, {}, tabContext(sender.tab)),
        )
        .then((ok) => sendResponse({ ok }));
      return true;
    default:
      return false;
  }
}

let captureEnabled = true;
// UDM's settings, which carry the capture rules; null until they arrive.
let rules = null;
// "Skip next download" in the popup: the next download stays in the browser.
let skipNext = false;
// The last link click with the skip key held, as { time }.
let skipClick = null;
// A download in UDM waiting for a new link: the next download from the
// browser gives it one instead of starting a new download.
let refreshWaiting = null;
const settingsReady = chrome.storage.local.get({ capture: true, takeoverNotice: true }).then((s) => {
  captureEnabled = s.capture;
  takeoverNotice = s.takeoverNotice;
  shareLinkCapture();
});
chrome.storage.onChanged.addListener((changes) => {
  if (changes.capture) captureEnabled = changes.capture.newValue;
  if (changes.takeoverNotice) takeoverNotice = changes.takeoverNotice.newValue;
  if (changes.capture) shareLinkCapture();
});

// --- Links taken as they are clicked (Safari) ----------------------------------

// Safari tells extensions nothing of its downloads, so there content.js
// takes a link to a file as it is clicked (clickTaken, in rules.js) and asks
// the worker to hand it over ("link:capture"). It must decide at once,
// before the browser follows the link, so it reads whether to and the
// capture rules from storage, as it reads the skip and force keys: this
// keeps them there, as `linkCapture`. Browsers with the downloads API never
// get it, so their content scripts take no click.
// Nothing is taken until UDM is connected and its rules are here: before
// them a link's type cannot be checked.
let sharedLinkCapture = null;
let sharedMagnetCapture = null;
function shareLinkCapture() {
  if (!chrome.storage || !chrome.storage.local) return;
  // Every browser: whether magnet links go to UDM as they are clicked.
  const magnets = connected && !!captureEnabled;
  if (magnets !== sharedMagnetCapture) {
    sharedMagnetCapture = magnets;
    Promise.resolve(chrome.storage.local.set({ magnetCapture: magnets })).catch(() => {});
  }
  if (chrome.downloads) return;
  const value = {
    on: connected && !!captureEnabled && !!rules,
    rules: rules
      ? {
          capture_types: rules.capture_types || [],
          capture_excluded_sites: rules.capture_excluded_sites || [],
          capture_excluded_urls: rules.capture_excluded_urls || [],
          torrents: { catch_torrent_files: catchesTorrents(rules) },
        }
      : null,
  };
  const json = JSON.stringify(value);
  if (json === sharedLinkCapture) return;
  sharedLinkCapture = json;
  Promise.resolve(chrome.storage.local.set({ linkCapture: value })).catch(() => {});
}
// A worker that starts again (Safari stops an idle one) is not connected
// yet: what the last one shared goes at once.
shareLinkCapture();

// How long a click that woke the worker waits for UDM's hello and rules.
const CLICK_CONNECT_WAIT_MS = 3000;

// Resolves once UDM is connected and its rules are here, or after `ms`.
function connectedWithRules(ms) {
  return new Promise((resolve) => {
    const until = Date.now() + ms;
    const check = () => {
      if ((connected && rules) || Date.now() >= until) resolve();
      else setTimeout(check, 100);
    };
    check();
  });
}

// Hand the link a page's click asked for (`url`, named by its `download`
// attribute `filename`) to UDM, when the capture rules and the popup's
// switches take it now. Says whether UDM has it; content.js lets the browser
// follow the link when not.
async function captureClickedLink(url, filename, sender) {
  const tabId = sender.tab.id;
  const pageUrl = sender.url || sender.tab.url || "";
  if (!isWeb(url)) return false;
  // The click may have woken the worker, which has to connect again.
  await Promise.all([settingsReady, tabsOffReady, connectedWithRules(CLICK_CONNECT_WAIT_MS)]);
  if (!rules || !clickTaken(rules, url, filename, pageUrl)) return false;
  // "Skip next download" keeps this one in the browser.
  if (skipNext) {
    skipNext = false;
    return false;
  }
  // The clicked link's document (maybe a frame) is the referrer, or the
  // nearest one with an address when it has none (about:blank), as for a
  // forced link.
  const parent = isWeb(pageUrl) ? undefined : await framesReferrer(tabId, sender.frameId);
  const referrer = webReferrer(pageUrl, parent, sender.tab.url) || "";
  const described = { url, referrer, filename: filename || "", mime: "", size: 0, key: heldKey(tabId) };
  const verdict = takesNow(described, tabId);
  if (!verdict) return false;
  if (verdict.refreshId !== null) {
    refreshWaiting = null;
    return handOff(url, referrer, filename, false, null, verdict.refreshId, sender.tab.url, null, {}, tabContext(sender.tab));
  }
  // `clicked`: should the connection drop before UDM has it, it goes back
  // to the browser (see onDisconnect).
  awaitingAck.set(url, { post: null, filename, clicked: true });
  const sent = await handOff(url, referrer, filename, false, null, null, sender.tab.url, null, {}, tabContext(sender.tab));
  if (!sent) awaitingAck.delete(url);
  return sent;
}

// --- Capture off in one tab ----------------------------------------------------

// The tabs capture is turned off in, from the popup's "Capture in this tab"
// (IDM's toolbar button does the same): what their pages download, forced
// links and web players' files stay with the browser. They are kept in
// session storage, which outlives the worker going to sleep but not the
// browser; older Firefox has none, and then only this map keeps them. A
// tab's entry goes when the tab closes.
const tabsOff = new Set();
const sessionStore = chrome.storage && chrome.storage.session;
// Settled once the stored tabs are back, so a download that wakes the
// worker is not checked against an empty set.
const tabsOffReady = sessionStore
  ? sessionStore
      .get({ tabsOff: [] })
      .then((s) => (s.tabsOff || []).forEach((id) => Number.isInteger(id) && tabsOff.add(id)))
      .catch(() => {})
  : Promise.resolve();
function saveTabsOff() {
  if (sessionStore) sessionStore.set({ tabsOff: [...tabsOff] }).catch(() => {});
}

// Whether capture is on in tab `tabId`; a download from no known tab counts
// as on.
function tabCaptures(tabId) {
  return tabId == null || tabId < 0 || !tabsOff.has(tabId);
}

// Turn capture in tab `tabId` on or off, and show it on its toolbar icon.
function setTabCapture(tabId, on) {
  if (on) tabsOff.delete(tabId);
  else tabsOff.add(tabId);
  saveTabsOff();
  showTabCapture(tabId);
}

// The toolbar icon of a tab with capture off is grey with an "X" badge; one
// with it on has the usual icon and the badge every tab has (the count of
// downloads in progress, see updateBadge). Firefox forgets a tab's icon and
// badge when it loads another page, so this runs again then.
async function showTabCapture(tabId) {
  if (!chrome.action) return;
  const off = tabsOff.has(tabId);
  const done = (p) => p && p.catch && p.catch(() => {});
  try {
    // null drops the tab's own text, so the tab shows the shared one.
    done(chrome.action.setBadgeText({ tabId, text: off ? "X" : null }));
    if (!chrome.action.setIcon) return;
    const grey = off ? await greyIcons() : null;
    if (grey) done(chrome.action.setIcon({ tabId, imageData: grey }));
    else if (!off) done(chrome.action.setIcon({ tabId, path: { 16: "icons/16.png", 32: "icons/32.png" } }));
  } catch (_) {
    // The tab closed meanwhile.
  }
}

// UDM's icon in grey, made once from its own icons (OffscreenCanvas, in
// Chrome's worker and Firefox's background page alike): { 16, 32 } as
// ImageData, or null where the browser cannot draw it; the "X" badge still
// shows then.
let greyIconsMade = null;
function greyIcons() {
  if (!greyIconsMade) greyIconsMade = makeGreyIcons().catch(() => null);
  return greyIconsMade;
}
async function makeGreyIcons() {
  if (typeof OffscreenCanvas !== "function" || typeof createImageBitmap !== "function" || typeof fetch !== "function") return null;
  const icons = {};
  for (const size of [16, 32]) {
    const blob = await (await fetch(chrome.runtime.getURL(`icons/${size}.png`))).blob();
    const bitmap = await createImageBitmap(blob);
    const canvas = new OffscreenCanvas(size, size);
    const context = canvas.getContext("2d");
    context.drawImage(bitmap, 0, 0, size, size);
    const image = context.getImageData(0, 0, size, size);
    greyPixels(image.data);
    icons[size] = image;
  }
  return icons;
}

if (chrome.tabs) {
  chrome.tabs.onRemoved.addListener((tabId) => {
    if (tabsOff.delete(tabId)) saveTabsOff();
  });
  chrome.tabs.onUpdated.addListener((tabId, change) => {
    if (change.status === "loading" && tabsOff.has(tabId)) showTabCapture(tabId);
  });
}

// --- Keys held down ---------------------------------------------------------------

// What the skip or force keys held down in each tab ask for, as its content
// script reported: tab id -> { action, time } (see heldReport). A download
// the tab starts meanwhile follows it. Switching tabs forgets them all: the
// keys may be let go in the other tab, which does not tell this one.
const heldKeys = new Map();
function noteHeldKeys(tabId, action) {
  if (action === "skip" || action === "force") heldKeys.set(tabId, { action, time: Date.now() });
  else heldKeys.delete(tabId);
}
// The held keys' action in tab `tabId` now: "skip", "force" or "".
function heldKey(tabId) {
  const entry = heldKeys.get(tabId);
  if (entry && !heldApplies(entry, Date.now())) heldKeys.delete(tabId);
  return heldApplies(entry, Date.now()) ? entry.action : "";
}
if (chrome.tabs) {
  if (chrome.tabs.onActivated) chrome.tabs.onActivated.addListener(() => heldKeys.clear());
  chrome.tabs.onRemoved.addListener((tabId) => heldKeys.delete(tabId));
}

// The tab a browser download came from: its request's, when the worker saw
// it, else the tab in front, where the user just did something.
async function downloadTabId(record) {
  if (record && record.tabId != null && record.tabId >= 0) return record.tabId;
  if (!chrome.tabs || !chrome.tabs.query) return undefined;
  try {
    const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
    return tab ? tab.id : undefined;
  } catch (_) {
    return undefined;
  }
}

// --- Connection to the host -------------------------------------------------

// Safari's extension pages are safari-web-extension:// ones. Its native
// messages go to UDM.app's app extension, which cannot keep a port open, so
// the worker polls it (pollingPort, in native.js). Safari has connectNative
// too, so only the address tells it apart.
const IS_SAFARI = String(chrome.runtime.getURL("")).startsWith("safari-web-extension:");

// A new port to the host: connectNative's, or in Safari a polling one.
// Safari's `browser` namespace is the one sure to answer with a promise.
function openPort() {
  if (!IS_SAFARI) return chrome.runtime.connectNative(HOST);
  const own = globalThis.browser && globalThis.browser.runtime && globalThis.browser.runtime.sendNativeMessage ? globalThis.browser : chrome;
  return pollingPort((request) => own.runtime.sendNativeMessage(HOST, request));
}

function connect() {
  clearTimeout(reconnectTimer);
  reconnectTimer = null;
  try {
    port = openPort();
  } catch (e) {
    onDisconnect(e.message);
    return;
  }
  port.onMessage.addListener(onHostMessage);
  // A polling port says why it closed itself (udmReason).
  port.onDisconnect.addListener((closed) => {
    onDisconnect(chrome.runtime.lastError?.message || (closed && closed.udmReason) || chrome.i18n.getMessage("udmDisconnected"));
  });
  port.postMessage({ op: "hello", version: chrome.runtime.getManifest().version });
  port.postMessage({ op: "list" });
  port.postMessage({ op: "get_settings" });
}

function onDisconnect(reason) {
  port = null;
  connected = false;
  lastError = reason;
  updateBadge();
  shareLinkCapture();
  // Links taken as they were clicked (Safari) that UDM never acknowledged
  // may never have reached it: the browser gets them back.
  for (const [url, taken] of awaitingAck) {
    if (!taken.clicked) continue;
    awaitingAck.delete(url);
    giveBack(url, null);
  }
  if (!reconnectTimer) {
    reconnectTimer = setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, RECONNECT_MAX_MS);
  }
}

function send(msg) {
  if (!port) return false;
  try {
    port.postMessage(msg);
    return true;
  } catch (e) {
    // Kept for the "cannot reach UDM" notice.
    if (e && e.message) lastError = e.message;
    return false;
  }
}

function upsert(id, url) {
  let e = downloads.get(id);
  if (!e) {
    e = { id, url, path: null, total: null, done: 0, status: "queued", error: null };
    downloads.set(id, e);
  }
  return e;
}

const TERMINAL = new Set(["done", "failed", "canceled", "interrupted", "waiting"]);

function onHostMessage(msg) {
  switch (msg.op) {
    case "hello":
      connected = true;
      lastError = "";
      reconnectDelay = RECONNECT_MIN_MS;
      injectOnConnect();
      break;
    case "ack":
      if (awaitingAck.has(msg.url)) {
        captured.set(msg.id, { url: msg.url, ...awaitingAck.get(msg.url) });
        awaitingAck.delete(msg.url);
      }
      break;
    case "downloads":
      downloads.clear();
      for (const e of msg.items) downloads.set(e.id, e);
      break;
    case "added":
      upsert(msg.id, msg.url);
      break;
    case "started": {
      const e = upsert(msg.id, msg.url);
      Object.assign(e, { status: "downloading", path: msg.path, total: msg.total, error: null });
      // It started, so it is UDM's for good: no fallback from here on.
      captured.delete(msg.id);
      break;
    }
    case "progress": {
      const e = upsert(msg.id, msg.url);
      if (!TERMINAL.has(e.status)) e.status = "downloading";
      e.done = msg.done;
      if (msg.total != null) e.total = msg.total;
      break;
    }
    case "done": {
      const e = upsert(msg.id, msg.url);
      Object.assign(e, { status: "done", path: msg.path, error: null });
      if (e.total != null) e.done = e.total;
      captured.delete(msg.id);
      break;
    }
    case "failed": {
      const e = upsert(msg.id, msg.url);
      Object.assign(e, { status: "failed", error: msg.error });
      handBackIfNeverStarted(msg.id, msg.error);
      break;
    }
    case "canceled":
      upsert(msg.id, msg.url).status = "canceled";
      captured.delete(msg.id);
      break;
    case "removed":
      downloads.delete(msg.id);
      break;
    case "name_taken": {
      // UDM asks in its window whether to rename or overwrite; it keeps the
      // download either way, so no hand-back.
      const e = upsert(msg.id, msg.url);
      Object.assign(e, { status: "confirming", path: msg.path, total: msg.total, error: null });
      captured.delete(msg.id);
      break;
    }
    case "updated":
      downloads.set(msg.item.id, msg.item);
      // Kept for later: it is UDM's now, so no hand-back.
      if (msg.item.status === "waiting") captured.delete(msg.item.id);
      break;
    case "settings":
      rules = msg.settings;
      // Content scripts read the keys from storage, so a click can be
      // handled at once without asking the worker.
      chrome.storage.local.set({
        captureKeys: {
          capture_skip_key: rules.capture_skip_key || "none",
          capture_force_key: rules.capture_force_key || "none",
          capture_keys_left_click: !!rules.capture_keys_left_click,
          capture_force_skips_pages: rules.capture_force_skips_pages !== false,
        },
      });
      if (JSON.stringify(menuHidden()) !== menuBuiltFor) buildMenus();
      break;
    case "refresh_waiting":
      refreshWaiting = msg.id ?? null;
      break;
    case "render_page":
      renderPage(msg.url).then((page) =>
        send({
          op: "rendered",
          request: msg.request,
          html: page ? page.html : null,
          url: page ? page.url : null,
          popups: page ? page.popups : [],
        }),
      );
      break;
    case "cookies_wanted":
      // A site crawl in UDM signs in with the browser's cookies.
      if (isWeb(msg.url)) {
        cookieHeader(msg.url).then((cookies) => send({ op: "cookies", request: msg.request, cookies: cookies || null }));
      }
      break;
    case "media_info": {
      const done = probes.get(msg.request);
      probes.delete(msg.request);
      if (done) done(msg.info || {});
      break;
    }
    case "error":
      lastError = msg.message;
      break;
    case "rejected": {
      // UDM refused a download the browser already let go of: give it back.
      lastError = msg.message;
      const taken = awaitingAck.get(msg.url);
      if (!taken) break;
      awaitingAck.delete(msg.url);
      giveBack(msg.url, taken.post);
      noticeTakeoverFailed(msg.url, taken.filename, msg.message);
      break;
    }
  }
  updateBadge();
  shareLinkCapture();
}

// A download UDM took but could not even start (the server refused it, say)
// goes back to the browser so the user does not lose it. `error` is why.
function handBackIfNeverStarted(id, error) {
  const taken = captured.get(id);
  if (!taken) return;
  captured.delete(id);
  send({ op: "remove", id });
  giveBack(taken.url, taken.post);
  noticeTakeoverFailed(taken.url, taken.filename, error || "?");
}

// --- The notice that UDM could not take a download ---------------------------

const TAKEOVER_NOTICE = "udm-takeover";
// Whether the notice shows; its "Don't show again" turns it off for good.
let takeoverNotice = true;
// Whether the last notice had that button; Firefox's notices have none, so
// there a click on the notice itself turns them off.
let noticeHasButton = false;

// Tell the user UDM could not take the download of `url` (named
// `filename`), which the browser keeps or gets back. `reason` is UDM's
// error, or null when UDM is not connected. One notice replaces the last.
async function noticeTakeoverFailed(url, filename, reason) {
  if (!takeoverNotice || !chrome.notifications) return;
  const file = filename || url;
  const message = reason ? chrome.i18n.getMessage("takeoverError", [file, reason]) : chrome.i18n.getMessage("takeoverNotConnected", file);
  const options = {
    type: "basic",
    iconUrl: chrome.runtime.getURL("icons/48.png"),
    title: chrome.i18n.getMessage("takeoverFailed"),
    message,
  };
  try {
    await chrome.notifications.create(TAKEOVER_NOTICE, { ...options, buttons: [{ title: chrome.i18n.getMessage("dontShowAgain") }] });
    noticeHasButton = true;
  } catch (_) {
    noticeHasButton = false;
    const hint = chrome.i18n.getMessage("takeoverClickToStop");
    await chrome.notifications.create(TAKEOVER_NOTICE, { ...options, message: `${message} ${hint}` }).catch(() => {});
  }
}

function stopTakeoverNotices() {
  takeoverNotice = false;
  chrome.storage.local.set({ takeoverNotice: false });
  chrome.notifications.clear(TAKEOVER_NOTICE).catch(() => {});
}

if (chrome.notifications) {
  chrome.notifications.onButtonClicked.addListener((id) => id === TAKEOVER_NOTICE && stopTakeoverNotices());
  chrome.notifications.onClicked.addListener((id) => {
    if (id !== TAKEOVER_NOTICE) return;
    if (noticeHasButton) chrome.notifications.clear(id).catch(() => {});
    else stopTakeoverNotices();
  });
}

// --- Other notices -------------------------------------------------------------

const MESSAGE_NOTICE = "udm-message";

// A short notice from UDM, for a menu item or button that could not do what
// it was asked. One notice replaces the last.
function notice(message) {
  if (!chrome.notifications) return;
  chrome.notifications
    .create(MESSAGE_NOTICE, { type: "basic", iconUrl: chrome.runtime.getURL("icons/48.png"), title: "UDM", message })
    .catch(() => {});
}

// UDM could not be reached: not connected, or the message to the host did
// not go. Says why when the browser did (the port's runtime.lastError).
// `fallback` adds that the browser downloads it instead.
function noticeCannotReach(fallback = false) {
  let message = lastError ? chrome.i18n.getMessage("cannotReachUdmReason", lastError) : chrome.i18n.getMessage("cannotReachUdm");
  if (fallback) message += ` ${chrome.i18n.getMessage("browserDownloadsInstead")}`;
  notice(message);
}

// Let the browser download `url` after all. Without the downloads API
// (Safari) it opens in a background tab, which downloads it.
function browserGets(url) {
  if (chrome.downloads) chrome.downloads.download({ url }).catch(() => {});
  else if (chrome.tabs && chrome.tabs.create) chrome.tabs.create({ url, active: false }).catch(() => {});
}

// Let the browser download it after all, the way it was first requested.
function giveBack(url, post) {
  if (!post) {
    browserGets(url);
    return;
  }
  // The browser can only resend a text body, and only with the downloads API.
  if (post.text === undefined || !chrome.downloads) return;
  const options = { url, method: "POST", body: post.text };
  if (post.contentType) options.headers = [{ name: "Content-Type", value: post.contentType }];
  chrome.downloads.download(options).catch(() => {});
}

function updateBadge() {
  let active = 0;
  for (const e of downloads.values()) if (!TERMINAL.has(e.status)) active++;
  chrome.action.setBadgeText({ text: active ? String(active) : "" });
  chrome.action.setBadgeBackgroundColor({ color: connected ? "#2563eb" : "#9ca3af" });
}

// --- Capture ----------------------------------------------------------------

const isWeb = (url) => /^https?:\/\//i.test(url || "");

// The cookies the browser would send to `url`, as a Cookie header. `from`
// is where the download comes from, { tabId, incognito, cookieStoreId }:
// a private window's come from its own cookie store, not the normal one.
async function cookieHeader(url, from = null) {
  try {
    const storeId = await privateCookieStore(from);
    const cookies = await chrome.cookies.getAll(storeId ? { url, storeId } : { url });
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch (_) {
    return "";
  }
}

// The cookie store of a private window's tab or download `from`, or
// undefined for a normal one (the default store). Firefox names it on the
// request, the download and the tab (`cookieStoreId`); Chrome has to be
// asked which store holds the tab, and calls its private store "1".
async function privateCookieStore(from) {
  if (!from || !(from.incognito || isPrivate(from.tabId))) return undefined;
  if (from.cookieStoreId) return from.cookieStoreId;
  if (!chrome.cookies.getAllCookieStores) return undefined;
  try {
    const stores = (await chrome.cookies.getAllCookieStores()) || [];
    const own = from.tabId != null && from.tabId >= 0 && stores.find((s) => (s.tabIds || []).includes(from.tabId));
    const any = stores.find((s) => s.incognito || s.id === "1" || s.id === "firefox-private");
    return (own || any || {}).id;
  } catch (_) {
    return undefined;
  }
}

// A tab as the `from` of cookieHeader.
function tabContext(tab) {
  return tab ? { tabId: tab.id, incognito: !!tab.incognito, cookieStoreId: tab.cookieStoreId } : null;
}

function baseName(path) {
  const name = (path || "").split(/[\\/]/).pop();
  return name || undefined;
}

// Add the browser's proxy setting to `msg`, for UDM's "use the browser's
// proxy" option: all of it as `browser_proxy` (fixed proxies, a
// configuration script, automatic detection, the system's setting or none,
// and the bypass list), and its fixed proxy alone as `proxy`, which is all
// older versions of UDM read.
async function addBrowserProxy(msg) {
  // Safari does not tell extensions its proxy.
  if (!chrome.proxy) return;
  let value;
  try {
    ({ value } = await chrome.proxy.settings.get({}));
  } catch (_) {
    return;
  }
  const fixed = fixedProxy(value);
  if (fixed) msg.proxy = fixed;
  const setting = proxySetting(value);
  if (setting) msg.browser_proxy = setting;
}

// The browser's fixed proxy as `scheme://host:port`. Only plain HTTP and
// SOCKS proxies are reported; other settings report nothing.
function fixedProxy(value) {
  if (value && "proxyType" in value) return firefoxProxy(value);
  if (!value || value.mode !== "fixed_servers" || !value.rules) return undefined;
  const p = value.rules.singleProxy || value.rules.proxyForHttps || value.rules.proxyForHttp;
  if (!p || !p.host || !["http", "socks4", "socks5"].includes(p.scheme || "http")) return undefined;
  const scheme = p.scheme || "http";
  const port = p.port || (scheme === "http" ? 80 : 1080);
  return `${scheme}://${p.host}:${port}`;
}

// Firefox reports its proxy as `{ proxyType, http, ssl, socks, ... }`, each
// proxy a "host:port" string. The HTTPS one is preferred, as Chrome's is.
function firefoxProxy(value) {
  if (value.proxyType !== "manual") return undefined;
  const http = value.httpProxyAll ? value.http : value.ssl || value.http;
  if (http) return proxyAddress("http", http, 80);
  if (value.socks) return proxyAddress(value.socksVersion === 4 ? "socks4" : "socks5", value.socks, 1080);
  return undefined;
}

function proxyAddress(scheme, address, defaultPort) {
  const hostPort = String(address).trim().replace(/^[a-z0-9+.-]+:\/\//i, "").replace(/\/.*$/, "");
  const m = /^(\[[^\]]+\]|[^:\s]+)(?::(\d+))?$/.exec(hostPort);
  if (!m) return undefined;
  return `${scheme}://${m[1]}:${Number(m[2]) || defaultPort}`;
}

// Hand `url` to UDM. `referrer` is sent to the server as the Referer
// header; `page`, the web page the download came from (the tab's address),
// is only shown and sorted by in UDM, as IDM keeps the two apart.
// `description` is a note shown with it in UDM's list (a link's title).
// `details` is what the browser's own request adds (requestDetails): its
// other headers, its redirects, and the response's size and type. `from`
// says which tab or download it comes from, for its cookies (cookieHeader).
async function handOff(url, referrer, filename, later = false, post = null, refreshId = null, page = null, description = null, details = {}, from = null) {
  const cookies = await cookieHeader(url, from);
  const msg = { op: "add", url, user_agent: navigator.userAgent, ...details };
  await addBrowserProxy(msg);
  if (post) {
    msg.method = "POST";
    msg.body = post.body;
    if (post.contentType) msg.headers = { "Content-Type": post.contentType };
  }
  // "Download later": UDM adds it to its main queue without starting it.
  if (later) msg.start = false;
  if (referrer) msg.referrer = referrer;
  if (isWeb(page)) msg.page = page;
  if (filename) msg.filename = filename;
  if (description) msg.description = description;
  if (cookies) msg.cookies = cookies;
  if (refreshId !== null) {
    // The same request, as the new link of a download already in UDM.
    const { op, ...download } = msg;
    return send({ op: "refresh_link", id: refreshId, download });
  }
  return send(msg);
}

// The web page a browser download came from. A download item only knows
// its referrer, the document that started it, which may be a frame inside
// the page; the page is the tab in front, the one the user clicked in. A
// download without a referrer (an address typed in) has no page, and a tab
// already showing the download's own address (a link opened in a new tab)
// is not one. Null when no tab fits.
async function downloadPage(referrer, url) {
  if (!referrer || !chrome.tabs || !chrome.tabs.query) return null;
  let tabs;
  try {
    tabs = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  } catch (_) {
    return null;
  }
  const tab = (tabs || [])[0];
  if (!tab || !privateAllowed(tab) || !isWeb(tab.url) || tab.url === url) return null;
  return tab.url;
}

// The browser's download bubble (Chrome's), hidden while a download is
// taken from it. Firefox cannot hide its download panel.
const bubble = bubbleGate((enabled) => {
  if (!chrome.downloads || !chrome.downloads.setUiOptions) return;
  try {
    const done = chrome.downloads.setUiOptions({ enabled });
    if (done && done.catch) done.catch(() => {});
  } catch (_) {
    // Not allowed (no downloads.ui permission): the bubble shows.
  }
});

// The web address of the frame `frameId` of tab `tabId` or, when it has
// none (about:blank, about:srcdoc), of the nearest frame above it that
// has one. Undefined when the browser cannot tell.
async function framesReferrer(tabId, frameId) {
  if (!chrome.webNavigation || !chrome.webNavigation.getAllFrames || tabId == null || tabId < 0 || frameId == null) return undefined;
  try {
    return frameReferrer(await chrome.webNavigation.getAllFrames({ tabId }), frameId);
  } catch (_) {
    return undefined;
  }
}

// Keep what a page said about a form's file fields (content.js).
function noteFormFiles(msg) {
  if (!isWeb(msg.url) || !Array.isArray(msg.names)) return;
  const now = Date.now();
  while (formFiles.length && (now - formFiles[0].time > POST_MATCH_MS || formFiles.length >= 50)) formFiles.shift();
  const names = msg.names.filter((n) => typeof n === "string").slice(0, 100);
  formFiles.push({ url: msg.url, names, chosen: !!msg.chosen, time: now });
}

// The recorded POST a download came from, with its form's file fields: the
// one its request (`record`) was, else the latest to its address.
function postFor(record, urls, now) {
  const posted =
    (record && record.method === "POST" && postById(posts, record.id)) ||
    urls.map((u) => findPost(posts, u, now)).find(Boolean) ||
    null;
  if (posted) {
    const files = findPost(formFiles, posted.url, now);
    if (files) posted.files = files;
  }
  return posted;
}

// Close tab `tabId` when it was opened only for the download at one of
// `urls`, as IDM does: taken by UDM, it would stay blank.
async function closeIfOpenedFor(tabId, urls) {
  if (tabId == null || tabId < 0 || !chrome.tabs || !chrome.tabs.get || !chrome.tabs.remove) return;
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  if (openedOnlyFor(tab, openedTabs.get(tabId), urls, Date.now())) chrome.tabs.remove(tabId).catch(() => {});
}

// Whether a download described by `described`, from tab `tabId`, would be
// taken now: UDM is connected, capture is on (and on in its tab), no skip
// applies, and the capture rules (or a download waiting for a new link) want
// it. A held key's action is in `described.key`. { refreshId }, or null.
function takesNow(described, tabId) {
  if (!connected || !captureEnabled || skipNext || clickApplies(skipClick, Date.now())) return null;
  if (!tabCaptures(tabId) || described.key === "skip") return null;
  const refreshId = refreshWaiting;
  if (refreshId === null && !captureDecision(rules, described).capture) return null;
  return { refreshId };
}

// A frame's response `d` that is a download (`record.response`), seen when
// its headers arrive, before the browser starts saving it. In Firefox the
// worker may hold the response: one UDM takes is canceled here and handed
// over, and the browser never starts it. Chrome lets the worker only look:
// the download is taken when it appears (downloads.onCreated), with what was
// learned here, and the bubble is hidden from now so it does not flash up.
function atHeaders(d, record) {
  const r = record.response;
  const described = {
    url: d.url,
    referrer: webReferrer(requestReferer(record), d.documentUrl, d.originUrl) || "",
    filename: r.filename,
    mime: r.mime,
    size: r.size,
    key: heldKey(d.tabId),
  };
  const verdict = takesNow(described, d.tabId);
  if (!verdict) return undefined;
  if (!BLOCKING) {
    // Without the downloads API (Safari) the download is not seen again:
    // the browser keeps it.
    if (!chrome.downloads) return undefined;
    record.bubbleHeld = true;
    bubble.hold();
    return undefined;
  }
  let post = null;
  let requestUrl = d.url;
  if (d.method === "POST") {
    const posted = postFor(record, [], Date.now());
    if (!posted) return undefined;
    post = rebuildPost(posted);
    // The browser downloads what cannot be rebuilt, as before.
    if (post.leave) return undefined;
    requestUrl = posted.url;
  }
  takeAtHeaders(d, record, post, requestUrl, verdict.refreshId);
  return { cancel: true };
}

// Hand the response `d` canceled at its headers to UDM (see atHeaders).
async function takeAtHeaders(d, record, post, requestUrl, refreshId) {
  const tab = d.tabId >= 0 && chrome.tabs && chrome.tabs.get ? await chrome.tabs.get(d.tabId).catch(() => null) : null;
  const tabUrl = tab && privateAllowed(tab) && isWeb(tab.url) && tab.url !== d.url ? tab.url : null;
  const parent = d.type === "sub_frame" ? await framesReferrer(d.tabId, d.parentFrameId) : undefined;
  const referrer = webReferrer(requestReferer(record), d.documentUrl, parent, d.originUrl, tabUrl);
  if (refreshWaiting === refreshId) refreshWaiting = null;
  const details = requestDetails(record);
  await deliver(requestUrl, referrer, record.response.filename, post, refreshId, tabUrl, details, d);
  if (d.type === "main_frame") closeIfOpenedFor(d.tabId, [d.url, ...record.redirects]);
}

// A download the browser created. Chrome's bubble may have been hidden for
// it when its headers came (atHeaders); it shows again once it is decided.
// Safari tells extensions of none.
if (chrome.downloads) {
  chrome.downloads.onCreated.addListener(async (item) => {
    const record = findRequest(requestLog, item.finalUrl || item.url, Date.now()) || findRequest(requestLog, item.url, Date.now());
    try {
      await browserDownload(item, record);
    } finally {
      if (record && record.bubbleHeld) {
        record.bubbleHeld = false;
        bubble.release();
      }
    }
  });
}

async function browserDownload(item, record) {
  await Promise.all([settingsReady, tabsOffReady]);
  if (!captureEnabled) return; // let the browser handle it
  if (!privateAllowed(item)) return; // keep private-window downloads out of UDM's list
  if (item.byExtensionId === chrome.runtime.id) return; // our own hand-back
  const url = item.finalUrl || item.url;
  if (!isWeb(url)) return; // blob:, data:, file: only exist in the browser
  // Capture turned off in the tab it comes from leaves it to the browser;
  // a key held down there (the skip or force keys) decides for it.
  const tabId = await downloadTabId(record);
  if (!tabCaptures(tabId)) return;
  const key = heldKey(tabId);
  // What the response's headers said fills in what the download item does
  // not know yet: Chrome often reports no size and type this early.
  const seen = (record && record.response) || {};
  const itemSize = item.totalBytes > 0 ? item.totalBytes : item.fileSize > 0 ? item.fileSize : 0;
  const described = {
    url,
    referrer: item.referrer,
    filename: baseName(item.filename) || seen.filename,
    mime: item.mime || seen.mime,
    size: itemSize || seen.size || 0,
    key,
  };
  if (!connected) {
    // It stays in the browser; say so if UDM would have taken it.
    if (!skipNext && !clickApplies(skipClick, Date.now()) && key !== "skip" && captureDecision(rules, described).capture) {
      noticeTakeoverFailed(url, described.filename, null);
    }
    return;
  }
  if (skipNext) {
    skipNext = false;
    return;
  }
  if (clickApplies(skipClick, Date.now())) {
    skipClick = null;
    return;
  }
  if (key === "skip") return;
  // A download waiting for a new link takes this one, whatever the capture
  // rules say: the user asked for it.
  const refreshId = refreshWaiting;
  if (refreshId === null && !captureDecision(rules, described).capture) return;

  await takeDownload(item, url, described.filename, refreshId, true, record);
}

// Take browser download `item` (its address `url`, named `filename`) from
// the browser and hand it to UDM, or, with `refreshId`, give that download
// in UDM it as its new link. Used for the downloads the browser starts and
// for one the popup hands over after it started. False when UDM did not get
// it: a form whose request cannot be rebuilt or one already finished stays
// in the browser, one the host did not take goes back to it.
// `guessPage` takes the tab in front as its page (see downloadPage), which
// only holds while the download has just started. `record` is the request
// it came from, when the worker saw it (see request.js).
async function takeDownload(item, url, filename, refreshId = null, guessPage = true, record = null) {
  // A form's download is sent as the same POST; one whose request cannot be
  // rebuilt stays in the browser.
  let post = null;
  let requestUrl = url;
  const posted = postFor(record, [item.url, url], Date.now());
  if (posted) {
    post = rebuildPost(posted);
    if (post.leave) return false;
    requestUrl = posted.url;
  }

  // Take it from the browser before it writes much, then hand it over.
  bubble.hold();
  try {
    await chrome.downloads.cancel(item.id);
    await chrome.downloads.erase({ id: item.id });
  } catch (_) {
    // Already finished or gone; nothing to take.
    return false;
  } finally {
    bubble.release();
  }
  // The referrer is the document that started it, or the nearest one with
  // an address when that was made from nothing (about:blank).
  const header = requestReferer(record);
  const from = webReferrer(item.referrer, header);
  const page = guessPage ? await downloadPage(from || item.referrer, url) : null;
  const parent = record && record.type === "sub_frame" ? await framesReferrer(record.tabId, record.parentFrameId) : undefined;
  const referrer = from || (item.referrer ? webReferrer(parent, page) : undefined);
  if (refreshId !== null) refreshWaiting = null;
  const origin = { tabId: record ? record.tabId : undefined, incognito: !!item.incognito, cookieStoreId: item.cookieStoreId };
  const sent = await deliver(requestUrl, referrer, filename, post, refreshId, page, requestDetails(record), origin);
  if (record && record.type === "main_frame") closeIfOpenedFor(record.tabId, [url, item.url, ...record.redirects]);
  return sent;
}

// Send a download taken from the browser to UDM, or as the new link of
// download `refreshId`; give it back to the browser when the message does
// not go. Says whether UDM has it (a refresh counts as taken either way).
async function deliver(requestUrl, referrer, filename, post, refreshId, page, details, from = null) {
  if (refreshId !== null) {
    if (await handOff(requestUrl, referrer, filename, false, post, refreshId, page, null, details, from)) return true;
    giveBack(requestUrl, post);
    return true;
  }
  awaitingAck.set(requestUrl, { post, filename });
  const sent = await handOff(requestUrl, referrer, filename, false, post, null, page, null, details, from);
  if (!sent) {
    // The port dropped between the check and the send: give it back.
    awaitingAck.delete(requestUrl);
    giveBack(requestUrl, post);
    noticeTakeoverFailed(requestUrl, filename, null);
  }
  return sent;
}

// --- Downloads the browser already started ----------------------------------

// What the popup lists of the browser's own downloads in progress, which
// "Send to UDM" takes over (IDM's "download with IDM" on a started
// download; Chrome has no menu on its downloads). Private windows' stay out
// unless UDM's settings let them in.
async function browserDownloads() {
  if (!chrome.downloads || !chrome.downloads.search) return [];
  let items;
  try {
    items = await chrome.downloads.search({ state: "in_progress" });
  } catch (_) {
    return [];
  }
  return (items || [])
    .filter((d) => privateAllowed(d) && isWeb(d.finalUrl || d.url))
    .map((d) => ({
      id: d.id,
      url: d.finalUrl || d.url,
      filename: baseName(d.filename) || "",
      done: d.bytesReceived || 0,
      total: d.totalBytes > 0 ? d.totalBytes : null,
    }));
}

// Hand the browser's download `id` to UDM: the browser's copy is canceled
// and erased, and UDM starts it again with its referrer, name and cookies,
// as a captured download. Whatever the capture rules say: the user asked.
async function sendBrowserDownload(id) {
  if (!connected) {
    noticeCannotReach();
    return false;
  }
  if (!chrome.downloads) return false;
  let item;
  try {
    [item] = await chrome.downloads.search({ id });
  } catch (_) {
    return false;
  }
  if (!item || item.state !== "in_progress" || !privateAllowed(item)) return false;
  const url = item.finalUrl || item.url;
  if (!isWeb(url)) return false;
  // It may have started long ago, in another tab than the one in front.
  return takeDownload(item, url, baseName(item.filename), null, false);
}

// --- All links on a page ----------------------------------------------------

// The most links one page sends to the picker.
const MAX_PAGE_LINKS = 5000;

// Runs in each frame of the page after links.js (so it can use only what
// that declares): the frame's links as { url, text, image }, from its
// links, frames, inline scripts and images, and from the frames inside it
// made from nothing; see collectPageLinks. With `selection`, only what the
// selection holds.
function collectLinks(selection, types) {
  return collectPageLinks(document, { selection, types });
}

// Collect the links of tab `tabId` (in every frame) and send them to the
// window's picker, with the cookies of each site they point to. Says how it
// went: "sent", "none" (no links), "unreadable" (a page extensions may not
// script) or "unreachable" (UDM is not connected); see noticePick.
async function pickLinks(tabId, pageUrl, selection = false) {
  if (!connected) return "unreachable";
  if (!chrome.scripting || tabId == null || tabId < 0) return "unreadable";
  let results;
  try {
    // The extraction first: frames made before UDM was installed, or made
    // from nothing, have no content script.
    await chrome.scripting.executeScript({ target: { tabId, allFrames: true }, files: ["links.js"] });
    results = await chrome.scripting.executeScript({
      target: { tabId, allFrames: true },
      func: collectLinks,
      args: [selection, (rules && rules.capture_types) || []],
    });
  } catch (_) {
    return "unreadable"; // a page extensions may not script, such as the web store
  }
  // A link in several frames (or in a frame made from nothing, which its
  // parent walks into and the browser may also have run the script in) is
  // listed once.
  const links = dedupeLinks(
    (results || []).flatMap((r) => (Array.isArray(r && r.result) ? r.result : [])),
    MAX_PAGE_LINKS,
  );
  if (links.length === 0) return "none";
  const cookies = new Map();
  for (const l of links) {
    let origin;
    try {
      origin = new URL(l.url).origin;
    } catch (_) {
      continue;
    }
    if (!isWeb(l.url)) continue; // ftp has no cookies
    if (!cookies.has(origin)) cookies.set(origin, await cookieHeader(l.url, { tabId, incognito: isPrivate(tabId) }));
    if (cookies.get(origin)) l.cookies = cookies.get(origin);
  }
  return send({ op: "pick_links", page: pageUrl, links, user_agent: navigator.userAgent }) ? "sent" : "unreachable";
}

// Say why the links of a page did not reach UDM's picker, when they did not:
// a menu item or button that does nothing would look broken.
function noticePick(result, selection) {
  if (result === "none" && selection) notice(chrome.i18n.getMessage("noSelectedLinksFound"));
  else if (result === "none") notice(chrome.i18n.getMessage("noLinksFound"));
  else if (result === "unreadable") notice(chrome.i18n.getMessage("linksUnreadable"));
  else if (result === "unreachable") noticeCannotReach();
  return result === "sent";
}

// --- Pages for a site crawl --------------------------------------------------

// A page UDM's grabber reads after its scripts run: opened in a background
// tab, given time to load and settle, then read and closed.
const RENDER_TIMEOUT_MS = 30000;
const RENDER_SETTLE_MS = 1500;

function waitForLoad(tabId, ms) {
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      chrome.tabs.onUpdated.removeListener(listener);
      resolve();
    };
    const listener = (id, info) => {
      if (id === tabId && info.status === "complete") done();
    };
    const timer = setTimeout(done, ms);
    chrome.tabs.onUpdated.addListener(listener);
  });
}

// The page at `url` as the browser shows it: { html, url, popups }, or
// null. Windows its scripts open are closed at once, and their addresses
// listed in `popups` for UDM to follow or ignore.
async function renderPage(url) {
  if (!isWeb(url) || !chrome.scripting || !chrome.tabs || !chrome.tabs.create) return null;
  let tab = null;
  const popups = [];
  const opened = (t) => {
    if (!tab || t.openerTabId !== tab.id) return;
    const at = t.pendingUrl || t.url;
    if (isWeb(at)) popups.push(at);
    chrome.tabs.remove(t.id).catch(() => {});
  };
  chrome.tabs.onCreated.addListener(opened);
  try {
    tab = await chrome.tabs.create({ url, active: false });
    await waitForLoad(tab.id, RENDER_TIMEOUT_MS);
    await new Promise((r) => setTimeout(r, RENDER_SETTLE_MS));
    const [res] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: () => ({ html: document.documentElement.outerHTML, url: location.href }),
    });
    return res && res.result ? { ...res.result, popups } : null;
  } catch (_) {
    return null;
  } finally {
    chrome.tabs.onCreated.removeListener(opened);
    if (tab) chrome.tabs.remove(tab.id).catch(() => {});
  }
}

// --- Context menu -----------------------------------------------------------

// The menu entries of each item UDM's Options can leave out (its
// `browser_menu_hidden_chromium` and `browser_menu_hidden_firefox` name
// them), with their titles and contexts.
const PAGE_CONTEXTS = ["page", "frame", "video", "audio"];
const MENU_ITEMS = {
  page: [{ id: "udm-page", title: chrome.i18n.getMessage("downloadPageWithUdm"), contexts: ["page", "frame"] }],
  download: [
    { id: "udm-link", title: chrome.i18n.getMessage("downloadWithUdm"), contexts: ["link"] },
    { id: "udm-media", title: chrome.i18n.getMessage("downloadWithUdm"), contexts: ["image", "video", "audio"] },
  ],
  download_later: [
    { id: "udm-later-link", title: chrome.i18n.getMessage("downloadLaterWithUdm"), contexts: ["link"] },
    { id: "udm-later-media", title: chrome.i18n.getMessage("downloadLaterWithUdm"), contexts: ["image", "video", "audio"] },
  ],
  all_links: [{ id: "udm-all-links", title: chrome.i18n.getMessage("downloadAllLinksWithUdm"), contexts: ["page", "frame"] }],
  selected_links: [{ id: "udm-selection-links", title: chrome.i18n.getMessage("downloadSelectedLinksWithUdm"), contexts: ["selection"] }],
  last_video: [{ id: "udm-last-video", title: chrome.i18n.getMessage("downloadLastVideo"), contexts: PAGE_CONTEXTS }],
  recent_videos: [{ id: "udm-recent", title: chrome.i18n.getMessage("recentVideos"), contexts: PAGE_CONTEXTS }],
  gallery: [{ id: "udm-gallery", title: chrome.i18n.getMessage("downloadGalleryWithUdm"), contexts: ["page", "frame", "image"] }],
};
// How many videos the recent videos submenu lists.
const RECENT_VIDEOS = 10;
const RECENT_PREFIX = "udm-recent-";
// The hidden list the menu was last built for, as JSON.
let menuBuiltFor = null;
const ignoreMenuError = () => void chrome.runtime.lastError;

// Firefox has a list of its own: its extension pages are moz-extension://
// ones, where Chrome's and Edge's are chrome-extension://.
const IS_FIREFOX = String(chrome.runtime.getURL("")).startsWith("moz-extension:");

function menuHidden() {
  return hiddenMenuItems(rules, IS_FIREFOX);
}

// Build the context menu with the items UDM's settings leave in.
function buildMenus() {
  const hidden = menuHidden();
  menuBuiltFor = JSON.stringify(hidden);
  chrome.contextMenus.removeAll(() => {
    recentMenuIds = [];
    for (const [name, entries] of Object.entries(MENU_ITEMS)) {
      if (hidden.includes(name)) continue;
      for (const e of entries) {
        chrome.contextMenus.create({ id: e.id, title: e.title, contexts: e.contexts }, ignoreMenuError);
      }
    }
    scheduleRecentMenu();
  });
}

// The recent videos submenu's entries, and the update that rebuilds them.
let recentMenuIds = [];
let recentMenuTimer = 0;
let recentMenuUpdate = Promise.resolve();

function scheduleRecentMenu() {
  clearTimeout(recentMenuTimer);
  recentMenuTimer = setTimeout(() => {
    recentMenuUpdate = recentMenuUpdate.then(updateRecentMenu, updateRecentMenu);
  }, 300);
}

async function tabInfo(tabId) {
  try {
    const tab = await chrome.tabs.get(tabId);
    return { title: tab.title || "", url: tab.url || "" };
  } catch (_) {
    return { title: "", url: "" };
  }
}

// List the newest videos (the video button's, across tabs) under the
// recent videos item, and enable the items only when there is one.
async function updateRecentMenu() {
  const hidden = menuHidden();
  const recent = recentMedia(media, RECENT_VIDEOS);
  if (!hidden.includes("last_video")) {
    chrome.contextMenus.update("udm-last-video", { enabled: recent.length > 0 }, ignoreMenuError);
  }
  if (hidden.includes("recent_videos")) return;
  for (const id of recentMenuIds) chrome.contextMenus.remove(id, ignoreMenuError);
  recentMenuIds = [];
  for (const { tabId, entry } of recent) {
    const { title } = await tabInfo(tabId);
    const name = (title || hostOf(entry.url)).slice(0, 60);
    const id = RECENT_PREFIX + entry.id;
    chrome.contextMenus.create(
      { id, parentId: "udm-recent", title: `${name} (${mediaLabel(entry)})`, contexts: PAGE_CONTEXTS },
      ignoreMenuError,
    );
    recentMenuIds.push(id);
  }
  chrome.contextMenus.update("udm-recent", { enabled: recent.length > 0 }, ignoreMenuError);
}

// The right-clicked link's `download` attribute and title, from the content
// script of its frame (which saw the click), as { filename, description };
// empty when it cannot tell (a page extensions may not script).
async function linkDetails(tab, info) {
  if (!tab || tab.id == null || tab.id < 0 || !chrome.tabs || !chrome.tabs.sendMessage) return {};
  let answer;
  try {
    answer = await chrome.tabs.sendMessage(tab.id, { type: "link:details", url: info.linkUrl }, { frameId: info.frameId || 0 });
  } catch (_) {
    return {};
  }
  if (!answer) return {};
  return { filename: linkFileName(answer.download), description: linkDescription(answer.title, answer.text) };
}

// Firefox shows "Download selected links" only when the selection holds
// links: when the menu opens it asks the selection's frame and hides the
// item if not (menus.onShown and menus.refresh). Chrome has neither, so
// there the item shows for every selection.
const MENUS = chrome.menus || chrome.contextMenus;
if (MENUS && MENUS.onShown && MENUS.refresh) {
  // Counts menus opened, so a late answer for an earlier menu is dropped.
  let shownMenu = 0;
  MENUS.onShown.addListener(async (info, tab) => {
    const menu = ++shownMenu;
    // Not menuIds: Firefox lists only the items it shows, so once hidden
    // the item would never be in it again. A selection menu with the item
    // in UDM's menu (the settings may leave it out) is the one to check.
    const made = !menuHidden().includes("selected_links");
    if (!made || !(info.contexts || []).includes("selection") || !tab || tab.id == null) return;
    let links = linksFromText(info.selectionText).length > 0;
    if (!links) {
      try {
        const answer = await chrome.tabs.sendMessage(tab.id, { type: "selection:hasLinks" }, { frameId: info.frameId || 0 });
        // No answer (a page extensions may not script): leave the item.
        links = !answer || answer.links !== false;
      } catch (_) {
        links = true;
      }
    }
    if (menu !== shownMenu) return;
    MENUS.update("udm-selection-links", { visible: links }, ignoreMenuError);
    MENUS.refresh();
  });
  if (MENUS.onHidden) MENUS.onHidden.addListener(() => shownMenu++);
}

// Send a recent video to UDM in its best quality, named after its page.
async function downloadRecent({ tabId, entry }) {
  const { title, url } = await tabInfo(tabId);
  return downloadMedia(tabId, { entry: entry.id, title }, url, url);
}

chrome.runtime.onInstalled.addListener(buildMenus);

// Tabs open before UDM was installed (or updated, or turned back on) have
// no content script: the browser only adds it to pages loaded after. They
// get it here, in every frame that does not have it yet (content.js also
// does nothing when it runs twice). Pages extensions may not script, and
// tabs the browser has unloaded, are left as they are.
async function injectOpenTabs() {
  if (!chrome.scripting || !chrome.tabs || !chrome.tabs.query) return 0;
  const scripts = ((chrome.runtime.getManifest().content_scripts || [])[0] || {}).js || [];
  if (scripts.length === 0) return 0;
  let tabs = [];
  try {
    tabs = await chrome.tabs.query({ url: ["http://*/*", "https://*/*"] });
  } catch (_) {
    return 0;
  }
  let injected = 0;
  for (const tab of tabs || []) {
    if (!tab || tab.id == null || tab.discarded || !isWeb(tab.url)) continue;
    try {
      // A frame is claimed as it is checked, so two runs at once (install
      // and first connect) do not both inject into it.
      const results = await chrome.scripting.executeScript({
        target: { tabId: tab.id, allFrames: true },
        func: () => {
          if (globalThis.udmContentLoaded === true || globalThis.udmContentInjecting === true) return true;
          globalThis.udmContentInjecting = true;
          return false;
        },
      });
      const missing = (results || []).filter((r) => r && r.result !== true).map((r) => r.frameId);
      if (missing.length === 0) continue;
      await chrome.scripting.executeScript({ target: { tabId: tab.id, frameIds: missing }, files: scripts });
      injected++;
    } catch (_) {
      // A page extensions may not script, such as the web store.
    }
  }
  return injected;
}
chrome.runtime.onInstalled.addListener(() => injectOpenTabs());

// The first time UDM connects in a browser session, open tabs get the
// content script too: an extension turned off and on again, or reloaded,
// gets no onInstalled. Once a session (chrome.storage.session) is enough.
let injectedOnConnect = false;
async function injectOnConnect() {
  if (injectedOnConnect) return;
  injectedOnConnect = true;
  const store = chrome.storage && chrome.storage.session;
  if (store) {
    const s = await store.get({ contentInjected: false }).catch(() => ({}));
    if (s.contentInjected) return;
    await store.set({ contentInjected: true }).catch(() => {});
  }
  await injectOpenTabs();
}

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  // A private window's links and media stay out of UDM, like its downloads,
  // unless UDM's settings let them in.
  if (!privateAllowed(tab)) return;
  if (info.menuItemId === "udm-last-video") {
    const [last] = recentMedia(media, 1);
    if (last) await downloadRecent(last);
    return;
  }
  if (String(info.menuItemId).startsWith(RECENT_PREFIX)) {
    const id = Number(String(info.menuItemId).slice(RECENT_PREFIX.length));
    const chosen = recentMedia(media, Infinity).find((r) => r.entry.id === id);
    if (chosen) await downloadRecent(chosen);
    return;
  }
  if (info.menuItemId === "udm-all-links" || info.menuItemId === "udm-selection-links") {
    const selection = info.menuItemId === "udm-selection-links";
    noticePick(await pickLinks(tab && tab.id, info.pageUrl || (tab && tab.url) || "", selection), selection);
    return;
  }
  if (info.menuItemId === "udm-gallery") {
    // The tab's page goes to UDM's window, which has gallery-dl list
    // its pictures there: nothing runs until this click, and only for an
    // http or https page.
    const page = info.pageUrl || (tab && tab.url) || "";
    if (!isWeb(page)) return;
    if (!connected || !send({ op: "pick_gallery", page, user_agent: navigator.userAgent })) noticeCannotReach();
    return;
  }
  if (info.menuItemId === "udm-page") {
    // The page (or the frame clicked in) itself, as IDM's "Download with
    // IDM" on a page does; the tab's page is its referrer and its page.
    // A frame made from nothing (about:blank) is no page to download.
    const url = webReferrer(info.frameUrl, info.pageUrl);
    if (!isWeb(url)) return;
    if (!connected || !(await handOff(url, info.pageUrl, undefined, false, null, null, info.pageUrl, null, {}, tabContext(tab)))) noticeCannotReach();
    return;
  }
  const isLink = info.menuItemId === "udm-link" || info.menuItemId === "udm-later-link";
  const later = info.menuItemId === "udm-later-link" || info.menuItemId === "udm-later-media";
  const url = isLink ? info.linkUrl : info.srcUrl;
  if (!isWeb(url)) return;
  // The link's frame is the referrer, or the nearest frame above it with an
  // address when it has none (about:blank); the tab's page is where it came
  // from.
  const parent = isWeb(info.frameUrl) ? undefined : await framesReferrer(tab && tab.id, info.frameId);
  const referrer = webReferrer(info.frameUrl, parent, info.pageUrl);
  // A link's `download` attribute names the file, and its title (or text)
  // is the download's description, as IDM takes them.
  const { filename, description } = isLink && connected ? await linkDetails(tab, info) : {};
  if (connected && (await handOff(url, referrer, filename, later, null, null, info.pageUrl, description, {}, tabContext(tab)))) return;
  // UDM is not available; the click should still download something.
  noticeCannotReach(true);
  browserGets(url);
});

// --- Popup ------------------------------------------------------------------

function snapshot() {
  return {
    connected,
    lastError,
    capture: captureEnabled,
    tabsOff: [...tabsOff],
    skipNext,
    refreshWaiting,
    videoButton: !rules || rules.video_button !== false,
    videoExcluded: rules ? rules.video_button_excluded_sites || [] : [],
    items: [...downloads.values()].sort((a, b) => b.id - a.id),
  };
}

// Change the video button setting in UDM: on or off everywhere, or for one
// site. UDM keeps it, and the window's Options shows it.
function setVideoButton(change) {
  if (!rules) return;
  const next = { ...rules };
  if (typeof change.on === "boolean") next.video_button = change.on;
  if (typeof change.site === "string" && /^[a-z0-9.-]+$/i.test(change.site)) {
    const site = change.site.toLowerCase();
    const list = (rules.video_button_excluded_sites || []).filter((s) => !siteMatches(s, site));
    if (!change.siteOn) list.push(site);
    next.video_button_excluded_sites = list;
  }
  send({ op: "set_settings", settings: next });
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  // Only this extension's own pages and content scripts talk to the worker.
  if (sender.id !== chrome.runtime.id) return;
  // Content scripts run in web pages; the popup (even opened in a tab) is
  // one of the extension's own pages.
  // chrome-extension://<id>/ in Chrome and Edge, moz-extension://<uuid>/ in
  // Firefox.
  const ownPage = String(sender.url || "").startsWith(chrome.runtime.getURL(""));
  if (sender.tab && !ownPage) return onContentMessage(msg, sender, sendResponse);
  switch (msg.type) {
    case "state":
      break;
    case "setCapture":
      captureEnabled = !!msg.value;
      chrome.storage.local.set({ capture: captureEnabled });
      shareLinkCapture();
      break;
    case "cancelRefresh":
      refreshWaiting = null;
      send({ op: "cancel_refresh" });
      break;
    case "setSkipNext":
      skipNext = !!msg.value;
      break;
    case "setTabCapture":
      if (!Number.isInteger(msg.tabId)) return;
      setTabCapture(msg.tabId, !!msg.value);
      break;
    case "setVideoButton":
      setVideoButton(msg);
      break;
    case "add":
      if (isWeb(msg.url)) send({ op: "add", url: msg.url });
      break;
    case "browserDownloads":
      browserDownloads().then((items) => sendResponse({ items }));
      return true;
    case "sendBrowserDownload":
      if (!Number.isInteger(msg.id)) return;
      sendBrowserDownload(msg.id).then((ok) => sendResponse({ ok }));
      return true;
    case "allLinks":
      // The picker opens in the window; the popup only starts it.
      chrome.tabs
        .query({ active: true, currentWindow: true })
        .then(([tab]) => tab && pickLinks(tab.id, tab.url || "").then((result) => noticePick(result, false)))
        .catch(() => {});
      break;
    case "start":
      if (Number.isInteger(msg.id)) {
        const confirm = { op: "confirm", id: msg.id, start: true };
        // Started from the popup, nothing asks about a taken name: under
        // "ask" it gets a new one instead of asking again.
        if (rules && rules.file_exists === "ask") confirm.file_exists = "rename";
        send(confirm);
      }
      break;
    case "cancel":
    case "retry":
    case "remove":
      if (Number.isInteger(msg.id)) send({ op: msg.type, id: msg.id });
      break;
    case "reconnect":
      reconnectDelay = RECONNECT_MIN_MS;
      if (!port) connect();
      break;
    default:
      return;
  }
  sendResponse(snapshot());
});

connect();
updateBadge();
