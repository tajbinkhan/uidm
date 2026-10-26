// Downloads started by a form POST.
//
// The downloads API only reports a URL, so UIDM would fetch it with GET and
// often save an error page instead. The service worker records the method
// and body of recent requests in frames (webRequest), and these functions
// match a new download to one and rebuild its body. When a download's
// request cannot be rebuilt, it stays in the browser.

// How long a recorded form request can match a download.
const POST_MATCH_MS = 60 * 1000;
// Form requests kept at once.
const POST_KEEP = 50;

// Base64 of bytes, without btoa's Latin-1 limits.
function bytesToBase64(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return btoa(s);
}

function textToBase64(text) {
  return bytesToBase64(new TextEncoder().encode(text));
}

// application/x-www-form-urlencoded from webRequest's parsed formData, in the
// order the browser gave.
function encodeForm(formData) {
  const parts = [];
  for (const [name, values] of Object.entries(formData)) {
    for (const value of values) {
      parts.push(`${encodeURIComponent(name)}=${encodeURIComponent(value)}`.replace(/%20/g, "+"));
    }
  }
  return parts.join("&");
}

// What to send for a recorded request: { method: "POST", body, contentType,
// text } with `body` base64-encoded and `text` the body as a string when it
// is one (for handing it back to the browser); or { leave: reason } when it
// cannot be rebuilt.
function rebuildPost(record) {
  const type = String(record.contentType || "").split(";")[0].trim().toLowerCase();
  const body = record.requestBody;
  if (!body || body.error) return { leave: "the browser did not share the form" };
  if (body.formData) {
    // Chrome parses both urlencoded and multipart forms into formData; only
    // the first can be rebuilt as sent (multipart may carry files).
    if (type && type !== "application/x-www-form-urlencoded") {
      return { leave: `a ${type} form cannot be resent` };
    }
    const text = encodeForm(body.formData);
    return {
      method: "POST",
      body: textToBase64(text),
      text,
      contentType: "application/x-www-form-urlencoded",
    };
  }
  if (Array.isArray(body.raw)) {
    const chunks = [];
    for (const part of body.raw) {
      if (!part.bytes) return { leave: "the form has a file in it" };
      chunks.push(new Uint8Array(part.bytes));
    }
    const all = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
    let at = 0;
    for (const c of chunks) {
      all.set(c, at);
      at += c.length;
    }
    return { method: "POST", body: bytesToBase64(all), contentType: type || "application/octet-stream" };
  }
  // A POST with no body at all.
  return { method: "POST", body: "", text: "", contentType: type || undefined };
}

// The recorded POST a new download came from: the latest to the same URL,
// recently. `null` means it was a GET.
function findPost(records, url, now) {
  for (let i = records.length - 1; i >= 0; i--) {
    const r = records[i];
    if (now - r.time > POST_MATCH_MS) break;
    if (r.url === url) return r;
  }
  return null;
}

// Drop old records and keep the list short.
function prunePosts(records, now) {
  while (records.length && (now - records[0].time > POST_MATCH_MS || records.length > POST_KEEP)) {
    records.shift();
  }
}

if (typeof module !== "undefined") {
  module.exports = { encodeForm, rebuildPost, findPost, prunePosts, textToBase64, POST_MATCH_MS };
}
