// Downloads started by a form POST.
//
// The downloads API only reports a URL, so UDM would fetch it with GET and
// often save an error page instead. The service worker records the method
// and body of recent requests in frames and of scripts' requests
// (webRequest), and these functions match a new download to one and
// rebuild its body: a urlencoded or multipart form from the fields the
// browser parsed, anything else from its bytes. When a download's request
// cannot be rebuilt, it stays in the browser.

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

// A boundary for a rebuilt multipart body, unlikely to be in any value.
function multipartBoundary() {
  let random = "";
  for (let i = 0; i < 24; i++) random += Math.floor(Math.random() * 16).toString(16);
  return `----UdmFormBoundary${random}`;
}

// multipart/form-data from webRequest's parsed formData, with `boundary`,
// leaving out the fields named in `skip` (the form's file fields: their
// files are not in formData and cannot be read back). Names and values are
// written as the browser writes them, quotes and line breaks escaped.
function encodeMultipart(formData, boundary, skip = []) {
  const escape = (s) => String(s).replace(/"/g, "%22").replace(/\r/g, "%0D").replace(/\n/g, "%0A");
  let out = "";
  for (const [name, values] of Object.entries(formData)) {
    if (skip.includes(name)) continue;
    for (const value of values) {
      out += `--${boundary}\r\nContent-Disposition: form-data; name="${escape(name)}"\r\n\r\n${value}\r\n`;
    }
  }
  return `${out}--${boundary}--\r\n`;
}

// What to send for a recorded request: { method: "POST", body, contentType,
// text } with `body` base64-encoded and `text` the body as a string when it
// is one (for handing it back to the browser); or { leave: reason } when it
// cannot be rebuilt. `record.files`, when the form's page said (see
// content.js), is { names, chosen }: its file fields, and whether a file
// was chosen in any.
function rebuildPost(record, boundary = multipartBoundary()) {
  const type = String(record.contentType || "").split(";")[0].trim().toLowerCase();
  const body = record.requestBody;
  if (!body || body.error) return { leave: "the browser did not share the form" };
  if (body.formData) {
    // The browser parses both urlencoded and multipart forms into formData,
    // without the files. A multipart form is rebuilt without its file
    // fields; one that sends a file stays in the browser, which has it.
    if (type === "multipart/form-data") {
      const files = record.files || { names: [], chosen: false };
      if (files.chosen) return { leave: "the form sends a file" };
      const text = encodeMultipart(body.formData, boundary, files.names || []);
      const contentType = `multipart/form-data; boundary=${boundary}`;
      return { method: "POST", body: textToBase64(text), text, contentType };
    }
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
    // A body the browser did not parse (a script's XMLHttpRequest or fetch,
    // a text/plain form), sent as its bytes.
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
// recently. `null` means it was a GET. A script's request (`type`
// "xmlhttprequest") never matches by its address alone: the page's own
// navigation to it is a GET. It matches only by its request id.
function findPost(records, url, now) {
  for (let i = records.length - 1; i >= 0; i--) {
    const r = records[i];
    if (now - r.time > POST_MATCH_MS) break;
    if (r.url === url && r.type !== "xmlhttprequest") return r;
  }
  return null;
}

// The recorded POST with request id `id`, if any.
function postById(records, id) {
  return records.find((r) => r.id === id) || null;
}

// Drop old records and keep the list short.
function prunePosts(records, now) {
  while (records.length && (now - records[0].time > POST_MATCH_MS || records.length > POST_KEEP)) {
    records.shift();
  }
}

if (typeof module !== "undefined") {
  module.exports = { encodeForm, encodeMultipart, rebuildPost, findPost, postById, prunePosts, textToBase64, POST_MATCH_MS };
}
