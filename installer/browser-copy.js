// What installer/make-firefox.js and installer/make-safari.js share: making
// a browser's copy of extension/chromium and telling whether a copy is still
// what the Chromium extension makes.
//
// A copy is the Chromium extension with manifest.json changed by the
// browser's file of changes (extension/firefox.json, extension/safari.json):
// each key there replaces the manifest's, and null removes it.
//
// Every file must be byte for byte the same and none missing or extra.
// manifest.json is compared as JSON, since make-firefox.ps1 writes it with
// PowerShell's own spacing, key order and escapes.

const fs = require("node:fs");
const path = require("node:path");
const util = require("node:util");

const EXTENSION = path.join(__dirname, "..", "extension");
const SOURCE = path.join(EXTENSION, "chromium");
// What a copy remakes instead of copying.
const SKIPPED = ["manifest.json"];

// The manifest made from `source`'s manifest.json and the file of changes
// `changes`.
function copyManifest(source, changes) {
  const manifest = JSON.parse(fs.readFileSync(path.join(source, "manifest.json"), "utf8"));
  for (const [key, value] of Object.entries(JSON.parse(fs.readFileSync(changes, "utf8")))) {
    delete manifest[key];
    if (value !== null) manifest[key] = value;
  }
  return manifest;
}

// Every file under `dir`, as paths relative to it with "/" between folders,
// sorted, leaving out the top-level names in `skip`.
function listFiles(dir, skip = []) {
  const files = [];
  const walk = (rel) => {
    for (const entry of fs.readdirSync(path.join(dir, rel), { withFileTypes: true })) {
      const name = rel ? `${rel}/${entry.name}` : entry.name;
      if (!rel && skip.includes(entry.name)) continue;
      if (entry.isDirectory()) walk(name);
      else files.push(name);
    }
  };
  walk("");
  return files.sort();
}

// Write the copy of `source` that `changes` describes to `out`, replacing an
// earlier copy.
function makeCopy(out, source, changes) {
  if (fs.existsSync(out)) {
    // Only ever replace an earlier copy, never some other folder.
    if (fs.readdirSync(out).length && !fs.existsSync(path.join(out, "manifest.json"))) {
      throw new Error(`${out} is not empty and holds no extension`);
    }
    fs.rmSync(out, { recursive: true, force: true });
  }
  fs.mkdirSync(out, { recursive: true });
  for (const name of fs.readdirSync(source)) {
    if (SKIPPED.includes(name)) continue;
    fs.cpSync(path.join(source, name), path.join(out, name), { recursive: true });
  }
  fs.writeFileSync(path.join(out, "manifest.json"), JSON.stringify(copyManifest(source, changes), null, 2) + "\n");
}

// How the copy in `folder` differs from what `source` and `changes` make,
// one line per difference; none when it is up to date.
function checkCopy(folder, source, changes) {
  if (!fs.existsSync(folder)) return [`${folder} does not exist`];
  const differences = [];
  const expected = listFiles(source, SKIPPED);
  const found = new Set(listFiles(folder));
  for (const file of expected) {
    if (!found.has(file)) differences.push(`missing: ${file}`);
    else if (!fs.readFileSync(path.join(source, file)).equals(fs.readFileSync(path.join(folder, file)))) {
      differences.push(`changed: ${file}`);
    }
    found.delete(file);
  }
  if (!found.has("manifest.json")) {
    differences.push("missing: manifest.json");
  } else {
    found.delete("manifest.json");
    let manifest;
    try {
      manifest = JSON.parse(fs.readFileSync(path.join(folder, "manifest.json"), "utf8").replace(/^﻿/, ""));
    } catch (e) {
      manifest = undefined;
      differences.push(`changed: manifest.json is not JSON (${e.message})`);
    }
    if (manifest !== undefined) {
      const want = copyManifest(source, changes);
      const keys = [...new Set([...Object.keys(want), ...Object.keys(manifest)])].sort();
      for (const key of keys) {
        if (!util.isDeepStrictEqual(want[key], manifest[key])) differences.push(`changed: manifest.json key "${key}"`);
      }
    }
  }
  for (const file of [...found].sort()) differences.push(`extra: ${file}`);
  return differences;
}

// The command line of a maker: `node installer/<script> [--check] [folder]`.
// `browser` names the copy in what it prints ("Firefox"), `copy` is where it
// is kept, and `make` and `check` take a folder.
function commandLine(script, browser, copy, make, check) {
  const args = process.argv.slice(2);
  const checking = args[0] === "--check";
  const rest = checking ? args.slice(1) : args;
  if (rest.length > 1 || rest.some((a) => a.startsWith("--"))) {
    console.error(`usage: node installer/${script} [--check] [folder]`);
    process.exit(2);
  }
  const folder = rest[0] ?? copy;
  if (checking) {
    const differences = check(folder);
    if (differences.length) {
      console.error(`${folder} is not the ${browser} copy of the current extension:`);
      for (const d of differences) console.error(`  ${d}`);
      console.error(`Remake it with: node installer/${script}${rest.length ? ` ${folder}` : ""}`);
      process.exit(1);
    }
    console.log(`${folder} is up to date with the extension`);
  } else {
    try {
      make(folder);
    } catch (e) {
      console.error(e.message);
      process.exit(1);
    }
    console.log(`Wrote the ${browser} extension to ${folder}`);
  }
}

module.exports = { EXTENSION, SOURCE, copyManifest, listFiles, makeCopy, checkCopy, commandLine };
