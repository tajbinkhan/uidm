// installer/make-store-zips.js, which makes the zips the Chrome Web Store
// and addons.mozilla.org take (#248). The zips are read back here with a
// reader of their own, not the writer's code.

const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const zlib = require("node:zlib");
const { makeStoreZips } = require("../../installer/make-store-zips.js");
const { makeFirefox } = require("../../installer/make-firefox.js");

const CHROMIUM = path.join(__dirname, "..", "chromium");
const FIREFOX = path.join(__dirname, "..", "firefox");

// A new temporary folder, removed after the test.
function tempDir(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "udm-store-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

// The entries of the zip in `buf`, in the central directory's order, as
// { name, data, time, date }, each checked against its local header and CRC.
function readZip(buf) {
  const end = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  assert.ok(end >= 0, "no end of central directory");
  const count = buf.readUInt16LE(end + 10);
  let at = buf.readUInt32LE(end + 16);
  const entries = [];
  for (let i = 0; i < count; i++) {
    assert.equal(buf.readUInt32LE(at), 0x02014b50, "central directory entry");
    const method = buf.readUInt16LE(at + 10);
    const time = buf.readUInt16LE(at + 12);
    const date = buf.readUInt16LE(at + 14);
    const crc = buf.readUInt32LE(at + 16);
    const size = buf.readUInt32LE(at + 20);
    const length = buf.readUInt32LE(at + 24);
    const nameLength = buf.readUInt16LE(at + 28);
    const extra = buf.readUInt16LE(at + 30);
    const comment = buf.readUInt16LE(at + 32);
    const offset = buf.readUInt32LE(at + 42);
    const name = buf.toString("utf8", at + 46, at + 46 + nameLength);
    at += 46 + nameLength + extra + comment;

    assert.equal(buf.readUInt32LE(offset), 0x04034b50, `${name}: local header`);
    const localName = buf.readUInt16LE(offset + 26);
    const localExtra = buf.readUInt16LE(offset + 28);
    assert.equal(buf.toString("utf8", offset + 30, offset + 30 + localName), name);
    const start = offset + 30 + localName + localExtra;
    const body = buf.subarray(start, start + size);
    assert.ok(method === 0 || method === 8, `${name}: method ${method}`);
    const data = method === 8 ? zlib.inflateRawSync(body) : Buffer.from(body);
    assert.equal(data.length, length, `${name}: size`);
    assert.equal(zlib.crc32(data), crc, `${name}: CRC`);
    entries.push({ name, data, time, date });
  }
  return entries;
}

// Every file under `dir`, relative, with "/" between folders, leaving out
// hidden files and folders as the zips do.
function filesUnder(dir, rel = "") {
  return fs.readdirSync(path.join(dir, rel), { withFileTypes: true }).flatMap((e) => {
    if (e.name.startsWith(".")) return [];
    const name = rel ? `${rel}/${e.name}` : e.name;
    return e.isDirectory() ? filesUnder(dir, name) : [name];
  });
}

test("the store zips are named after the extension's version and hold its files at the root", (t) => {
  const out = tempDir(t);
  const version = JSON.parse(fs.readFileSync(path.join(CHROMIUM, "manifest.json"), "utf8")).version;
  const made = makeStoreZips(out);
  assert.deepEqual(made, {
    chromium: path.join(out, `UDM-${version}-chromium-store.zip`),
    firefox: path.join(out, `UDM-${version}-firefox-store.zip`),
  });

  for (const [zipFile, source] of [
    [made.chromium, CHROMIUM],
    [made.firefox, FIREFOX],
  ]) {
    const entries = readZip(fs.readFileSync(zipFile));
    const names = entries.map((e) => e.name);
    assert.deepEqual(names, [...names].sort(), "sorted");
    assert.deepEqual(names, filesUnder(source).sort());
    assert.ok(names.includes("manifest.json") && names.includes("background.js") && names.includes("_locales/en/messages.json"));
    for (const e of entries) {
      assert.ok(!e.name.startsWith("/") && !e.name.includes("\\"), e.name);
      assert.equal(e.time, 0, `${e.name}: one fixed time`);
      assert.equal(e.date, (1 << 5) | 1, `${e.name}: one fixed date`);
      if (e.name !== "manifest.json" || source === FIREFOX) {
        assert.ok(e.data.equals(fs.readFileSync(path.join(source, e.name))), `${e.name} is the file in git`);
      }
    }
  }
});

test("the Chrome Web Store's manifest has no key, and Firefox's keeps its gecko ID", (t) => {
  const made = makeStoreZips(tempDir(t));
  const manifest = (file) => JSON.parse(readZip(fs.readFileSync(file)).find((e) => e.name === "manifest.json").data);

  const chromium = manifest(made.chromium);
  const unpacked = JSON.parse(fs.readFileSync(path.join(CHROMIUM, "manifest.json"), "utf8"));
  assert.ok(unpacked.key, "the unpacked extension keeps its key, and so its fixed ID");
  assert.equal("key" in chromium, false);
  delete unpacked.key;
  assert.deepEqual(chromium, unpacked);
  assert.deepEqual(chromium.background, { service_worker: "background.js" });

  const firefox = manifest(made.firefox);
  assert.equal(firefox.browser_specific_settings.gecko.id, "udm@udm.local");
  assert.equal("key" in firefox, false);
  assert.equal(firefox.version, chromium.version);
});

test("the same extension makes the same zips", (t) => {
  const first = makeStoreZips(tempDir(t));
  const second = makeStoreZips(tempDir(t));
  assert.ok(fs.readFileSync(first.chromium).equals(fs.readFileSync(second.chromium)));
  assert.ok(fs.readFileSync(first.firefox).equals(fs.readFileSync(second.firefox)));
});

test("no zip is made from a Firefox copy older than the extension", (t) => {
  const dir = tempDir(t);
  const copy = path.join(dir, "firefox");
  makeFirefox(copy);
  fs.writeFileSync(path.join(copy, "media.js"), "// old\n");
  const out = path.join(dir, "out");
  assert.throws(() => makeStoreZips(out, CHROMIUM, copy), /changed: media\.js.*make-firefox\.js/);
  assert.equal(fs.existsSync(out), false);
});

test("hidden files stay out of the zips", (t) => {
  const dir = tempDir(t);
  const source = path.join(dir, "chromium");
  fs.cpSync(CHROMIUM, source, { recursive: true });
  fs.writeFileSync(path.join(source, ".DS_Store"), "");
  const copy = path.join(dir, "firefox");
  makeFirefox(copy, source);
  const made = makeStoreZips(path.join(dir, "out"), source, copy);
  for (const file of [made.chromium, made.firefox]) {
    assert.equal(readZip(fs.readFileSync(file)).some((e) => e.name.startsWith(".")), false);
  }
});

test("the extension's version is the workspace's, so the release tag check cannot be the first to see them apart", () => {
  const manifest = JSON.parse(fs.readFileSync(path.join(CHROMIUM, "manifest.json"), "utf8"));
  const cargo = fs.readFileSync(path.join(__dirname, "..", "..", "Cargo.toml"), "utf8");
  const workspace = cargo.slice(cargo.indexOf("[workspace.package]"));
  const version = /^version\s*=\s*"([^"]+)"/m.exec(workspace);
  assert.ok(version, "no version under [workspace.package] in Cargo.toml");
  // A browser's version has only numbers: a pre-release's 0.6.0-rc.1 is 0.6.0.
  assert.equal(manifest.version, version[1].split("-")[0]);
});
