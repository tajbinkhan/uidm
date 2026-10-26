# UIDM

A download manager for Windows, rebuilt from scratch in Rust. It catches
downloads in Chrome and Edge, fetches each file over parallel connections,
resumes after interruptions, keeps downloading after the browser closes, and
shows everything in a desktop window.

## Install

You need the build toolchain described under [Build](#build). Then, from this
folder:

```powershell
.\install-host.ps1
```

That builds the three programs if needed, registers UIDM with Chrome and Edge,
and adds **UIDM** to the Start menu. Then load the extension once:

1. Open `chrome://extensions` (or `edge://extensions`) and turn on
   **Developer mode**.
2. Click **Load unpacked** and choose the `extension` folder.
3. Check that its ID reads `epmjakacaejhdhldkpbjnakhfmjjbhfm`. The manifest
   carries a fixed public key, so the ID is the same on every machine.

To remove everything the script added: `.\install-host.ps1 -Uninstall`.
Downloaded files are left alone.

## Use

- **Open the window** from the Start menu (**UIDM**), or run
  `target\release\uidm-gui.exe`. It lists every download with its size,
  progress, speed and time left. **Cancel** stops one and keeps the partial
  file; **Retry** resumes it; **Open** and **Show in folder** work on finished
  files; **Remove** takes a row off the list without deleting the file. Paste
  a URL at the top to add one.
- **Download in the browser as usual.** While UIDM is connected, the extension
  takes each download away from the browser and hands it to UIDM, cookies and
  referrer included, so sign-in-protected files work. Right-click a link,
  image, video or audio and choose **Download with UIDM** to send it directly.
- **The toolbar popup** shows whether UIDM is connected, lists recent
  downloads, takes a pasted URL, and has a **Capture downloads** switch. With
  capture off, or while UIDM is not running, the browser keeps its own
  downloads.

- **A save-as dialog** opens for each new download, with its real name and
  size, and the folder its category picks. Change the name or folder, then
  **Start download**, **Download later** (it waits in the list with a
  **Start** button) or **Cancel**. If the window is closed, UIDM opens it; if
  no window shows up within 20 seconds, the download starts as proposed.
  Options can turn the dialog off.
- **Categories** sort downloads by file type into their own folders:
  Compressed, Documents, Music, Programs and Video go into folders of those
  names inside the download folder, and everything else (**General**) goes
  into the download folder itself. The window's sidebar filters the list by
  category. Options edits the categories, their file types and folders.
- **The tray icon** keeps UIDM at hand. Closing the window hides it there;
  click the icon to bring it back. Its tooltip shows how many downloads run
  and how fast, and its menu has **Open UIDM**, **Add URL from clipboard**,
  **Pause all**, **Resume all**, **Speed limit** and **Exit**. Exit closes the
  window; downloads keep going and the daemon stops on its own once idle.
  **Pause all** stops every queued and running download, keeping its partial
  file; **Resume all** starts them again, along with any interrupted ones.
- **Start with Windows** (in Options) starts UIDM hidden in the tray when you
  sign in, ready for the browser.
- **When a download ends**, UIDM shows a Windows notification: **Open** and
  **Show in folder** for a finished file, **Retry** for a failed one. Options
  can show an IDM-style dialog in the window instead (a notification still
  appears if the window is not running), or nothing, and can play a sound for
  each. Canceled downloads are never announced.
- **Virus scan** (off by default) checks each finished file with Microsoft
  Defender or a scanner command you set, with `{path}` where the file goes.
  The list shows **No threat found**, **Threat found** or **Scan failed**,
  and opening a file with a threat asks first. With scanning on, the
  notification waits for the scan and says what it found.
- **Options** (top right of the window) has four tabs. **General**: the
  download folder, connections per file (1 to 16), downloads at once (1 to
  10), a total speed limit, what to do when a file name is taken, the save-as
  dialog, whether to resume unfinished downloads when UIDM starts, Start with
  Windows, and whether closing the window keeps UIDM in the tray.
  **Categories**: as above. **Browser capture**: the rules below. **When
  done**: notifications, sounds and the virus scan. The status bar shows the
  speed limit and toggles it.

Files are saved to your Downloads folder, sorted by category, unless Options
says otherwise.
Closing the browser or the window does not stop a download. The list survives
restarts: downloads that were running when UIDM stopped show as
**Interrupted**, and **Retry** resumes them from their partial file.

## How it fits together

```
browser extension --stdio--> uidm-host --+
                                          +--loopback TCP--> uidm-daemon --> engine
desktop window (uidm-gui) ---------------+   (authenticated)   (owns downloads)
```

- **`uidm-daemon`** owns the download engine and keeps a list of every
  download it has seen. It listens on `127.0.0.1:51515`; winning that bind is
  its single-instance lock. Replies to a request go to the client that asked;
  download events go to every client, so the window, the popup and any open
  browser all stay in step. It exits by itself after 60 seconds with no
  clients and no queued or running downloads. It also shows the
  notifications, plays the sounds and runs the virus scans, since it is the
  one process running whenever a download ends. Its log is
  `%TEMP%\uidm-daemon.log`. It keeps its files in:
  - `%APPDATA%\UIDM\settings.json`: the settings. A missing file or field
    means the default.
  - `%LOCALAPPDATA%\UIDM\history.json`: the download list and the next id,
    written about a second after each change and again on exit. It never
    holds cookies or request headers.
  - `%LOCALAPPDATA%\UIDM\requests.bin`: the original requests of downloads
    that can still be retried, cookies included, encrypted with Windows DPAPI
    for the current user. It is deleted when none is left.
- **`uidm-host`** is what the browser launches. It connects to the daemon,
  starting it if nothing answers, then copies frames between the browser's
  stdin/stdout and the daemon without parsing them. It waits up to 10 seconds
  for a daemon it just started, so it gives up within about 15 seconds if no
  genuine daemon answers. When the browser closes the port, the host closes
  its side, delivers any reply still in flight for up to 2 seconds, and exits.
  The daemon is started with the host's stdio marked non-inheritable, so it
  never holds the browser's pipes open.
- **`uidm-gui`** is the desktop window: one more daemon client. It starts the
  daemon too if needed, asks for the full list on connect, and applies every
  event to the same `DownloadTable` the daemon keeps.
- **The extension** keeps one native-messaging port open, reconnecting with
  backoff (1 s doubling to 30 s) if UIDM goes away.

Every link uses the same framing: a 4-byte little-endian length, then that
many bytes of UTF-8 JSON. Between browser and host that is the browser's
native-messaging standard; UIDM reuses it everywhere so the host can relay
frames untouched.

## What the extension does and does not take

It takes a download from the browser only when all of these hold:

- UIDM is connected and **Capture downloads** is on;
- the URL is `http` or `https` (`blob:` and `data:` downloads only exist inside
  the browser);
- it is not from a private window, so private downloads never enter UIDM's
  list;
- the extension did not start it itself (that is how hand-backs avoid loops);
- it passes the capture rules from **Options > Browser capture**:
  - its file type is in the list (by default every category's types plus
    `iso`, `img`, `dmg`, `deb`, `rpm` and `bin`; an empty list takes every
    type). The type comes from the file name, else the MIME type the server
    sent, else the URL. A download that says only "a file"
    (`application/octet-stream`) is taken;
  - it is not smaller than the minimum size, when the browser already knows
    the size (often it does not yet, and then only the type and site count);
  - neither its URL nor the page it came from is on an excluded site.
    `example.com` and `*.example.com` both cover the site and every subdomain;
- **Skip next download** in the popup was not pressed; it lets exactly one
  download stay in the browser.

The extension asks for the rules when it connects and gets every change as it
is saved, so they are edited only in the window.

If UIDM takes a download but cannot even start it, for example because the
server refuses the request, the extension removes it from UIDM's list and
hands it back to the browser, so it is never silently lost.

## Host-daemon authentication

Loopback keeps other machines out, but any local process can connect to the
port. Both ends therefore prove they hold a shared secret, and the secret never
crosses the socket.

At start the daemon writes a random 256-bit token to
`%LOCALAPPDATA%\UIDM\daemon.token`, which the profile's default ACL limits to
the current user, SYSTEM and Administrators. It writes the token only after it
wins the bind, so a second daemon cannot overwrite the running one's token.
Each connection then runs:

1. client to daemon: `auth_hello { client_nonce }`
2. daemon to client: `auth_challenge { server_nonce, proof }`, where `proof` is
   `HMAC-SHA256(token, "uidm-daemon", client_nonce, server_nonce)`
3. the client checks that proof, then sends `auth { proof }` with the
   `"uidm-host"` label
4. the daemon checks it and replies `auth_ok`

Every field is length-prefixed before it is hashed, and the two labels keep one
role's proof from being replayed as the other's. A process squatting on the
port cannot answer step 2, so a client gives up before it sends a single
request. The squatter only ever sees a random nonce, never the token, cookies
or URLs. A client that sends nothing, sends the wrong frame or sends a frame
over 1 KiB is dropped, and each step times out after 5 seconds.

## The download engine

Given an `add`, the engine:

1. Probes the URL with a one-byte range request. A `206` reply means the server
   supports ranges and reports the total size; anything else falls back to a
   single stream.
2. Splits the total into up to 8 contiguous byte ranges (one connection per
   ~256 KiB, capped at 16), and fetches them on parallel threads. Each thread
   opens its own file handle and writes at its own offset, so writes never race.
3. Records progress in a `<file>.uidmpart` JSON sidecar next to the output,
   flushed about every 1 MiB and again on cancel or failure. Re-running the
   download reads the sidecar and refetches only the missing bytes of each
   segment. The sidecar is checked against the server's ETag, Last-Modified and
   size, so stale bytes are never stitched onto a changed file, and it is
   deleted on success.
4. Retries a segment that fails with exponential backoff (400 ms doubling to a
   10 s ceiling, up to 5 times), resuming from that segment's own cursor.

The output filename is, in priority order: an explicit name on the request, the
server's `Content-Disposition` (including the RFC 5987
`filename*=UTF-8''...` form, percent-decoded), the URL path, then a generic
fallback. The file's folder is its category's, from that final name, unless
the save-as dialog chose one.

A new download never writes into an existing file unless that file is a
partial copy of the same download, which its `.uidmpart` sidecar proves (same
size, and same ETag or Last-Modified when the server sends them). Otherwise it
is saved as `name (1).ext`, `name (2).ext`, and so on, or replaces the file
when Options says to overwrite. Two downloads never write the same file at
once. A retry reuses the file its earlier attempt wrote.

Downloads run up to the **Downloads at once** setting (3 by default) and the
rest queue in order. Changing the setting takes effect at once: raising it
starts queued downloads, lowering it lets running ones finish. Canceling a
queued download takes it off the queue at once.

The speed limit is one token bucket shared by every connection of every
download. Each read takes tokens for the bytes it read and sleeps when the
bucket runs dry, and reads shrink to about a tenth of a second's worth so
throttled downloads flow evenly. A new limit, or turning it off, applies to
running downloads within 100 ms.

The daemon keeps each download's original request, cookies included, while it
can still be retried, so a retry resends the same headers. It drops the request
once the download finishes or is removed. When a retried download is refused
with HTTP 401, 403, 404 or 410, the error says the link or its sign-in may
have expired.

Ids keep increasing across restarts, so a new download never reuses a saved
id. With **Resume unfinished downloads at startup** on, the daemon retries
every interrupted download as soon as it starts.

## Messages

Client to daemon (the host relays the browser's unchanged):

```json
{ "op": "hello", "version": "0.2.0", "client": "window" }
{ "op": "ping" }
{ "op": "add", "url": "https://...", "headers": { "X-Example": "1" }, "cookies": "...",
  "referrer": "https://...", "user_agent": "...", "filename": "f.zip" }
{ "op": "cancel", "id": 1 }
{ "op": "retry", "id": 1 }
{ "op": "remove", "id": 1 }
{ "op": "confirm", "id": 1, "filename": "f.zip", "folder": "D:\\Downloads", "start": true }
{ "op": "list" }
{ "op": "pause_all" }
{ "op": "resume_all" }
{ "op": "get_settings" }
{ "op": "set_settings", "settings": { "download_dir": null, "connections": 8, "max_concurrent": 3,
  "speed_limit_enabled": false, "speed_limit_kbps": 1024, "resume_at_startup": false,
  "file_exists": "rename", "ask_where_to_save": true,
  "categories": [ { "name": "Compressed", "extensions": ["zip", "rar"], "folder": null } ],
  "capture_types": ["zip", "pdf"], "capture_min_size_kb": 0,
  "capture_excluded_sites": ["*.example.com"], "close_to_tray": true,
  "start_with_windows": false, "notify": "toast", "sound_on_done": false,
  "sound_on_failed": false, "scan_after_download": false, "scanner": "defender",
  "scanner_command": "" } }
```

Daemon to clients:

```json
{ "op": "hello", "name": "uidm-daemon", "version": "0.1.0" }
{ "op": "pong" }
{ "op": "ack", "id": 1, "url": "https://...", "status": "accepted" }
{ "op": "added", "id": 1, "url": "https://..." }
{ "op": "started", "id": 1, "url": "...", "path": "C:\\Users\\...\\f.zip", "total": 12345, "resumable": true, "connections": 8 }
{ "op": "progress", "id": 1, "url": "...", "done": 1048576, "total": 12345 }
{ "op": "done", "id": 1, "url": "...", "path": "C:\\Users\\...\\f.zip" }
{ "op": "failed", "id": 1, "url": "...", "error": "..." }
{ "op": "canceled", "id": 1, "url": "..." }
{ "op": "removed", "id": 1 }
{ "op": "updated", "item": { "id": 1, "url": "...", "path": "C:\\Users\\...\\Compressed\\f.zip",
  "total": 12345, "done": 0, "status": "confirming", "error": null, "resumable": true, "connections": 0 } }
{ "op": "downloads", "items": [ { "id": 1, "url": "...", "path": "...", "total": 12345,
  "done": 12345, "status": "done", "error": null, "resumable": true, "connections": 8 } ] }
{ "op": "settings", "settings": { ... } }
{ "op": "show" }
{ "op": "error", "message": "..." }
```

`ack`, `downloads` and `error` go only to the client that asked, as does
`settings` when it answers `get_settings`; everything else goes to every
client, including `settings` after a change. A status is one of `queued`,
`downloading`, `done`, `failed`, `canceled`, `interrupted`, `confirming`
(waiting for the save-as dialog; `path` is the proposed file) or `waiting`
(added to start later). `updated` replaces a whole entry, for the changes no
other event covers. A `hello` with `client: "window"` tells the daemon a
window is open to show save-as dialogs. `confirm` answers the dialog of a
`confirming` download or starts a `waiting` one, keeping its id; `filename`
and `folder` default to the proposed ones, and `start: false` makes it
`waiting`. Canceling a `confirming` download reports `canceled`. `retry` removes
the old entry (`removed`) and queues a new one with a new id; it is refused
unless the download failed, was canceled or was interrupted. `remove` is
refused while a download is still running. `set_settings` with an invalid
value (`connections` outside 1-16, `max_concurrent` outside 1-10, a speed
limit outside 1-10,000,000 KB/s, or a folder that is not a full path) is
refused with an `error` that says why. `download_dir: null` means the
Downloads folder, and `file_exists` is `rename` or `overwrite`. A category
needs a unique name other than `General`, at least one file type, and a full
path or `null` for its folder (`null` means a folder named after it inside the
download folder). File types are lowercase letters and digits without the
dot. `notify` is `toast`, `dialog` or `off`; `scanner` is `defender` or
`custom`, and a custom `scanner_command` must name a program and contain
`{path}`. Turning `start_with_windows` on or off adds or removes the `UIDM`
value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, which
runs `uidm-gui.exe --minimized`.

A finished download's entry carries `"scan": { "result": "scanning" }`, then
`clean`, `threat` or `failed` (the last two with a `detail`, the end of the
scanner's output) when scanning is on. `pause_all` cancels every queued and
running download; `resume_all` retries those and every interrupted one. `show`
asks the window to come forward, after a click on a notification.

## Notifications and virus scans

Windows only shows notifications from an app it knows, so at start the daemon
registers the app id `UIDM.DownloadManager` under
`HKCU\Software\Classes\AppUserModelId` with UIDM's name and an icon it writes
to `%LOCALAPPDATA%\UIDM\uidm.png`. Each button carries an argument such as
`open:12`, which the daemon acts on while it runs. A failure is announced one
second late, and only if the download is still failed then, so a download the
extension hands back to the browser is not announced. A file with a threat
is never opened from a notification; its Open button brings up the window,
which asks first.

The scanner runs as a program with separate arguments, never through a shell.
A custom command line is split into arguments first (double quotes group;
backslashes are literal) and `{path}` is replaced afterwards inside each
argument, so a file name cannot add arguments or run anything, whatever it
contains. Defender runs as `MpCmdRun.exe -Scan -ScanType 3 -File <path>
-DisableRemediation` and reports a threat with exit code 2. A custom scanner
reports a threat with exit code 1 (ClamAV's convention). Any other exit code
is a failed scan, as is one that runs over 10 minutes.

## Layout

| Path | What it is |
| --- | --- |
| `src/bin/uidm-gui.rs` | Desktop window |
| `src/bin/uidm-host.rs` | Native-messaging relay and CLI test mode |
| `src/bin/uidm-daemon.rs` | Daemon: server, auth, download list, retry/remove, idle exit |
| `src/state.rs` | `DownloadTable`, kept by both the daemon and the window |
| `src/settings.rs` | Settings, their defaults, validation and file |
| `src/history.rs` | Download history and saved requests on disk |
| `src/dpapi.rs` | DPAPI encryption for saved requests |
| `src/registry.rs` | Start with Windows and the notification app id |
| `src/notify.rs` | Windows notifications and sounds |
| `src/scan.rs` | Virus scans of finished downloads |
| `src/bin/gui/` | The window's tray icon and Win32 helpers |
| `src/ipc.rs` | Daemon address, start-up, token file, HMAC handshake |
| `src/app.rs` | Request building and event mapping |
| `src/protocol.rs` | Framing and message types |
| `src/engine/` | Queue, segments, resume sidecar, speed limiter, HTTP |
| `extension/` | MV3 extension: capture, context menu, popup |
| `extension/rules.js` | Capture rules (file types, minimum size, excluded sites) |
| `extension/test/` | Node tests for the rules and the service worker |
| `assets/icon-64.rgba` | Window icon, raw RGBA made from `extension/icons/128.png` |
| `host/com.uidm.host.json` | Template of the native-messaging manifest |
| `install-host.ps1` | Builds, registers with Chrome and Edge, adds the Start menu entry |

## Build

Rust on Windows needs a linker. This project builds with the
`stable-x86_64-pc-windows-gnu` toolchain plus MinGW-w64 (WinLibs) on `PATH`.

```bash
cargo test
cargo build --release
node --test extension/test/*.test.js
```

The Rust tests also run on Linux under Wine, cross-compiled with
`cargo test --target x86_64-pc-windows-gnu` and
`CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine` (needs the `mingw-w64` and
`wine64` packages).

`cargo build --release` produces `uidm-host.exe`, `uidm-daemon.exe` and
`uidm-gui.exe` in `target\release`. The host and the window start the daemon
from their own folder, so keep the three together. The window renders through
OpenGL, which needs a working graphics driver.

To run one download without the browser, window or daemon:

```bash
cargo run --release --bin uidm-host -- download https://example.com/file.zip
```

## Verified

- 75 unit tests: segment splitting, the resume sidecar, framing,
  `Content-Disposition`, backoff, HMAC proofs, the download table's
  lifecycle and ordering cases, settings loading, defaults and validation,
  history loading (running downloads become interrupted, ids never go
  backwards), DPAPI round trips, and the speed limiter. Engine tests against a
  local HTTP server cover an existing file (saved as `name (2)`), an existing
  file with a stale sidecar, a matching sidecar (resumes and fetches only the
  missing half), the overwrite setting, a 2 MiB download held to 1 MiB/s
  within 10%, raising the concurrency live, canceling a queued download,
  category folders, and probing a download for the save-as dialog. Category
  tests cover uppercase extensions, names without one and dot files.
- 10 extension tests: each capture rule, and the real `background.js` run
  with a stand-in `chrome` API: it asks for the rules on connect, takes or
  leaves downloads by type, size and site, applies changed rules at once,
  skips exactly one download after **Skip next download**, and starts a
  download waiting for its dialog from the popup.
- Save-as dialog, under Wine against the real host and daemon: the proposal
  lands in `Downloads\Documents\report.pdf` with the real size; with no window
  possible the download starts as proposed after 19.6 s; a new name and folder
  are used; **Download later** waits and later starts under the same id;
  canceling in the dialog reports `canceled` and a later `confirm` is
  refused. With a display, the daemon opened the window itself and it showed
  the dialog; **Download later**, the category sidebar, and adding a category
  in Options (`JPG, .png webp` saved as `jpg`, `png`, `webp`) were checked.
- Tray, notifications and scans. Unit tests cover the registry (read, write,
  delete, a quoted Run command), the notification XML (escaping, buttons),
  scanner command lines (quoting, a file name with quotes and `&` staying one
  argument, a command without `{path}` refused) and real scanner runs with
  exit codes 0, 1 and 7. Under Wine against the real daemon: Start with
  Windows writes `"...\uidm-gui.exe" --minimized` to the Run key and removes
  it again; the app id is registered; a scanner reporting a threat and one
  reporting clean show as such; **Pause all** stops a running and a queued
  download and **Resume all** finishes both, the running one in its own
  partial file. Under a virtual display, `--minimized` starts with no visible
  window; in dialog mode a finished download brought the hidden window
  forward with its completion dialog and scan result; **Open** on a file with
  a threat asked first; closing the window hid it while the process kept
  running, and the next finished download brought it back.
- Settings and history, run under Wine against the real host and daemon:
  `set_settings` with 99 connections is refused with a message; a saved folder
  and a 2 MiB/s limit take effect (4 MiB in 1.96 s) and survive a restart; a
  download killed mid-way shows as interrupted after restart, its retry gets a
  new id and resumes into the same file byte-identical; with resume at startup
  on, a killed download finishes on its own once the daemon starts again;
  `requests.bin` holds no plaintext cookie; clearing the list empties
  `history.json` and deletes `requests.bin`.
- Engine: segmented downloads are byte-identical to a reference download; a
  download killed at ~10 MiB of 33 MiB resumes to a byte-identical file.
- Daemon split: a host closed at 1 MiB of a 33 MiB download exits at once and
  the daemon finishes the file with no client attached, byte-identical; a
  second client reuses the same daemon.
- Authentication: a fake listener on the port that answers `auth_ok` to
  everything sees only `auth_hello`, never the token, a proof or a cookie, and
  the client refuses it after about 10 seconds.
- Requests: `list` returns the running download; `remove` on a running download
  is refused; cancel at 4.2 MB then `retry` resumes past that point and
  finishes byte-identical; `retry` on a finished download is refused.
- Extension: the real `background.js` was run against the real host and daemon
  with a stand-in for the `chrome` API (18 checks). It takes a download from
  the browser, forwards cookies, referrer and user agent, and the file lands
  under its `Content-Disposition` name; private, `blob:` and self-started
  downloads stay in the browser, as does everything while capture is off; a
  download UIDM cannot start goes back to the browser; the context menu works
  connected and falls back to the browser when not; the worker notices when the
  host dies, reconnects and restores the list.
- Window: driven through Windows UI Automation. It starts the daemon, shows a
  running download, and its Cancel, Retry and Remove buttons reach the daemon;
  the retried download finishes byte-identical. The Options dialog, its error
  for a relative folder, saving, and the status bar's speed limit toggle were
  checked under Wine.

Not verified here: Windows notifications (Wine has no WinRT notifications;
the daemon reached the call at the right moments and logged its failure) and
the tray icon's menu (the virtual display had no tray to click). In the first
of four runs of the minimized-window test the window did not come forward;
the three runs after it did, and the cause was not found.

Also not verified: Chrome itself loading the extension and launching the host.
Current Chrome builds ignore the `--load-extension` flag, so that last step was
not automated. The pieces on either side of it are tested above.

## Known gaps

- The history is written about a second after each change, so a crash can
  lose the last second of progress; the sidecar still has it, so resuming is
  unaffected.
- When a file name is taken, UIDM renames or overwrites; it cannot ask. The
  save-as dialog shows the proposed name, not the `name (1)` the engine may
  pick if that name is taken when the download starts.
- Adding a category does not add its types to the captured types; that list
  is edited on its own.
- Downloads the browser starts with a POST request (some form submissions) are
  re-requested by UIDM as a GET and may fail. Turn capture off for such sites.
- Every client sees every download, not only the ones it started.
- A process that squats on the port cannot learn anything, but it does stop
  downloads from starting until it goes away. A per-user named pipe would close
  that gap.
- Other processes running as the same user can read the token. That is outside
  this design's threat model: such a process can already read the browser's
  cookies directly.
