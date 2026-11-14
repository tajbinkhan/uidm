// Renders assets/icon-1024.png, the icon's source, into every other icon
// file UDM ships: extension/chromium/icons/{16,32,48,128}.png (copied to
// extension/firefox/icons and extension/safari/icons) and
// assets/icon-256.png (the tray and window icon). Run make-icon.js
// afterwards to pack installer/windows/udm.ico.
//
// It scales the picture on a canvas in headless Chrome, halving it step by
// step so the small sizes stay sharp. Set CHROME to the browser
// when it is not in its usual Windows place.
//
//   node installer/render-icons.js

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const root = path.join(__dirname, "..");
const chrome =
  process.env.CHROME || "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe";
const source = fs.readFileSync(path.join(root, "assets", "icon-1024.png"));
const sizes = [16, 32, 48, 128, 256];

const page = `<!doctype html><body><script>
const img = new Image();
img.onload = () => {
  const out = {};
  for (const s of ${JSON.stringify(sizes)}) {
    // Halve until the next halving would pass the size, then draw it.
    let from = img, side = img.width;
    while (side / 2 >= s) {
      const half = document.createElement("canvas");
      half.width = half.height = side / 2;
      const h = half.getContext("2d");
      h.imageSmoothingQuality = "high";
      h.drawImage(from, 0, 0, side / 2, side / 2);
      from = half;
      side /= 2;
    }
    const c = document.createElement("canvas");
    c.width = c.height = s;
    const g = c.getContext("2d");
    g.imageSmoothingQuality = "high";
    g.drawImage(from, 0, 0, s, s);
    out[s] = { png: c.toDataURL("image/png").split(",")[1] };
  }
  const pre = document.createElement("pre");
  pre.id = "out";
  pre.textContent = JSON.stringify(out);
  document.body.appendChild(pre);
};
img.src = "data:image/png;base64,${source.toString("base64")}";
</script></body>`;

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "udm-icon-"));
const html = path.join(dir, "render.html");
fs.writeFileSync(html, page);
const dom = execFileSync(
  chrome,
  ["--headless", "--disable-gpu", `--user-data-dir=${path.join(dir, "profile")}`,"--virtual-time-budget=5000", "--dump-dom", `file:///${html.replace(/\\/g, "/")}`],
  { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
);
fs.rmSync(dir, { recursive: true, force: true });

const match = dom.match(/<pre id="out">([^<]*)<\/pre>/);
if (!match) throw new Error("Chrome did not render the icon");
const out = JSON.parse(match[1].replace(/&quot;/g, '"').replace(/&amp;/g, "&"));

const write = (file, data) => {
  fs.writeFileSync(path.join(root, file), data);
  console.log(`wrote ${file} (${data.length} bytes)`);
};
for (const s of [16, 32, 48, 128]) write(`extension/chromium/icons/${s}.png`, Buffer.from(out[s].png, "base64"));
write("assets/icon-256.png", Buffer.from(out[256].png, "base64"));
require("./make-firefox.js").makeFirefox();
console.log("remade extension/firefox");
require("./make-safari.js").makeSafari();
console.log("remade extension/safari");
