// Popup: shows the worker's state and sends it commands. The worker owns the
// connection to UIDM; this page only asks it for snapshots.
//
// Names and URLs come from the web, so everything is built with DOM calls and
// textContent, never innerHTML.

const MAX_ROWS = 20;
const $ = (id) => document.getElementById(id);
const t = (name, subs) => chrome.i18n.getMessage(name, subs);

// The page's own text in the browser's language: data-i18n names the message
// for an element's text, data-i18n-title, -placeholder and -aria-label those
// for its attributes. The HTML keeps the English for reading it as it is.
for (const node of document.querySelectorAll("[data-i18n]")) node.textContent = t(node.dataset.i18n);
for (const attr of ["title", "placeholder", "aria-label"]) {
  for (const node of document.querySelectorAll(`[data-i18n-${attr}]`)) {
    node.setAttribute(attr, t(node.getAttribute(`data-i18n-${attr}`)));
  }
}

function ask(msg) {
  return chrome.runtime.sendMessage(msg).then(render).catch(() => {});
}

// Sizes are written the way the browser's language writes numbers.
const SIZE_UNITS = ["sizeBytes", "sizeKB", "sizeMB", "sizeGB", "sizeTB"];
const oneDecimal = new Intl.NumberFormat(chrome.i18n.getUILanguage(), {
  minimumFractionDigits: 1,
  maximumFractionDigits: 1,
  useGrouping: false,
});

function human(n) {
  if (n == null) return "?";
  let v = n;
  let u = 0;
  while (v >= 1024 && u < SIZE_UNITS.length - 1) {
    v /= 1024;
    u++;
  }
  return t(SIZE_UNITS[u], u === 0 ? String(n) : oneDecimal.format(v));
}

function nameOf(e) {
  const fromPath = (e.path || "").split(/[\\/]/).pop();
  if (fromPath) return fromPath;
  const tail = e.url.split(/[?#]/)[0].split("/").pop();
  return tail || e.url;
}

function el(tag, attrs = {}, text) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) node[k] = v;
  if (text != null) node.textContent = text;
  return node;
}

function button(label, onClick, disabled) {
  const b = el("button", { disabled }, label);
  b.addEventListener("click", onClick);
  return b;
}

const STATUS_TEXT = {
  queued: t("stateQueued"),
  downloading: t("stateDownloading"),
  done: t("stateDone"),
  failed: t("stateFailed"),
  canceled: t("stateCanceled"),
  interrupted: t("stateInterrupted"),
  confirming: t("stateConfirming"),
  waiting: t("stateWaiting"),
};

function row(e, connected) {
  const li = el("li");
  li.append(el("div", { className: "name", title: e.path || e.url }, nameOf(e)));

  const bar = el("div", { className: "bar" });
  const fill = el("div");
  const pct = e.total ? Math.min(100, (e.done / e.total) * 100) : e.status === "done" ? 100 : 0;
  fill.style.width = `${pct}%`;
  bar.append(fill);
  li.append(bar);

  const meta = el("div", { className: "meta" });
  const status = el("span", { className: e.status, title: e.error || "" }, STATUS_TEXT[e.status] || e.status);
  meta.append(status);
  if (e.scan && e.scan.result === "threat") {
    meta.append(el("span", { className: "failed", title: e.scan.detail || "" }, t("threatFound")));
  }
  meta.append(el("span", { className: "grow" }, e.total ? t("sizeProgress", [human(e.done), human(e.total)]) : human(e.done)));

  if (e.status === "confirming") {
    meta.append(button(t("start"), () => ask({ type: "start", id: e.id }), !connected));
    meta.append(button(t("cancel"), () => ask({ type: "cancel", id: e.id }), !connected));
  } else if (e.status === "waiting") {
    meta.append(button(t("start"), () => ask({ type: "start", id: e.id }), !connected));
    meta.append(button(t("remove"), () => ask({ type: "remove", id: e.id }), !connected));
  } else if (e.status === "queued" || e.status === "downloading") {
    meta.append(button(t("cancel"), () => ask({ type: "cancel", id: e.id }), !connected));
  } else if (e.status === "failed" || e.status === "canceled" || e.status === "interrupted") {
    meta.append(button(t("retry"), () => ask({ type: "retry", id: e.id }), !connected));
    meta.append(button(t("remove"), () => ask({ type: "remove", id: e.id }), !connected));
  } else if (e.status === "done") {
    meta.append(button(t("remove"), () => ask({ type: "remove", id: e.id }), !connected));
  }
  li.append(meta);
  return li;
}

// Whether the next download is set to stay in the browser, as last reported.
let skipNext = false;
// The site of the tab the popup opened over, for the per-site video button.
let tabHost = "";
if (chrome.tabs) {
  chrome.tabs
    .query({ active: true, currentWindow: true })
    .then(([tab]) => {
      tabHost = /^https?:/i.test(tab?.url || "") ? new URL(tab.url).hostname.toLowerCase() : "";
    })
    .catch(() => {});
}

function render(state) {
  if (!state) return;
  skipNext = state.skipNext;
  $("dot").className = state.connected ? "dot on" : "dot";
  const status = $("status");
  if (state.connected) {
    status.className = "status";
    status.textContent = state.capture ? t("statusConnected") : t("statusConnectedCaptureOff");
  } else {
    status.className = "status error";
    status.textContent = state.lastError ? t("statusNotConnectedReason", state.lastError) : t("statusNotConnected");
  }
  $("capture").checked = state.capture;
  const refresh = $("refresh");
  const waitingFor = state.items.find((e) => e.id === state.refreshWaiting);
  refresh.hidden = !waitingFor;
  if (waitingFor) {
    $("refresh-text").textContent = t("refreshWaiting", nameOf(waitingFor));
  }
  $("all-links").disabled = !state.connected;
  const skip = $("skip");
  skip.disabled = !state.connected || !state.capture;
  skip.textContent = state.skipNext ? t("dontSkip") : t("skipNextDownload");
  skip.title = state.skipNext ? t("skippingNextDownloadTitle") : t("skipNextDownloadTitle");
  $("add").disabled = !state.connected;

  const video = $("video");
  video.checked = state.videoButton;
  video.disabled = !state.connected;
  const siteLabel = $("video-site-label");
  siteLabel.hidden = !tabHost || !state.videoButton;
  if (tabHost) {
    $("video-site-text").textContent = t("videoButtonOnSite", tabHost);
    $("video-site").checked = !state.videoExcluded.some((site) => siteMatches(site, tabHost));
    $("video-site").disabled = !state.connected;
  }

  const list = $("list");
  list.replaceChildren();
  const items = state.items.slice(0, MAX_ROWS);
  if (items.length === 0) {
    list.append(el("li", { className: "empty" }, t("noDownloads")));
  } else {
    for (const e of items) list.append(row(e, state.connected));
  }
}

$("capture").addEventListener("change", (ev) => ask({ type: "setCapture", value: ev.target.checked }));
$("skip").addEventListener("click", () => ask({ type: "setSkipNext", value: !skipNext }));
$("all-links").addEventListener("click", () => ask({ type: "allLinks" }).then(() => window.close()));
$("refresh-cancel").addEventListener("click", () => ask({ type: "cancelRefresh" }));
$("video").addEventListener("change", (ev) => ask({ type: "setVideoButton", on: ev.target.checked }));
$("video-site").addEventListener("change", (ev) =>
  ask({ type: "setVideoButton", site: tabHost, siteOn: ev.target.checked }),
);

function addFromInput() {
  const url = $("url").value.trim();
  if (!/^https?:\/\//i.test(url)) return;
  $("url").value = "";
  ask({ type: "add", url });
}
$("add").addEventListener("click", addFromInput);
$("url").addEventListener("keydown", (ev) => {
  if (ev.key === "Enter") addFromInput();
});

// The worker reconnects on its own; nudging it when the popup opens means a
// fresh attempt instead of waiting out the backoff.
ask({ type: "reconnect" });
setInterval(() => ask({ type: "state" }), 500);
