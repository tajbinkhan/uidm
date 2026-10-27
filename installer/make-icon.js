// Packs the extension's PNG icons into installer/uidm.ico, the icon of the
// setup program, its shortcuts and its entry in Installed apps. Windows reads
// PNG images inside an .ico directly.
//
//   node installer/make-icon.js

const fs = require("node:fs");
const path = require("node:path");

const icons = path.join(__dirname, "..", "extension", "icons");
const images = [16, 32, 48, 128].map((size) => fs.readFileSync(path.join(icons, `${size}.png`)));

const header = Buffer.alloc(6);
header.writeUInt16LE(0, 0); // reserved
header.writeUInt16LE(1, 2); // type: icon
header.writeUInt16LE(images.length, 4);

let offset = header.length + 16 * images.length;
const entries = images.map((png) => {
  const e = Buffer.alloc(16);
  const width = png.readUInt32BE(16);
  const height = png.readUInt32BE(20);
  e.writeUInt8(width % 256, 0); // 0 means 256
  e.writeUInt8(height % 256, 1);
  e.writeUInt8(0, 2); // no palette
  e.writeUInt8(0, 3);
  e.writeUInt16LE(1, 4); // color planes
  e.writeUInt16LE(32, 6); // bits per pixel
  e.writeUInt32LE(png.length, 8);
  e.writeUInt32LE(offset, 12);
  offset += png.length;
  return e;
});

const out = path.join(__dirname, "uidm.ico");
fs.writeFileSync(out, Buffer.concat([header, ...entries, ...images]));
console.log(`wrote ${out} (${offset} bytes)`);
