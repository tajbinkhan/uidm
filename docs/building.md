# Building UDM

Where the code lives, how to build and package it, and how to add a language.

## Layout

The Rust code is a Cargo workspace with one crate per layer. `AGENTS.md`
gives the rules between them; its section 16 says where the code still
differs.

| Path | What it is |
| --- | --- |
| `crates/domain/` | Pure rules, no I/O: the download model (`download.rs`), the list kept by both the daemon and the window (`table.rs`), queues and schedules (`queue.rs`), URL patterns and pasted lists (`batch.rs`), site patterns (`site.rs`), file names (`filename.rs`), settings and their defaults (`settings.rs`), streams (`media.rs`), the grabber's model (`grab.rs`), and the port traits (`ports.rs`) |
| `crates/application/` | Use cases, one per file: `downloads/`, `queues/`, `quota/`, `power/`, `update/`, `grab/`; the daemon's `Command`s and `Event`s and its service (`daemon/`); the window's ports (`window.rs`); in-memory adapters for tests (`memory.rs`) |
| `crates/infrastructure/src/engine/` | Download engine: segments, resume sidecar, speed limiter, HTTP, FTP, streams |
| `crates/infrastructure/src/media/` | HLS playlists, DASH and HDS manifests, yt-dlp pages, ffmpeg conversion |
| `crates/infrastructure/src/protocol.rs`, `wire.rs` | Framing and wire messages, and their mapping to `Command` and `Event` |
| `crates/infrastructure/src/ipc.rs`, `server.rs`, `daemon_client.rs` | Daemon address, start-up, token file and HMAC handshake; the daemon's side and the window's side of the connection |
| `crates/infrastructure/src/store.rs`, `history.rs`, `state.rs` | The daemon's download list, queues and kept requests, and their files |
| `crates/infrastructure/src/settings.rs`, `live_settings.rs` | The settings file, its migrations and the checks that read the disk |
| `crates/infrastructure/src/transfers.rs`, `probes.rs`, `connect.rs` | The engine's adapter in the daemon, server probes, and the proxy and logins a download uses |
| `crates/infrastructure/src/net.rs`, `pac.rs`, `ntlm.rs` | Proxies, PAC scripts, site logins and HTTP authentication |
| `crates/infrastructure/src/grab.rs`, `crawler.rs` | The site grabber's crawl and `robots.txt`, and its project files |
| `crates/infrastructure/src/paths.rs`, `logging.rs` | Every folder UDM uses, and the log files |
| `crates/infrastructure/src/dpapi.rs`, `registry.rs`, `autostart.rs`, `browsers.rs` | Sealing saved requests and passwords, registry values, start at sign-in, and registering the host with each browser |
| `crates/infrastructure/src/notify.rs`, `scan.rs`, `power.rs`, `update.rs` | Notifications and sounds, virus scans, sleep and shut down, the update check |
| `crates/infrastructure/src/listfile.rs`, `from_old_names.rs` | Exporting and importing the download list; moving the files of UDM's earlier names, Pullbay and UIDM, over |
| `crates/ui/` | The window's Tauri shell (ADR 0002): its state and the actor that owns it (`state.rs`, `actor.rs`), what each window shows (`projection/`), the commands the webview calls (`commands/`), the types that cross to it (`contract/`), the windows, tray and theme, `tauri.conf.json`, one capability file per window kind (`capabilities/`), and the translations (`src/i18n.rs`, `src/lang/`) |
| `frontend/` | The window's web frontend: Svelte 5 and TypeScript, one page per window kind (`src/windows/`), the generated `src/bindings.ts`, and tests in `test/` |
| `crates/app/src/bin/udm-daemon.rs` | Daemon: wires the adapters into the service, starts the background threads and serves clients |
| `crates/app/src/bin/udm-gui.rs` | The window: builds its adapters and starts `udm-ui` |
| `crates/app/src/bin/udm-host.rs` | Native-messaging relay, `send` for other programs, and CLI test mode |
| `crates/app/src/bin/udm-cli.rs`, `crates/application/src/remote/`, `crates/infrastructure/src/tool.rs` | The command-line tool: its wiring, its commands (use cases over the daemon connection), and its arguments, output and exit codes |
| `crates/app/src/bin/udm-mcp.rs`, `crates/application/src/remote/assistant.rs`, `crates/infrastructure/src/mcp.rs` | The AI assistants' MCP server: its wiring, the requests it runs on one daemon connection, and the protocol and its tools |
| `extension/chromium/` | MV3 extension for Chrome, Edge and other Chromium browsers: capture, context menu, popup. Edit the extension here |
| `extension/chromium/_locales/` | The extension's text in each language |
| `extension/chromium/rules.js` | Capture rules (file types, minimum size, excluded sites) |
| `extension/chromium/links.js` | The links of a page for "Download all links" and "selected links" |
| `extension/chromium/post.js` | Matching downloads to the form POSTs that started them |
| `extension/chromium/request.js` | The request behind a download: headers, redirects, response, proxy setting |
| `extension/chromium/media.js` | Which responses are videos and streams, per tab, and the video menu |
| `extension/chromium/content.js` | The video button over playing videos, and in Safari the download links taken as they are clicked |
| `extension/chromium/native.js` | Safari's polling stand-in for a native port ([Safari](install.md#safari)) |
| `extension/firefox/` | The Firefox copy of `extension/chromium`, made by `node installer/make-firefox.js` and kept in git; do not edit it by hand |
| `extension/firefox.json` | What the Firefox copy changes in `manifest.json` |
| `extension/safari/` | The Safari copy of `extension/chromium`, made by `node installer/make-safari.js` and kept in git; do not edit it by hand |
| `extension/safari.json` | What the Safari copy changes in `manifest.json` |
| `extension/test/` | Node tests for the rules, the service worker and the Firefox and Safari copies |
| `assets/icon-1024.png` | The app icon's source, 1024 pixels square with a transparent background; `installer/render-icons.js` renders every other icon file from it, and the macOS disk image uses it as is |
| `host/com.udm.host.json` | Template of the native-messaging manifest |
| `install-host.ps1` | Builds, registers with Chrome, Edge and Firefox, adds the Start menu entry |
| `installer/windows/udm.iss` | Inno Setup script for `UDM-Setup-<version>.exe` |
| `installer/windows/build.ps1` | Builds the programs and the installer into `dist\` |
| `installer/linux/` | `package.sh` (the `.deb`, `.rpm`, AppImage and `.tar.gz`), the tarball's `install.sh`, the AppImage's `AppRun` and the desktop entry |
| `installer/macos/package.sh` | Builds the universal `.dmg` |
| `installer/macos/safari/` | `build.sh`, which builds the Safari extension into `UDM.app` with Xcode, its native-messaging handler (`SafariWebExtensionHandler.swift`) and the sandbox entitlements |
| `installer/make-firefox.js` | Makes `extension/firefox`, or with `--check` tells whether it is out of date |
| `installer/make-safari.js` | The same for `extension/safari` (both use `installer/browser-copy.js`) |
| `installer/make-store-zips.js` | Makes the zips the Chrome Web Store and Firefox Add-ons take ([Publishing the extension](extension-stores.md)) |
| `installer/windows/make-firefox.ps1` | The same in PowerShell, which Setup's build uses to check the copy |
| `installer/render-icons.js` | Renders `assets/icon-1024.png` to the extension's PNGs and `assets/icon-256.png` with headless Chrome |
| `installer/windows/udm.ico` | Setup and shortcut icon, packed from `extension/chromium/icons` by `make-icon.js` |
| `tools/screenshots/` | `shoot.js`, which takes the screenshots in `docs/images/` again from the preview page and the extension's video button ([how](../tools/screenshots/README.md)) |
| `docs/adr/` | Architecture decisions |
| `GLOSSARY.md` | The names of the windows and dialogs |

## Build

On macOS and Linux, a stable Rust toolchain is enough, plus on Linux the GTK 3
and OpenSSL development packages (`sudo apt install libgtk-3-dev libssl-dev`
on Ubuntu):

```bash
cargo test --workspace
cargo build --release
```

The window (`udm-gui`, ADR 0002) also needs Node 22 or newer and
pnpm, and on Linux WebKitGTK 4.1 and its friends (`sudo apt install
libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev
build-essential`). The shell embeds the built frontend, so build it before
Cargo:

```bash
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend build
```

To work on the frontend with hot reload, run `pnpm --dir frontend dev`, then
`cargo run -p udm-app --bin udm-gui --no-default-features --features
tauri-window`, which loads Vite's server at `http://localhost:1420` instead of
the embedded frontend. In a plain browser,
`http://localhost:1420/dev/index.html?window=main&look=dark` draws any
window (`main`, `add-url`, `save-as`, `download`, `done`, `remove`,
`about`, `options`, `scheduler`, `grabber`, `grabber-results`, `batch`, `links`, `properties`,
`drop-target`, `login`, `countdown`, `refresh`, `password`, `pick-links`, `pick-videos`, `gallery`, `zip-preview`, `exception`) from the test fixtures, with the shell mocked.

When a contract type or a command changes, write the TypeScript bindings and
the fixtures again (the tests fail until they match):

```bash
UDM_WRITE_BINDINGS=1 UDM_WRITE_FIXTURES=1 cargo test -p udm-ui
```

The screenshots in `docs/images/` are drawn the same way, from the fixtures'
sample list. After a window changes, take them again, light and dark, as
WebP (it needs Playwright and its Chromium where Node finds them; see
[tools/screenshots](../tools/screenshots/README.md)):

```bash
node tools/screenshots/shoot.js
```

The server image (`Dockerfile`) builds `udm-app` with
`--no-default-features`, which leaves the window out.

`installer/linux/package.sh` makes the `.deb`, `.rpm`, AppImage and
`.tar.gz` in `dist/` (it needs `dpkg-deb`, `rpmbuild` and Node, and
downloads `appimagetool` unless `$APPIMAGETOOL` names one);
`installer/macos/package.sh` makes the universal `.dmg` on a Mac. Name
formats to make only those: `installer/linux/package.sh --skip-build deb`.
With Xcode installed (not only its Command Line Tools), `package.sh` also
builds the Safari extension into `UDM.app` (`installer/macos/safari/build.sh`,
through Xcode's `safari-web-extension-converter`); without Xcode it warns and
leaves it out, as it does when that build fails, unless `UDM_SAFARI=require`.
That build has not run yet: until it has passed once on CI's macOS runner,
the build workflow's **Build the Safari extension** step may fail
(`continue-on-error`) and the release builds the disk image without
`UDM_SAFARI=require`. After the first green run, drop `continue-on-error`
from that step and set `UDM_SAFARI: require` on the release's **Build the
disk image** step, so a broken Safari build stops a release again.

Rust on Windows needs a linker. This project builds with the
`stable-x86_64-pc-windows-gnu` toolchain plus MinGW-w64 (WinLibs) on `PATH`.
Built that way, `udm-gui.exe` needs `WebView2Loader.dll` beside it, which the
build puts in `target\release` and Setup installs; an MSVC build links the
loader in.

```bash
cargo test --workspace
cargo build --release
node --test extension/test/*.test.js
```

To try a build next to the installed UDM, give it a name in
`UDM_INSTANCE`. That copy keeps its settings and list in `UDM-<name>`
folders, runs its daemon on a port of its own, and shows the name in the
window's title. It does not end other daemons when it restarts its own.

```powershell
$env:UDM_INSTANCE = "test"; .\target\debug\udm-gui.exe
```

A string the window shows needs its translation in every table in
`crates/ui/src/lang/`; a test fails for one without them.
`crates/ui/src/lang/untranslated.json` lists the strings some table still lacks,
and an entry leaves the list once every table has it.

GitHub Actions builds and tests UDM on Windows, macOS and Linux on every
push and pull request (`.github/workflows/build.yml`), and the extension's
tests with Node. Those runs keep no files.

Each run also fails on a clippy warning or on code that rustfmt would
change. Run the same checks before pushing:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`rustfmt.toml` sets the line width to 120. Clippy checks only the code for
the platform it runs on, so a warning in a macOS-only or Linux-only block
shows up on that runner. With the target added through `rustup`,
`cargo clippy --target x86_64-pc-windows-gnu` or
`--target x86_64-apple-darwin` lints that code from Linux too.

### Making a release

The installers come from `.github/workflows/release.yml`. Before a release,
make the server-country table again from the newest data (below). Set the version in
`Cargo.toml` and `extension/chromium/manifest.json`, remake the Firefox and
Safari copies (`node installer/make-firefox.js`, `node installer/make-safari.js`), commit, and push a tag that matches it:

```bash
git tag v0.4.0
git push origin v0.4.0
```

It builds every file in the table under [Install](install.md) on Windows,
macOS and Linux runners, and the extension's store zips
([Publishing the extension](extension-stores.md)), and attaches them to a new
GitHub Release for that tag. Release files don't count against the repository's storage (each may be
up to 2 GB), and UDM's update check reads the latest release. The build
files are kept as artifacts for one day only, to hand them from the
builders to the release. Starting the workflow by hand
(**Actions > Release > Run workflow**) builds the same files without a
release, as artifacts for a day.

**The server-country table.** `crates/infrastructure/data/ip-country.bin`
must be made again from the CSV files of the latest
[ip-location-db GitHub release](https://github.com/sapics/ip-location-db/releases)
before each release: the copy in the repository was made from the project's
npm package, whose data has known wrong rows and which no longer gets
updates. With the [GitHub CLI](https://cli.github.com/), from the
repository's root:

```bash
gh release download --repo sapics/ip-location-db --dir ipdb --pattern 'geo-whois-asn-country-ipv4.csv' --pattern 'geo-whois-asn-country-ipv6.csv' && python3 tools/ip-country/make_table.py ipdb crates/infrastructure/data/ip-country.bin && rm -r ipdb
```

If `gh` finds no file of those names, look at the release's file list for the
`geo-whois-asn-country` IPv4 and IPv6 CSVs (`start,end,country` rows) and
download those two under these names. Then write the release's tag and date in
`crates/infrastructure/data/README.md`, run
`cargo test -p udm-infrastructure country` (it looks up a few well-known
servers in the new table), and commit the table with the README. The data is
CC BY 4.0: `THIRD-PARTY-NOTICES.txt`, which every package ships, carries its
notice.

To build the installer, install [Inno Setup 6](https://jrsoftware.org/isinfo.php)
(`winget install JRSoftware.InnoSetup`), then run:

```powershell
.\installer\windows\build.ps1
```

It builds the release programs and writes `dist\UDM-Setup-<version>.exe`,
with the version from `Cargo.toml`. Pass `-SkipBuild` when `target\release`
is already current.

The Rust tests also run on Linux under Wine, cross-compiled with
`cargo test --target x86_64-pc-windows-gnu` and
`CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine` (needs the `mingw-w64` and
`wine64` packages).

`cargo build --release` produces `udm-host.exe`, `udm-daemon.exe`,
`udm-gui.exe`, `udm-cli.exe` and `udm-mcp.exe` in `target\release`. The
host, the window, the command-line tool and the MCP server start the daemon from
their own folder, so keep them together. The window renders through
OpenGL, which needs a working graphics driver.

To run one download without the browser, window or daemon:

```bash
cargo run --release --bin udm-host -- download https://example.com/file.zip
```

### Adding a language

The window's strings are written in English in the code, and that English is
each string's ID: `t('Save')` in the frontend and `lang.tr("Save")` in the
shell, with `{placeholders}` filled in afterwards. A language is one JSON file in `crates/ui/src/lang/`, mapping each
English string to its translation, plus a line in `LANGUAGES` in
`crates/ui/src/i18n.rs`; a missing string shows in English. The tests check that
every key is still used and keeps its `{placeholders}`. The extension's text is
in `extension/chromium/_locales/<code>/messages.json`, as Chrome expects; its tests
check that every locale has the same messages as `en`. The webview shapes the
window's text and lays it out right to left; a right-to-left language also sets
`Language::right_to_left`, which sets the windows' direction.
[Help translate](translating.md) has the steps and which tables wait for review.

---

[← Back to the help pages](README.md)
