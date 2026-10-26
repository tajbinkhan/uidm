// Popup: shows the worker's state and sends it commands. The worker owns the
// connection to UIDM; this page only asks it for snapshots.
//
// Names and URLs come from the web, so everything is built with DOM calls and
// textContent, never innerHTML.

const MAX_ROWS = 20;
const $ = (id) => document.getElementById(id);

function ask(msg) {
  return chrome.runtime.sendMessage(msg).then(render).catch(() => {});
}

function human(n) {
  if (n == null) return "?";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return u === 0 ? `${n} B` : `${v.toFixed(1)} ${units[u]}`;
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
  queued: "Queued",
  downloading: "Downloading",
  done: "Done",
  failed: "Failed",
  canceled: "Canceled",
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
  meta.append(el("span", { className: "grow" }, e.total ? `${human(e.done)} / ${human(e.total)}` : human(e.done)));

  if (e.status === "queued" || e.status === "downloading") {
    meta.append(button("Cancel", () => ask({ type: "cancel", id: e.id }), !connected));
  } else if (e.status === "failed" || e.status === "canceled") {
    meta.append(button("Retry", () => ask({ type: "retry", id: e.id }), !connected));
    meta.append(button("Remove", () => ask({ type: "remove", id: e.id }), !connected));
  } else if (e.status === "done") {
    meta.append(button("Remove", () => ask({ type: "remove", id: e.id }), !connected));
  }
  li.append(meta);
  return li;
}

function render(state) {
  if (!state) return;
  $("dot").className = state.connected ? "dot on" : "dot";
  const status = $("status");
  if (state.connected) {
    status.className = "status";
    status.textContent = state.capture
      ? "Connected. Downloads you start in the browser go to UIDM."
      : "Connected. Capture is off, so the browser keeps its own downloads.";
  } else {
    status.className = "status error";
    status.textContent = `Not connected to UIDM${state.lastError ? `: ${state.lastError}` : ""}. Downloads stay in the browser.`;
  }
  $("capture").checked = state.capture;
  $("add").disabled = !state.connected;

  const list = $("list");
  list.replaceChildren();
  const items = state.items.slice(0, MAX_ROWS);
  if (items.length === 0) {
    list.append(el("li", { className: "empty" }, "No downloads yet."));
  } else {
    for (const e of items) list.append(row(e, state.connected));
  }
}

$("capture").addEventListener("change", (ev) => ask({ type: "setCapture", value: ev.target.checked }));

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
