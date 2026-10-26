# UIDM

A download manager, rebuilt from scratch in Rust. It has two layers so far: a
browser-integration layer (a native-messaging host and a test extension that
hands captured download requests to it) and a download engine that fetches a
file over parallel byte-range connections and resumes after an interruption.

## Why this layer first

A browser download manager has two halves: an extension that watches the
browser and decides what to grab, and a desktop app that does the downloading.
They talk over the browser's native-messaging channel. That channel is the
contract everything else hangs off, so it comes first.

The framing is the browser's public standard: a 4-byte little-endian length,
then that many bytes of UTF-8 JSON, both directions, over the process's
stdin/stdout. The message shapes below are UIDM's own and can change freely,
since UIDM owns both ends.

## Layout

| Path | What it is |
| --- | --- |
| `src/main.rs` | Host message loop, stdout writer, and CLI test mode |
| `src/protocol.rs` | Framing, message types, and tests |
| `src/engine/mod.rs` | Download orchestration: threads, progress, resume |
| `src/engine/segments.rs` | Splitting a length into byte ranges |
| `src/engine/resume.rs` | The `.uidmpart` resume manifest |
| `src/engine/http.rs` | TLS agent, range probe, range/whole fetch |
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

Downloads are queued: the host runs up to 3 at once (`MAX_CONCURRENT_DOWNLOADS`)
and the rest wait, so a burst of captures does not open unbounded connections.
Every download gets an id (returned in its `ack`); `{"op":"cancel","id":N}`
stops it and keeps the partial file and sidecar so it can resume later.

Verified end to end: a segmented download is byte-identical to a reference
download; a download killed at ~10 MiB of 33 MiB resumes to a byte-identical
file; two queued downloads in host mode both complete; and a download canceled
at 35% resumes on a later request to a byte-identical file. The host exits
cleanly when the browser closes the port.

### Known gaps

- The engine runs inside the host process, so downloads stop if the browser
  closes the port. Moving it into a long-lived desktop process is the intended
  architecture; the engine module is self-contained so it can move.

## CLI test mode

Run a download without a browser:

```bash
cargo run --release -- download https://example.com/file.zip
cargo run --release -- download https://example.com/file.zip C:\path\to\dir
```

Progress prints to stderr; the file lands in the given directory (or your
Downloads folder).

## Messages

Extension to host:

```json
{ "op": "hello", "version": "0.1.0" }
{ "op": "ping" }
{ "op": "add", "url": "https://...", "method": "GET",
  "headers": { "User-Agent": "..." }, "cookies": "...",
  "referrer": "https://...", "user_agent": "...", "filename": "f.zip" }
{ "op": "cancel", "id": 1 }
```

Host to extension:

```json
{ "op": "hello", "name": "uidm-host", "version": "0.1.0" }
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

```bash
cargo test
cargo build --release
```

Then wire it to a browser:

1. Open `chrome://extensions`, turn on Developer mode, choose **Load unpacked**,
   and select the `extension/` folder. Copy the extension ID it shows.
2. Register the host:

   ```powershell
   .\install-host.ps1 -ExtensionId <that-id>
   ```

3. Reload the extension, open its popup, and click **Send "add"** or
   **Send "ping"**. Replies print in the service worker console
   (`chrome://extensions` -> the extension -> service worker), and the host's
   own log goes to the browser's native-messaging stderr capture.

## Next

The `add` handler in `src/main.rs` is the seam for the download engine:
segmented HTTP range requests, a resume store, and a scheduler.
