#!/usr/bin/env node
// Take the screenshots in docs/images/ again, in light and dark, as WebP:
//
//     node tools/screenshots/shoot.js                 # every shot
//     node tools/screenshots/shoot.js main video      # only these
//     node tools/screenshots/shoot.js --scale 1.5     # a smaller picture
//     node tools/screenshots/shoot.js --out /tmp/shots  # elsewhere than docs/images
//
// The windows are UDM's real frontend drawn by its preview page
// (frontend/dev/), which feeds each window the snapshot the shell's own
// projections made from a sample list (frontend/test/fixtures/views.json,
// written by crates/ui/src/fixtures.rs). After changing a window, run
// `pnpm --dir frontend install` once, regenerate the fixtures if the shell
// changed (`UDM_WRITE_FIXTURES=1 cargo test -p udm-ui fixtures`), then
// this. The video button is the browser extension's own content script on
// a sample page, with its service worker's menu (media.js) worked out here
// from a sample stream, since the real worker needs UDM and a stream server.
//
// Needs Node 20 or later and Playwright with its Chromium, from anywhere
// Node's `require` finds it (`npm install -g playwright` and
// `npx playwright install chromium`, then NODE_PATH set to the global
// node_modules). Nothing is added to the project's package.json files. The
// shots use the fonts the machine has: on Windows the window's own Segoe UI,
// elsewhere what system-ui gives. See tools/screenshots/README.md.
"use strict";

const { spawn } = require("node:child_process");
const fs = require("node:fs");
const http = require("node:http");
const net = require("node:net");
const path = require("node:path");

const ROOT = path.resolve(__dirname, "..", "..");
const FRONTEND = path.join(ROOT, "frontend");
const EXTENSION = path.join(ROOT, "extension", "chromium");
const OUT = path.join(ROOT, "docs", "images");
const USAGE = "usage: node tools/screenshots/shoot.js [--scale N] [--quality Q] [--out DIR]";
const LOOKS = ["light", "dark"];

// Each window at the size the shell opens it (Label::size in
// crates/ui/src/windows.rs); the main window, which the user sizes, a
// little larger, and the download window with its details shown.
const WINDOWS = {
  main: { size: [1100, 640] },
  "save-as": { size: [576, 206] },
  download: { size: [552, 413], prepare: showDetails },
  // A torrent's download window (its view is the fixtures' download-torrent),
  // on its Files tab.
  torrent: { view: "download-torrent", size: [700, 400], prepare: showFiles },
  options: { size: [436, 505] },
  scheduler: { size: [657, 464], prepare: planOvernight },
};
const SHOTS = [...Object.keys(WINDOWS), "video"];

function parseArgs(argv) {
  const opts = { scale: 2, quality: 0.9, out: OUT, only: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--scale") opts.scale = Number(argv[++i]);
    else if (a === "--quality") opts.quality = Number(argv[++i]);
    else if (a === "--out") {
      if (!argv[i + 1]) throw new Error("--out needs a folder");
      opts.out = path.resolve(argv[++i]);
    } else if (a === "-h" || a === "--help") {
      process.stdout.write(`${USAGE} [${SHOTS.join("|")}]...\n`);
      process.exit(0);
    } else if (SHOTS.includes(a)) opts.only.push(a);
    else throw new Error(`unknown argument ${a}; the shots are ${SHOTS.join(", ")}`);
  }
  if (!(opts.scale > 0) || !(opts.quality > 0 && opts.quality <= 1)) throw new Error("--scale must be above 0, --quality in (0, 1]");
  if (opts.only.length === 0) opts.only = SHOTS;
  return opts;
}

function loadPlaywright() {
  try {
    return require("playwright");
  } catch (_) {
    throw new Error("Playwright is not installed where Node can find it; see tools/screenshots/README.md");
  }
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

// Vite's dev server for frontend/, which serves the preview page. The
// production build is left alone.
async function startVite() {
  const port = await freePort();
  const vite = path.join(FRONTEND, "node_modules", "vite", "bin", "vite.js");
  if (!fs.existsSync(vite)) throw new Error("frontend/node_modules is missing: run pnpm --dir frontend install");
  const child = spawn(process.execPath, [vite, "--port", String(port), "--strictPort", "--host", "127.0.0.1"], {
    cwd: FRONTEND,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let log = "";
  child.stdout.on("data", (d) => (log += d));
  child.stderr.on("data", (d) => (log += d));
  const base = `http://127.0.0.1:${port}`;
  const deadline = Date.now() + 60_000;
  for (;;) {
    if (child.exitCode !== null) throw new Error(`Vite stopped:\n${log}`);
    const up = await new Promise((resolve) => {
      http.get(`${base}/dev/index.html`, (res) => (res.resume(), resolve(res.statusCode === 200))).on("error", () => resolve(false));
    });
    if (up) return { base, stop: () => child.kill() };
    if (Date.now() > deadline) {
      child.kill();
      throw new Error(`Vite did not answer in 60 s:\n${log}`);
    }
    await new Promise((r) => setTimeout(r, 250));
  }
}

// What the user does before the shot.
async function showDetails(page) {
  await page.getByRole("button", { name: "Show details >>" }).click();
}

async function showFiles(page) {
  await page.getByRole("tab", { name: "Files" }).click();
}

async function planOvernight(page) {
  await page.getByRole("checkbox", { name: "Start download at", exact: true }).check();
  await page.getByRole("checkbox", { name: "Turn off computer when done", exact: true }).check();
}

// Encode a PNG as WebP in the browser, which needs no image library here.
async function toWebp(encoder, png, quality) {
  const dataUrl = await encoder.evaluate(
    async ([src, q]) => {
      const img = new Image();
      img.src = src;
      await img.decode();
      const canvas = document.createElement("canvas");
      canvas.width = img.naturalWidth;
      canvas.height = img.naturalHeight;
      canvas.getContext("2d").drawImage(img, 0, 0);
      return canvas.toDataURL("image/webp", q);
    },
    [`data:image/png;base64,${png.toString("base64")}`, quality],
  );
  if (!dataUrl.startsWith("data:image/webp;")) throw new Error("this Chromium cannot encode WebP");
  return Buffer.from(dataUrl.slice(dataUrl.indexOf(",") + 1), "base64");
}

async function save(encoder, name, png, opts) {
  const webp = await toWebp(encoder, png, opts.quality);
  const file = path.join(opts.out, `${name}.webp`);
  fs.writeFileSync(file, webp);
  process.stdout.write(`${path.relative(process.cwd(), file) || file}  ${(webp.length / 1024).toFixed(0)} KB\n`);
}

async function shootWindow(browser, encoder, base, kind, look, opts) {
  const { size, prepare, view } = WINDOWS[kind];
  const context = await browser.newContext({
    viewport: { width: size[0], height: size[1] },
    deviceScaleFactor: opts.scale,
    colorScheme: look,
    reducedMotion: "reduce",
  });
  const page = await context.newPage();
  const problems = [];
  page.on("pageerror", (e) => problems.push(String(e)));
  await page.goto(`${base}/dev/index.html?window=${view ?? kind}&look=${look}`);
  // The snapshot arrives just after the page loads; wait for the window to
  // draw it and for its fonts.
  await page.waitForFunction(() => (document.querySelector("#app")?.childElementCount ?? 0) > 0);
  await page.evaluate(() => document.fonts.ready);
  if (prepare) await prepare(page);
  // Nothing in focus or under the mouse, so no ring or hover shows.
  await page.mouse.move(size[0] - 1, size[1] - 1);
  await page.evaluate(() => document.activeElement instanceof HTMLElement && document.activeElement.blur());
  await page.waitForTimeout(300);
  if (problems.length) throw new Error(`${kind} (${look}): ${problems.join("; ")}`);
  await save(encoder, `${kind}-${look}`, await page.screenshot({ type: "png" }), opts);
  await context.close();
}

// The video page's height, which shows the text under the player whole.
const VIDEO_HEIGHT = 660;

// The sample page the video button is shown on: a heading, a player with
// a still picture drawn on a canvas, and some text, light or dark as the
// browser asks.
const VIDEO_PAGE = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Open Movie: the making of</title>
<meta property="og:site_name" content="Sample videos">
<style>
  :root { color-scheme: light dark; --bg: #ffffff; --fg: #1f2328; --muted: #59636e; --line: #d1d9e0; }
  @media (prefers-color-scheme: dark) { :root { --bg: #0d1117; --fg: #e6edf3; --muted: #9198a1; --line: #3d444d; } }
  body { margin: 0; background: var(--bg); color: var(--fg); font: 15px system-ui, sans-serif; }
  header { padding: 14px 28px; border-bottom: 1px solid var(--line); font-weight: 600; }
  main { max-width: 820px; margin: 22px auto; padding: 0 20px; }
  h1 { font-size: 22px; margin: 0 0 14px; }
  video { display: block; width: 100%; aspect-ratio: 16 / 9; background: #000; border-radius: 6px; }
  p { color: var(--muted); line-height: 1.5; }
</style></head>
<body><header>Sample videos</header>
<main><h1>Open Movie: the making of</h1><video id="v" controls preload="none"></video>
<p>Ninety minutes behind the scenes of an open movie, from the first sketches to the final render. Streamed in up to 1080p.</p></main>
<script>
  // The player's still: a sky, hills and a sun, drawn here so the page
  // loads nothing.
  const c = document.createElement("canvas");
  c.width = 1280; c.height = 720;
  const g = c.getContext("2d");
  const sky = g.createLinearGradient(0, 0, 0, 720);
  sky.addColorStop(0, "#1d3b6b"); sky.addColorStop(0.6, "#e8875b"); sky.addColorStop(1, "#f6c47a");
  g.fillStyle = sky; g.fillRect(0, 0, 1280, 720);
  g.fillStyle = "#ffe3a3"; g.beginPath(); g.arc(880, 430, 90, 0, Math.PI * 2); g.fill();
  g.fillStyle = "#2d4a3a"; g.beginPath(); g.moveTo(0, 560); g.quadraticCurveTo(320, 380, 700, 540); g.quadraticCurveTo(1000, 640, 1280, 500); g.lineTo(1280, 720); g.lineTo(0, 720); g.fill();
  g.fillStyle = "#1b3226"; g.beginPath(); g.moveTo(0, 650); g.quadraticCurveTo(500, 520, 1280, 660); g.lineTo(1280, 720); g.lineTo(0, 720); g.fill();
  document.getElementById("v").poster = c.toDataURL("image/png");
</script></body></html>`;

// What the worker's menu holds for the sample page: an HLS stream the
// player loaded, with the qualities UDM reports for it (the labels
// udm_domain::media::quality_label makes) and a subtitle track, run through
// the extension's own mediaMenu.
function sampleMenu() {
  const { mediaMenu } = require(path.join(EXTENSION, "media.js"));
  const master = "https://videos.example.com/open-movie/master.m3u8";
  const entries = [{ id: 1, url: master, kind: "hls", mime: "application/vnd.apple.mpegurl", size: 0, time: 1 }];
  const minutes = 90 * 60;
  const video = (height, mbps) => ({
    id: `https://videos.example.com/open-movie/${height}/index.m3u8`,
    label: `${height}p, ${mbps.toFixed(1)} Mbps`,
    size: Math.round((mbps * 1e6 * minutes) / 8),
  });
  const infos = new Map([
    [
      master,
      {
        kind: "hls",
        video: [video(1080, 5.0), video(720, 2.8), video(480, 1.4)],
        audio: [],
        subtitles: [
          { id: "en", label: "English", default: true },
          { id: "de", label: "Deutsch", default: false },
        ],
        drm: false,
      },
    ],
  ]);
  // The sound alone in its first two formats is enough to show.
  return mediaMenu(entries, infos).map((item) => ({ ...item, options: item.options.slice(0, 5) }));
}

// The extension's content scripts need a few of chrome.*; this answers as
// its service worker does when UDM is connected (onContentMessage, in
// background.js), with the English messages.
function chromeStub(messages, menu) {
  return `(() => {
    const messages = ${JSON.stringify(messages)};
    const menu = ${JSON.stringify(menu)};
    const getMessage = (name, subs) => {
      const m = messages[name];
      if (!m) return "";
      const args = [].concat(subs ?? []);
      const fill = (p) => m.placeholders[p.toLowerCase()].content.replace(/\\$(\\d)/g, (_, i) => args[i - 1]);
      return m.message.replace(/\\$(\\w+)\\$/g, (_, p) => fill(p));
    };
    const answer = (msg) => {
      switch (msg && msg.type) {
        case "video:enabled": return { enabled: true, view: "full", protectedNote: true };
        case "video:menu": return { connected: true, items: menu };
        default: return { ok: true };
      }
    };
    globalThis.chrome = {
      i18n: { getMessage, getUILanguage: () => "en" },
      runtime: { sendMessage: async (msg) => answer(msg), onMessage: { addListener() {} } },
      storage: { local: { get: async () => ({}) }, onChanged: { addListener() {} } },
    };
  })();`;
}

async function shootVideo(browser, encoder, look, opts) {
  const messages = JSON.parse(fs.readFileSync(path.join(EXTENSION, "_locales", "en", "messages.json"), "utf8"));
  const context = await browser.newContext({
    viewport: { width: 960, height: VIDEO_HEIGHT },
    deviceScaleFactor: opts.scale,
    colorScheme: look,
    reducedMotion: "reduce",
  });
  // The page is answered here, so nothing reaches the network.
  await context.route("**/*", (route) => {
    const url = route.request().url();
    if (url === "https://videos.example.com/open-movie") return route.fulfill({ contentType: "text/html", body: VIDEO_PAGE });
    return route.abort();
  });
  const page = await context.newPage();
  const problems = [];
  page.on("pageerror", (e) => problems.push(String(e)));
  await page.goto("https://videos.example.com/open-movie");
  await page.waitForFunction(() => !!document.getElementById("v").poster);
  await page.addScriptTag({ content: chromeStub(messages, sampleMenu()) });
  // What the page holds before the extension's scripts run: the button's
  // host is the element they add to it that shows.
  await page.evaluate(() => {
    globalThis.shotBefore = new Set(document.body.children);
  });
  // The scripts the manifest injects, in its order.
  for (const file of ["rules.js", "links.js", "content.js"]) {
    await page.addScriptTag({ content: fs.readFileSync(path.join(EXTENSION, file), "utf8") });
  }
  // The player starts: the button comes, as on a real page.
  await page.evaluate(() => document.getElementById("v").dispatchEvent(new Event("play")));
  const where = await page.waitForFunction(() => {
    const shown = [...document.body.children]
      .filter((e) => !globalThis.shotBefore.has(e))
      .map((e) => e.getBoundingClientRect())
      .filter((r) => r.width > 0 && r.height > 0);
    const r = shown.at(-1);
    return r ? { x: r.left + r.width / 2, y: r.top + r.height / 2 } : null;
  });
  const { x, y } = await where.jsonValue();
  // Open its menu, as a click on "Download this video" does: the button
  // fills most of the bar, so its middle is the button's.
  await page.mouse.click(x, y);
  await page.waitForTimeout(400);
  await page.mouse.move(5, VIDEO_HEIGHT - 10);
  await page.waitForTimeout(200);
  if (problems.length) throw new Error(`video (${look}): ${problems.join("; ")}`);
  await save(encoder, `video-${look}`, await page.screenshot({ type: "png" }), opts);
  await context.close();
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const { chromium } = loadPlaywright();
  fs.mkdirSync(opts.out, { recursive: true });
  const windows = opts.only.filter((s) => s in WINDOWS);
  // The browser first, so a browser that does not start leaves no Vite
  // behind.
  const browser = await chromium.launch();
  let vite = null;
  try {
    if (windows.length) vite = await startVite();
    const encoder = await browser.newPage();
    for (const kind of windows) {
      for (const look of LOOKS) await shootWindow(browser, encoder, vite.base, kind, look, opts);
    }
    if (opts.only.includes("video")) {
      for (const look of LOOKS) await shootVideo(browser, encoder, look, opts);
    }
  } finally {
    await browser.close();
    if (vite) vite.stop();
  }
}

main().catch((e) => {
  process.stderr.write(`${e.message || e}\n`);
  process.exit(1);
});
