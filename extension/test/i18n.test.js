// Tests for the extension's translations (_locales): every language has the
// messages and placeholders English has, and the messages the code, the
// popup and the manifest name are exactly the English ones. Run with:
// node --test extension/test/*.test.js

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const { uiText } = require("../media.js");
const { getMessage } = require("./harness.js");

const dir = path.join(__dirname, "..");
const read = (file) => fs.readFileSync(path.join(dir, file), "utf8");
const messages = (lang) => JSON.parse(read(path.join("_locales", lang, "messages.json")));
const en = messages("en");
const languages = fs.readdirSync(path.join(dir, "_locales"));

// A message's $NAME$ uses, and its placeholders as "name=content", in order.
const uses = (m) => [...m.message.matchAll(/\$([A-Za-z0-9_]+)\$/g)].map((x) => x[1].toLowerCase()).sort();
const placeholders = (m) =>
  Object.entries(m.placeholders || {})
    .map(([name, p]) => `${name.toLowerCase()}=${p.content}`)
    .sort();

// The extension's own files, without the tests.
const files = fs.readdirSync(dir);
const code = files.filter((f) => f.endsWith(".js")).map(read).join("\n");
const html = files.filter((f) => f.endsWith(".html")).map(read).join("\n");
const manifest = read("manifest.json") + read("firefox.json");
const all = (text, re) => [...text.matchAll(re)].map((m) => m[1]);

// Messages named in code (chrome.i18n.getMessage, popup.js's t and media.js's
// uiText), in the popup's data-i18n attributes, and in the manifest.
const called = all(code, /\b(?:getMessage|uiText|t)\(\s*"([^"]*)"/g);
const attributes = [...html.matchAll(/\bdata-i18n(-[a-z-]+)?="([^"]*)"/g)];
const inManifest = all(manifest, /__MSG_(\w+)__/g);

test("every language has exactly the English messages, with the same placeholders", () => {
  assert.ok(languages.includes("en"));
  assert.ok(languages.length > 1);
  for (const lang of languages) {
    const m = messages(lang);
    assert.deepEqual(Object.keys(m).sort(), Object.keys(en).sort(), `${lang} has other messages than en`);
    for (const [name, english] of Object.entries(en)) {
      assert.ok(typeof m[name].message === "string" && m[name].message.trim(), `${lang} ${name} is empty`);
      assert.deepEqual(placeholders(m[name]), placeholders(english), `${lang} ${name} placeholders`);
      assert.deepEqual(uses(m[name]), uses(english), `${lang} ${name} uses other placeholders`);
      if (english.message.includes("UIDM")) assert.ok(m[name].message.includes("UIDM"), `${lang} ${name} lost UIDM`);
    }
  }
});

test("English messages are well formed", () => {
  for (const [name, m] of Object.entries(en)) {
    assert.match(name, /^[A-Za-z0-9_]+$/);
    assert.ok(m.description, `${name} has no description for translators`);
    // Each placeholder is used, and each use has a placeholder.
    assert.deepEqual([...new Set(uses(m))], Object.keys(m.placeholders || {}).map((p) => p.toLowerCase()).sort(), name);
  }
});

test("every message the extension names is in English", () => {
  assert.ok(called.length > 0 && attributes.length > 0 && inManifest.length > 0);
  for (const name of called) assert.ok(name in en, `${name} (in code) is not in _locales/en`);
  for (const [, attr, name] of attributes) {
    assert.ok([undefined, "-title", "-placeholder", "-aria-label"].includes(attr), `data-i18n${attr} is not filled in`);
    assert.ok(name in en, `${name} (in popup.html) is not in _locales/en`);
  }
  for (const name of inManifest) assert.ok(name in en, `${name} (in the manifest) is not in _locales/en`);
});

test("every English message is used", () => {
  // Lists of message names (popup.js's and content.js's size units) count too.
  const listed = all(code, /\[((?:\s*"[A-Za-z0-9_]+",?)+\s*)\]/g).flatMap((list) => all(list, /"([^"]+)"/g));
  const used = new Set([...called, ...attributes.map((a) => a[2]), ...inManifest, ...listed]);
  for (const name of Object.keys(en)) assert.ok(used.has(name), `${name} is never used`);
});

test("messages fill in their values as the browser does", () => {
  assert.equal(getMessage("statusNotConnectedReason", "no host"), "Not connected to UIDM: no host. Downloads stay in the browser.");
  assert.equal(getMessage("sizeProgress", ["1.5 MB", "10.0 MB"]), "1.5 MB / 10.0 MB");
  assert.equal(getMessage("noDownloads"), "No downloads yet.");
  assert.equal(getMessage("noSuchMessage"), "");
  // media.js's English, used without a browser, is the browser's.
  for (const name of Object.keys(en)) assert.equal(uiText(name, ["A", "B"]), getMessage(name, ["A", "B"]), name);
});
