# UIDM

A download manager, rebuilt from scratch in Rust. A browser extension captures
downloads and hands them to a native-messaging host. The host relays them to a
long-lived daemon, and the daemon downloads each file over parallel byte-range
connections, resuming after an interruption. Downloads keep running after the
browser closes.

## How it fits together

```
browser extension --stdio--> uidm-host --loopback TCP--> uidm-daemon --> engine
   (captures)                (relay, one per             (one per user,    (segments,
                              browser connection)         owns downloads)   resume)
```

- **`uidm-host`** is what the browser launches. It connects to the daemon,
  starting it if nothing answers, then copies frames between the browser's
  stdin/stdout and the daemon's socket without parsing them. It waits up to
  10 seconds for a daemon it just started, so it gives up within about 15
  seconds if no genuine daemon answers. When the browser closes the port, the
  host closes its side, delivers any reply still in flight for up to 2 seconds,
  and exits; the daemon carries on. The daemon is started with the host's
  stdio marked non-inheritable, so it never holds the browser's pipes open.
- **`uidm-daemon`** owns the download engine. It listens on `127.0.0.1:51515`;
  winning that bind is its single-instance lock. Replies to a request (`hello`,
  `pong`, `ack`, `error`) go to the client that asked. Download events
  (`started`, `progress`, `done`, `failed`, `canceled`) go to every connected
  client. It exits by itself after 60 seconds with no clients and no queued or
  running downloads. Its log is `%TEMP%\uidm-daemon.log`.

Both links use the same framing: a 4-byte little-endian length, then that many
bytes of UTF-8 JSON. Between browser and host that is the browser's
native-messaging standard. UIDM reuses it to the daemon so the host can relay
frames untouched. The message shapes are UIDM's own.

## Host-daemon authentication

Loopback keeps other machines out, but any local process can connect to the
port. Both ends therefore prove they hold a shared secret, and the secret never
crosses the socket.

At start the daemon writes a random 256-bit token to
`%LOCALAPPDATA%\UIDM\daemon.token`, which the profile's default ACL limits to
the current user, SYSTEM and Administrators. It writes the token only after it
wins the bind, so a second daemon cannot overwrite the running one's token.
Each connection then runs:

1. host to daemon: `auth_hello { client_nonce }`
2. daemon to host: `auth_challenge { server_nonce, proof }`, where `proof` is
   `HMAC-SHA256(token, "uidm-daemon", client_nonce, server_nonce)`
3. the host checks that proof, then sends `auth { proof }` with the
   `"uidm-host"` label
4. the daemon checks it and replies `auth_ok`

Every field is length-prefixed before it is hashed, and the two labels keep one
role's proof from being replayed as the other's. A process squatting on the
port cannot answer step 2, so the host gives up before it relays a single
browser frame. The squatter only ever sees a random nonce, never the token,
cookies or URLs. A client that sends nothing, sends the wrong frame or
sends a frame over 1 KiB is dropped, and each step times out after 5 seconds.

## Layout

| Path | What it is |
| --- | --- |
| `src/bin/uidm-host.rs` | Native-messaging relay and CLI test mode |
| `src/bin/uidm-daemon.rs` | Daemon: TCP server, auth, shared engine, broadcast, idle exit |
| `src/ipc.rs` | Daemon address, start-up, token file, HMAC handshake |
| `src/app.rs` | Request building and event mapping shared by both binaries |
| `src/protocol.rs` | Framing and message types |
| `src/engine/mod.rs` | Download queue and orchestration: ids, cancel, retry, resume |
| `src/engine/segments.rs` | Splitting a length into byte ranges |
| `src/engine/resume.rs` | The `.uidmpart` resume manifest |
| `src/engine/http.rs` | TLS agent, range probe, `Content-Disposition`, fetches |
| `host/com.uidm.host.json` | Native-messaging manifest template |
| `extension/` | Minimal MV3 test extension |
| `install-host.ps1` | Builds and registers the host for Chrome and Edge |

## The download engine

Given an `add`, the engine:

1. Probes the URL with a one-byte range request. A `206` reply means the server
   supports ranges and reports the total size; anything else falls back to a
   single stream.
2. Splits the total into up to 8 contiguous byte ranges (one connection per
   ~256 KiB, capped at 16), and fetches them on parallel threads. Each thread
   opens its own file handle and writes at its own offset, so writes never race.
3. Records progress in a `<file>.uidmpart` JSON sidecar next to the output,
   flushed about every 1 MiB. If the download stops, re-running it reads the
   sidecar and refetches only the missing bytes of each segment. The sidecar is
   validated against the server's ETag/Last-Modified/size so stale bytes are
   never stitched onto a changed file, and it is deleted on success.
4. Retries a segment that fails with exponential backoff (400 ms doubling to a
   10 s ceiling, up to 5 times), resuming from that segment's own cursor so a
   retry refetches only what is still missing.

The output filename is chosen in priority order: an explicit name on the
request, then the server's `Content-Disposition` (including the RFC 5987
`filename*=UTF-8''...` form, percent-decoded), then the URL path, then a
generic fallback.

Up to 3 downloads run at once (`MAX_CONCURRENT_DOWNLOADS`) and the rest queue.
Every download gets an id, returned in its `ack`; `{"op":"cancel","id":N}`
stops it and keeps the partial file and sidecar so it can resume later.

Verified end to end: a segmented download is byte-identical to a reference
download; a download killed at ~10 MiB of 33 MiB resumes to a byte-identical
file; and a download canceled at 35% resumes on a later request to a
byte-identical file.

The two-process design is verified too. A host closed at 1 MiB of a 33 MiB
download exits at once, and the daemon finishes the file with no client
attached, byte-identical to a reference. A second host reuses the same
daemon and still gets its reply when it closes stdin right after sending. A
fake listener on the daemon port that answers `auth_ok` to everything sees
only `auth_hello`: never the token, the host's proof or the browser's cookies.
The host refuses it and gives up after about 10 seconds.

### Known gaps

- If the daemon itself stops, an interrupted download resumes only when it is
  requested again. The daemon does not yet rescan for `.uidmpart` sidecars at
  start.
- Every connected client receives every download's events, not only its own.
- A process that squats on the port cannot learn anything, but it does stop
  downloads from starting until it goes away. A per-user named pipe would close
  that gap.
- Other processes running as the same user can read the token. That is outside
  this design's threat model: such a process can already read the browser's
  cookies directly.

## Messages

Extension to host (relayed to the daemon unchanged):

```json
{ "op": "hello", "version": "0.1.0" }
{ "op": "ping" }
{ "op": "add", "url": "https://...", "method": "GET",
  "headers": { "User-Agent": "..." }, "cookies": "...",
  "referrer": "https://...", "user_agent": "...", "filename": "f.zip" }
{ "op": "cancel", "id": 1 }
```

Daemon to extension (relayed by the host):

```json
{ "op": "hello", "name": "uidm-daemon", "version": "0.1.0" }
{ "op": "pong" }
{ "op": "ack", "id": 1, "url": "https://...", "status": "accepted" }
{ "op": "started", "id": 1, "url": "...", "total": 12345, "resumable": true, "connections": 8 }
{ "op": "progress", "id": 1, "url": "...", "done": 1048576, "total": 12345 }
{ "op": "done", "id": 1, "url": "...", "path": "C:\\Users\\...\\f.zip" }
{ "op": "failed", "id": 1, "url": "...", "error": "..." }
{ "op": "canceled", "id": 1, "url": "..." }
{ "op": "error", "message": "..." }
```

The `add` payload carries what a hand-off needs to reissue a browser request
outside the page: URL, method, headers, cookies, referrer and user agent. The
`ack` returns an `id`; progress, completion and cancellation events all carry
that id, and a `cancel` references it.

## Build and test

Rust on Windows needs a linker. This project builds with the
`stable-x86_64-pc-windows-gnu` toolchain plus MinGW-w64 (WinLibs) on `PATH`.

```bash
cargo test
cargo build --release
```

`cargo build --release` produces `uidm-host.exe` and `uidm-daemon.exe` in
`target\release`. The host starts the daemon from its own folder, so keep the
two together.

Then wire it to a browser:

1. Open `chrome://extensions`, turn on Developer mode, choose **Load unpacked**,
   and select the `extension/` folder. Copy the extension ID it shows.
2. Register the host:

   ```powershell
   .\install-host.ps1 -ExtensionId <that-id>
   ```

3. Reload the extension, open its popup, and click **Send "add"** or
   **Send "ping"**. Replies print in the service worker console
   (`chrome://extensions` -> the extension -> service worker).

## CLI test mode

Run a download in-process, without a browser or the daemon:

```bash
cargo run --release --bin uidm-host -- download https://example.com/file.zip
cargo run --release --bin uidm-host -- download https://example.com/file.zip C:\path\to\dir
```

Progress prints to stderr; the file lands in the given directory (or your
Downloads folder).
