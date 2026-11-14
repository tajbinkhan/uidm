# Screenshots

`shoot.js` takes the screenshots in [`docs/images/`](../../docs/images/) again, each in light and
dark, as WebP. The README, the guides in [`docs/guides/`](../../docs/guides/) and the project page
use them. Run it after a window changes:

```bash
node tools/screenshots/shoot.js              # every shot
node tools/screenshots/shoot.js main video   # only these
node tools/screenshots/shoot.js --scale 1.5  # smaller files
node tools/screenshots/shoot.js --out /tmp/shots  # somewhere else, to compare
```

| File | What it shows | Size (CSS pixels) |
| --- | --- | --- |
| `main-{light,dark}.webp` | The main window | 1100 × 640 |
| `save-as-{light,dark}.webp` | Download File Info | 576 × 206 |
| `download-{light,dark}.webp` | The download window, with **Show details >>** clicked | 552 × 413 |
| `torrent-{light,dark}.webp` | A torrent's download window, on its **Files** tab | 700 × 400 |
| `options-{light,dark}.webp` | Options, on its General page | 436 × 505 |
| `scheduler-{light,dark}.webp` | The Scheduler, with **Start download at** and **Turn off computer when done** ticked | 657 × 464 |
| `video-{light,dark}.webp` | The extension's **Download this video** button and its menu on a sample page | 960 × 660 |
| `linux-{light,dark}.webp` | The real window on Linux, taken by hand (below) | 1102 × 646 |

The dialogs are at the size the shell opens them (`Label::size` in `crates/ui/src/windows.rs`).
Pictures are taken at twice that (`--scale 2`), which keeps each under 200 KB.

## What it needs

- Node 20 or later, and `pnpm --dir frontend install` done once.
- [Playwright](https://playwright.dev/) with its Chromium, where Node's `require` finds it. Nothing
  is added to the project's `package.json` files; install it for your user, for example
  `npm install -g playwright`, `npx playwright install chromium`, and `NODE_PATH` set to
  `npm root -g`.

No image library is needed: Chromium encodes the WebP files itself.

## How the pictures are made

- **The windows** are the real frontend drawn by the preview page (`frontend/dev/`), which Vite's
  dev server serves while the script runs; the production build is not touched. Each window gets
  the snapshot the shell's own projections made from a sample list,
  `frontend/test/fixtures/views.json`, which the frontend's tests draw too. To change the sample (the files, their states, the column
  widths), edit `crates/ui/src/fixtures.rs` and run
  `UDM_WRITE_FIXTURES=1 cargo test -p udm-ui fixtures`, then check that `pnpm --dir frontend test`
  still passes. Where a shot needs a click (the download window's details, a torrent's Files tab, the Scheduler's ticks),
  the script clicks as a user would.
- **The video button** is the extension's own content scripts (`rules.js`, `links.js`,
  `content.js`, as the manifest loads them) on a sample page the script serves to the browser
  itself, so nothing reaches the network. The service worker needs UDM and a stream server, so the
  script answers its messages in its place: the menu is the worker's own `mediaMenu` (`media.js`)
  run on a sample HLS stream with the qualities UDM would report for it, and the text is
  `_locales/en/messages.json`.
- **Fonts** are the machine's. On Windows the window gets its Segoe UI; elsewhere it gets what
  `system-ui` gives (Inter, on the machine the committed pictures were taken on).

## The Linux shot

`linux-light.webp` and `linux-dark.webp` are the real `udm-gui` (a debug build) on Linux, not the
preview page, and are not made by `shoot.js`, since they need an X server and a running daemon.
They were made like this, on Ubuntu 24.04 with WebKitGTK 4.1:

1. `apt-get install xvfb openbox xdotool imagemagick`, then `Xvfb :77 -screen 0 1280x800x24`
   and `openbox`, with `DISPLAY=:77`.
2. A scratch home: `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_RUNTIME_DIR` pointed into
   one folder and `UDM_INSTANCE=shots`, so nothing of the machine's own UDM was used.
3. Sample files from `tools/bench/faultserver.py` on a few ports (`--size 4G --rate 300K` for two
   slow downloads that stay running, and smaller ones without a limit), added with
   `udm-cli get` under file names given in the address, which the server ignores.
4. `udm-daemon`, then `udm-gui`, with `tips_on_startup` off and `theme` set to `light` or `dark`
   in `settings.json` (with the daemon stopped). `xdotool` sized the window to 1100 × 620 and
   dragged the column borders wider, as a user would, since Linux keeps no layout file.
5. `import -window root -crop 1102x646+30+30` caught the window with openbox's frame, and
   `convert -quality 90` made the WebP.
