// Makes the Firefox copy of the extension, as make-firefox.ps1 does on
// Windows, for the macOS and Linux packages:
//
//   node installer/make-firefox.js firefox
//
// The copy is the extension folder without its tests, with manifest.json
// changed by extension/firefox.json: each key there replaces the
// manifest's, and null removes it.

const fs = require("node:fs");
const path = require("node:path");

const out = process.argv[2];
if (!out) {
  console.error("usage: node installer/make-firefox.js <folder>");
  process.exit(2);
}
const source = path.join(__dirname, "..", "extension");

if (fs.existsSync(out)) {
  // Only ever replace an earlier copy, never some other folder.
  if (fs.readdirSync(out).length && !fs.existsSync(path.join(out, "manifest.json"))) {
    console.error(`${out} is not empty and holds no extension`);
    process.exit(1);
  }
  fs.rmSync(out, { recursive: true, force: true });
}
fs.mkdirSync(out, { recursive: true });
for (const name of fs.readdirSync(source)) {
  if (["test", "firefox.json", "manifest.json"].includes(name)) continue;
  fs.cpSync(path.join(source, name), path.join(out, name), { recursive: true });
}

const manifest = JSON.parse(fs.readFileSync(path.join(source, "manifest.json"), "utf8"));
const changes = JSON.parse(fs.readFileSync(path.join(source, "firefox.json"), "utf8"));
for (const [key, value] of Object.entries(changes)) {
  delete manifest[key];
  if (value !== null) manifest[key] = value;
}
fs.writeFileSync(path.join(out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
console.log(`Wrote the Firefox extension to ${out}`);
