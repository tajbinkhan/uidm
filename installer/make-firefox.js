// Makes extension/firefox, the Firefox copy of extension/chromium, as
// make-firefox.ps1 does in PowerShell:
//
//   node installer/make-firefox.js
//
// The copy is the Chromium extension with manifest.json changed by
// extension/firefox.json: each key there replaces the manifest's, and null
// removes it. Chrome warns about Firefox's keys in Manifest V3, so the two
// cannot share one manifest.
//
// Git keeps the copy, so it can fall behind the Chromium extension, as an
// uncommitted copy once did (#149). This tells whether a copy is still what
// the Chromium extension makes, listing what differs, and exits with 1 if it
// is not:
//
//   node installer/make-firefox.js --check
//
// The extension's tests and CI run the check on extension/firefox. Both
// commands also take another folder, such as a package's staging folder.
// installer/browser-copy.js does the work, for the Safari copy too
// (installer/make-safari.js).

const path = require("node:path");
const { EXTENSION, SOURCE, copyManifest, makeCopy, checkCopy, commandLine } = require("./browser-copy.js");

const CHANGES = path.join(EXTENSION, "firefox.json");
const COPY = path.join(EXTENSION, "firefox");

// The Firefox manifest made from `source`'s manifest.json and firefox.json.
function firefoxManifest(source = SOURCE, changes = CHANGES) {
  return copyManifest(source, changes);
}

// Write the Firefox copy of `source` to `out`, replacing an earlier copy.
function makeFirefox(out = COPY, source = SOURCE) {
  makeCopy(out, source, CHANGES);
}

// How the Firefox copy in `folder` differs from what `source` makes, one
// line per difference; none when it is up to date.
function checkFirefox(folder = COPY, source = SOURCE) {
  return checkCopy(folder, source, CHANGES);
}

module.exports = { makeFirefox, checkFirefox, firefoxManifest, COPY };

if (require.main === module) {
  commandLine("make-firefox.js", "Firefox", COPY, (f) => makeFirefox(f), (f) => checkFirefox(f));
}
