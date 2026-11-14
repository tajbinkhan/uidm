// The native port in Safari.
//
// Chrome, Edge and Firefox start udm-host themselves (runtime.connectNative)
// and keep a port to it open both ways. Safari hands native messages to the
// app extension inside UDM.app (installer/macos/safari), which can answer a
// message but cannot send one of its own. So in Safari the worker talks
// through pollingPort, which looks like the port connectNative returns:
// each message goes out with runtime.sendNativeMessage, and one poll is
// always out besides, which the app extension answers as soon as udm-host
// has written something (or after about 20 seconds with nothing).
//
//   a port's first request: { frames: [...], start: true }, which starts a
//                           new udm-host, so nothing an earlier worker left
//                           unread comes to this one
//   messages:               { frames: [message, ...] }
//   a poll:                 { poll: true }
//   every answer:           { session, frames, more, error }
//
// `session` names the udm-host that answered; `frames` are what it wrote
// (only polls carry them, so they arrive in order); `more` says it has
// further frames already; `error` that it is gone, and why.

// The least time between two polls that bring nothing, should the app
// extension answer at once, and how long any request may take: longer than
// the app extension holds a poll.
var SAFARI_POLL_GAP_MS = 1000;
var SAFARI_REQUEST_TIMEOUT_MS = 30000;

// A port over `sendNative(request)`, which sends one request to the app
// extension and resolves to its answer. `clock` replaces setTimeout,
// clearTimeout and Date.now in tests. Like a port of connectNative's, it has
// postMessage, disconnect, onMessage and onDisconnect; disconnect stops it
// without telling onDisconnect, and onDisconnect's listeners get the port,
// whose `udmReason` says why it closed.
function pollingPort(sendNative, clock = { setTimeout, clearTimeout, now: () => Date.now() }) {
  const messageListeners = [];
  const disconnectListeners = [];
  const queue = [];
  const timers = new Set();
  let sendTimer = null;
  let pollTimer = null;
  let sending = false;
  let polling = false;
  let started = false; // the first request went out
  let ready = false; // its answer came back
  let closed = false;
  let session = null;

  const listeners = (list) => ({
    addListener: (f) => void list.push(f),
    removeListener: (f) => {
      const i = list.indexOf(f);
      if (i >= 0) list.splice(i, 1);
    },
    hasListener: (f) => list.includes(f),
  });

  const port = {
    udmReason: "",
    onMessage: listeners(messageListeners),
    onDisconnect: listeners(disconnectListeners),
    postMessage(message) {
      if (closed) throw new Error("Attempting to use a disconnected port object");
      queue.push(message);
      scheduleSend();
    },
    disconnect() {
      stop();
    },
  };

  function later(f, ms) {
    const id = clock.setTimeout(() => {
      timers.delete(id);
      f();
    }, ms);
    timers.add(id);
    return id;
  }

  function stop() {
    closed = true;
    for (const id of timers) clock.clearTimeout(id);
    timers.clear();
  }

  function close(reason) {
    if (closed) return;
    stop();
    port.udmReason = reason;
    for (const f of [...disconnectListeners]) {
      try {
        f(port);
      } catch (_) {
        // One listener's fault is not the port's.
      }
    }
  }

  // Send `request`, failing after SAFARI_REQUEST_TIMEOUT_MS: a request that
  // never comes back must not leave the port looking connected.
  function ask(request) {
    return new Promise((resolve, reject) => {
      const timer = later(() => reject(new Error("no answer from UDM")), SAFARI_REQUEST_TIMEOUT_MS);
      const settle = (f) => (v) => {
        clock.clearTimeout(timer);
        timers.delete(timer);
        f(v);
      };
      try {
        Promise.resolve(sendNative(request)).then(settle(resolve), settle(reject));
      } catch (e) {
        settle(reject)(e);
      }
    });
  }

  const failed = (e) => close((e && e.message) || String(e || "") || "no answer from UDM");

  // Take an answer: hand on its frames and say how many there were, or -1
  // when the port closed over it.
  function received(reply) {
    if (closed) return -1;
    if (!reply || typeof reply !== "object") {
      close("no answer from UDM");
      return -1;
    }
    if (typeof reply.session === "string" && reply.session) {
      // Another udm-host never heard this port's hello: start over.
      if (session !== null && reply.session !== session) {
        close("UDM's host started again");
        return -1;
      }
      session = reply.session;
    }
    const frames = Array.isArray(reply.frames) ? reply.frames.filter((f) => f && typeof f === "object") : [];
    for (const frame of frames) {
      for (const f of [...messageListeners]) {
        try {
          f(frame, port);
        } catch (_) {
          // As a browser's port does, the next listener and frame still run.
        }
      }
      if (closed) return -1;
    }
    if (reply.error) {
      close(String(reply.error));
      return -1;
    }
    return frames.length;
  }

  function scheduleSend() {
    if (closed || sendTimer !== null) return;
    sendTimer = later(() => {
      sendTimer = null;
      flush();
    }, 0);
  }

  function flush() {
    if (closed || sending) return;
    // Nothing goes beside the first request until it is answered.
    if (started && (!ready || !queue.length)) return;
    sending = true;
    const request = { frames: queue.splice(0) };
    if (!started) request.start = true;
    started = true;
    ask(request).then(
      (reply) => {
        sending = false;
        if (received(reply) < 0) return;
        if (!ready) {
          ready = true;
          schedulePoll(0);
        }
        if (queue.length) scheduleSend();
      },
      (e) => {
        sending = false;
        failed(e);
      },
    );
  }

  function schedulePoll(ms) {
    if (closed || polling || pollTimer !== null) return;
    pollTimer = later(() => {
      pollTimer = null;
      poll();
    }, ms);
  }

  function poll() {
    if (closed || polling) return;
    polling = true;
    const askedAt = clock.now();
    ask({ poll: true }).then(
      (reply) => {
        polling = false;
        const count = received(reply);
        if (count < 0) return;
        const quick = count > 0 || reply.more === true;
        schedulePoll(quick ? 0 : Math.max(0, SAFARI_POLL_GAP_MS - (clock.now() - askedAt)));
      },
      (e) => {
        polling = false;
        failed(e);
      },
    );
  }

  // The first request goes out even when nothing is posted.
  scheduleSend();
  return port;
}

if (typeof module !== "undefined") {
  module.exports = { pollingPort, SAFARI_POLL_GAP_MS, SAFARI_REQUEST_TIMEOUT_MS };
}
