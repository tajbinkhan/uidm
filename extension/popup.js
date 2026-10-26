// The popup talks to the service worker, which owns the native port.
document.getElementById("add").addEventListener("click", () => {
  const url = document.getElementById("url").value;
  chrome.runtime.sendMessage({
    op: "add",
    url,
    method: "GET",
    headers: { "User-Agent": navigator.userAgent },
    referrer: "https://example.com/",
    user_agent: navigator.userAgent,
  });
});

document.getElementById("ping").addEventListener("click", () => {
  chrome.runtime.sendMessage({ op: "ping" });
});
