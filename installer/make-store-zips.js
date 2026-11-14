// Makes the zips the browser stores take, from the extension in git:
//
//   node installer/make-store-zips.js [folder]
//
// writes, in `folder` (dist by default):
//
//   UDM-<version>-chromium-store.zip  for the Chrome Web Store (and Edge
//                                     Add-ons): extension/chromium, with
//                                     manifest.json's `key` left out
//   UDM-<version>-firefox-store.zip   for addons.mozilla.org: extension/firefox
//
// <version> is the extension's, from its manifest. The Chrome Web Store
// refuses a manifest with a `key` and gives the listed copy an ID of its
// own; the key stays in extension/chromium, so Load unpacked keeps its fixed
// ID. The Firefox copy must be what installer/make-firefox.js makes from the
// Chromium extension, or nothing is written. docs/extension-stores.md says
// how the zips are uploaded, and the Release workflow attaches them to each
// release.
//
// Each zip holds the files at its root (manifest.json at the top), in sorted
// order, with one fixed time, so the same extension makes the same bytes
// with the same Node and zlib version. The extension has no build step: the
// zip is its source too.

const fs = require("node:fs");
const path = require("node:path");
const zlib = require("node:zlib");
const { checkFirefox, COPY } = require("./make-firefox.js");

const ROOT = path.join(__dirname, "..");
const CHROMIUM = path.join(ROOT, "extension", "chromium");
const OUT = path.join(ROOT, "dist");

// 1980-01-01 00:00, the earliest time a zip can hold, in MS-DOS form.
const DOS_TIME = 0;
const DOS_DATE = (0 << 9) | (1 << 5) | 1;
// Bit 11: names are UTF-8.
const UTF8 = 0x0800;

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(data) {
  let c = 0xffffffff;
  for (const byte of data) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

// A zip of `files`, each { name, data } with `name` relative and "/" between
// folders, in the order given. Each is deflated, or stored when deflating
// does not make it smaller.
function zip(files) {
  const locals = [];
  const central = [];
  let offset = 0;
  for (const { name, data } of files) {
    const nameBytes = Buffer.from(name, "utf8");
    const deflated = zlib.deflateRawSync(data, { level: 9 });
    const stored = deflated.length >= data.length;
    const body = stored ? data : deflated;
    const crc = crc32(data);

    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4); // version needed: 2.0
    local.writeUInt16LE(UTF8, 6);
    local.writeUInt16LE(stored ? 0 : 8, 8);
    local.writeUInt16LE(DOS_TIME, 10);
    local.writeUInt16LE(DOS_DATE, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(body.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    local.writeUInt16LE(0, 28); // no extra field
    locals.push(local, nameBytes, body);

    const entry = Buffer.alloc(46);
    entry.writeUInt32LE(0x02014b50, 0);
    entry.writeUInt16LE(20, 4); // made by: MS-DOS, 2.0
    entry.writeUInt16LE(20, 6);
    entry.writeUInt16LE(UTF8, 8);
    entry.writeUInt16LE(stored ? 0 : 8, 10);
    entry.writeUInt16LE(DOS_TIME, 12);
    entry.writeUInt16LE(DOS_DATE, 14);
    entry.writeUInt32LE(crc, 16);
    entry.writeUInt32LE(body.length, 20);
    entry.writeUInt32LE(data.length, 24);
    entry.writeUInt16LE(nameBytes.length, 28);
    // Extra field, comment, disk number, internal and external attributes: none.
    entry.writeUInt32LE(offset, 42);
    central.push(entry, nameBytes);

    offset += local.length + nameBytes.length + body.length;
  }
  const directory = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(directory.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, directory, end]);
}

// Every file under `dir` as { name, data }, sorted by name, leaving out
// hidden files and folders (such as a .DS_Store) and the names in `skip`.
function readFiles(dir, skip = []) {
  const names = [];
  const walk = (rel) => {
    for (const entry of fs.readdirSync(path.join(dir, rel), { withFileTypes: true })) {
      if (entry.name.startsWith(".")) continue;
      const name = rel ? `${rel}/${entry.name}` : entry.name;
      if (entry.isDirectory()) walk(name);
      else if (!skip.includes(name)) names.push(name);
    }
  };
  walk("");
  return names.sort().map((name) => ({ name, data: fs.readFileSync(path.join(dir, ...name.split("/"))) }));
}

// extension/chromium's manifest as the Chrome Web Store takes it: without
// `key`.
function chromiumStoreManifest(source = CHROMIUM) {
  const manifest = JSON.parse(fs.readFileSync(path.join(source, "manifest.json"), "utf8"));
  delete manifest.key;
  return manifest;
}

// The files of each store zip: { chromium, firefox }, with the extension's
// version. Throws when the Firefox copy is not current.
function storeFiles(chromium = CHROMIUM, firefox = COPY) {
  const differences = checkFirefox(firefox, chromium);
  if (differences.length) {
    throw new Error(
      `${firefox} is not the Firefox copy of the current extension (${differences.join(", ")}). ` +
        "Remake it with: node installer/make-firefox.js",
    );
  }
  const manifest = chromiumStoreManifest(chromium);
  const chromiumFiles = readFiles(chromium, ["manifest.json"]);
  chromiumFiles.push({ name: "manifest.json", data: Buffer.from(JSON.stringify(manifest, null, 2) + "\n") });
  chromiumFiles.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  return { version: manifest.version, chromium: chromiumFiles, firefox: readFiles(firefox) };
}

// Write both zips to `out` and return their paths.
function makeStoreZips(out = OUT, chromium = CHROMIUM, firefox = COPY) {
  const files = storeFiles(chromium, firefox);
  fs.mkdirSync(out, { recursive: true });
  const chromiumZip = path.join(out, `UDM-${files.version}-chromium-store.zip`);
  const firefoxZip = path.join(out, `UDM-${files.version}-firefox-store.zip`);
  fs.writeFileSync(chromiumZip, zip(files.chromium));
  fs.writeFileSync(firefoxZip, zip(files.firefox));
  return { chromium: chromiumZip, firefox: firefoxZip };
}

module.exports = { makeStoreZips, chromiumStoreManifest, zip, crc32 };

if (require.main === module) {
  const args = process.argv.slice(2);
  if (args.length > 1 || args.some((a) => a.startsWith("--"))) {
    console.error("usage: node installer/make-store-zips.js [folder]");
    process.exit(2);
  }
  try {
    const made = makeStoreZips(args[0] ?? OUT);
    console.log(`Wrote ${made.chromium}`);
    console.log(`Wrote ${made.firefox}`);
  } catch (e) {
    console.error(e.message);
    process.exit(1);
  }
}
