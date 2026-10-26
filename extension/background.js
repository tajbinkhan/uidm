// Service worker: owns the one long-lived port to the native host.
const HOST = "com.uidm.host";

let port = null;

function connect() {
  port = chrome.runtime.connectNative(HOST);
  port.onMessage.addListener((msg) => {
    console.log("[uidm] from host:", msg);
  });
  port.onDisconnect.addListener(() => {
    console.warn("[uidm] host disconnected:", chrome.runtime.lastError?.message);
    port = null;
  });
  // Handshake so you can see a round trip immediately in the host's stderr log.
  port.postMessage({ op: "hello", version: chrome.runtime.getManifest().version });
  return port;
}

function send(message) {
  if (!port) connect();
  port.postMessage(message);
}

// Let the popup (and later, the capture logic) hand work to the host.
chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  send(message);
  sendResponse({ sent: true });
  return true;
});

connect();
