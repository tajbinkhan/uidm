// Makes extension/safari, the Safari copy of extension/chromium, as
// installer/make-firefox.js makes the Firefox one:
//
//   node installer/make-safari.js
//
// The copy is the Chromium extension with manifest.json changed by
// extension/safari.json: each key there replaces the manifest's, and null
// removes it. Safari leaves out the permissions it does not have (the
// downloads API, the proxy settings, notifications) and the Chrome key.
//
// Git keeps the copy, as it keeps the Firefox one, so this tells whether a
// copy is still what the Chromium extension makes, listing what differs, and
// exits with 1 if it is not:
//
//   node installer/make-safari.js --check
//
// The extension's tests run the check on extension/safari, and
// installer/macos/package.sh refuses a stale copy before Xcode's
// safari-web-extension-converter makes the app extension from it.

const path = require("node:path");
const { EXTENSION, SOURCE, copyManifest, makeCopy, checkCopy, commandLine } = require("./browser-copy.js");

const CHANGES = path.join(EXTENSION, "safari.json");
const COPY = path.join(EXTENSION, "safari");

// The Safari manifest made from `source`'s manifest.json and safari.json.
function safariManifest(source = SOURCE, changes = CHANGES) {
  return copyManifest(source, changes);
}

// Write the Safari copy of `source` to `out`, replacing an earlier copy.
function makeSafari(out = COPY, source = SOURCE) {
  makeCopy(out, source, CHANGES);
}

// How the Safari copy in `folder` differs from what `source` makes, one
// line per difference; none when it is up to date.
function checkSafari(folder = COPY, source = SOURCE) {
  return checkCopy(folder, source, CHANGES);
}

module.exports = { makeSafari, checkSafari, safariManifest, COPY };

if (require.main === module) {
  commandLine("make-safari.js", "Safari", COPY, (f) => makeSafari(f), (f) => checkSafari(f));
}
