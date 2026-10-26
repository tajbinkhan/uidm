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

Files are saved to your Downloads folder. Closing the browser or the window
does not stop a download.

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
  clients and no queued or running downloads. Its log is
  `%TEMP%\uidm-daemon.log`.
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
- the extension did not start it itself (that is how hand-backs avoid loops).

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
fallback. Up to 3 downloads run at once (`MAX_CONCURRENT_DOWNLOADS`) and the
rest queue.

The daemon keeps each download's original request, cookies included, while it
can still be retried, so a retry resends the same headers. It drops the request
once the download finishes or is removed.

## Messages

Client to daemon (the host relays the browser's unchanged):

```json
{ "op": "hello", "version": "0.2.0" }
{ "op": "ping" }
{ "op": "add", "url": "https://...", "headers": { "X-Example": "1" }, "cookies": "...",
  "referrer": "https://...", "user_agent": "...", "filename": "f.zip" }
{ "op": "cancel", "id": 1 }
{ "op": "retry", "id": 1 }
{ "op": "remove", "id": 1 }
{ "op": "list" }
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
{ "op": "downloads", "items": [ { "id": 1, "url": "...", "path": "...", "total": 12345,
  "done": 12345, "status": "done", "error": null, "resumable": true, "connections": 8 } ] }
{ "op": "error", "message": "..." }
```

`ack`, `downloads` and `error` go only to the client that asked; everything
else goes to every client. `retry` removes the old entry (`removed`) and queues
a new one with a new id; it is refused unless the download failed or was
canceled. `remove` is refused while a download is still running.

## Layout

| Path | What it is |
| --- | --- |
| `src/bin/uidm-gui.rs` | Desktop window |
| `src/bin/uidm-host.rs` | Native-messaging relay and CLI test mode |
| `src/bin/uidm-daemon.rs` | Daemon: server, auth, download list, retry/remove, idle exit |
| `src/state.rs` | `DownloadTable`, kept by both the daemon and the window |
| `src/ipc.rs` | Daemon address, start-up, token file, HMAC handshake |
| `src/app.rs` | Request building and event mapping |
| `src/protocol.rs` | Framing and message types |
| `src/engine/` | Queue, segments, resume sidecar, HTTP |
| `extension/` | MV3 extension: capture, context menu, popup |
| `assets/icon-64.rgba` | Window icon, raw RGBA made from `extension/icons/128.png` |
| `host/com.uidm.host.json` | Template of the native-messaging manifest |
| `install-host.ps1` | Builds, registers with Chrome and Edge, adds the Start menu entry |

## Build

Rust on Windows needs a linker. This project builds with the
`stable-x86_64-pc-windows-gnu` toolchain plus MinGW-w64 (WinLibs) on `PATH`.

```bash
cargo test
cargo build --release
```

`cargo build --release` produces `uidm-host.exe`, `uidm-daemon.exe` and
`uidm-gui.exe` in `target\release`. The host and the window start the daemon
from their own folder, so keep the three together. The window renders through
OpenGL, which needs a working graphics driver.

To run one download without the browser, window or daemon:

```bash
cargo run --release --bin uidm-host -- download https://example.com/file.zip
```

## Verified

- 34 unit tests: segment splitting, the resume sidecar, framing,
  `Content-Disposition`, backoff, HMAC proofs, and the download table's
  lifecycle and ordering cases.
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
  the retried download finishes byte-identical.

Not verified here: Chrome itself loading the extension and launching the host.
Current Chrome builds ignore the `--load-extension` flag, so that last step was
not automated. The pieces on either side of it are tested above.

## Known gaps

- The daemon's list lives in memory. When the daemon exits (60 seconds after
  the last client and download), finished entries are forgotten, and an
  interrupted download resumes only when it is requested again.
- Downloads the browser starts with a POST request (some form submissions) are
  re-requested by UIDM as a GET and may fail. Turn capture off for such sites.
- Every client sees every download, not only the ones it started.
- A process that squats on the port cannot learn anything, but it does stop
  downloads from starting until it goes away. A per-user named pipe would close
  that gap.
- Other processes running as the same user can read the token. That is outside
  this design's threat model: such a process can already read the browser's
  cookies directly.
