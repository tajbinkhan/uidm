# Messages

The [automation API](command-line.md#the-automation-api) is the documented, versioned part of
these messages for other programs, with a [JSON Schema](automation-api.schema.json); the rest are
UDM's own and may change between releases.

Client to daemon (the host relays the browser's unchanged):

```json
{ "op": "hello", "version": "0.2.0", "client": "window" }
{ "op": "ping" }
{ "op": "add", "url": "https://...", "headers": { "X-Example": "1" }, "cookies": "...",
  "referrer": "https://...", "page": "https://...", "user_agent": "...", "filename": "f.zip" }
{ "op": "add", "url": "https://.../export", "method": "POST", "body": "<base64>",
  "headers": { "Content-Type": "application/x-www-form-urlencoded" } }
{ "op": "add", "url": "https://cdn.../a.zip", "request_headers": [["Accept", "*/*"]],
  "redirects": ["https://.../get/a"], "size": 1048576, "content_type": "application/zip",
  "browser_proxy": { "mode": "fixed", "http": "http://proxy:3128", "bypass": ["<local>"] } }
{ "op": "cancel", "id": 1 }
{ "op": "retry", "id": 1 }
{ "op": "remove", "id": 1 }
{ "op": "confirm", "id": 1, "filename": "f.zip", "folder": "D:\\Downloads", "start": true }
{ "op": "confirm", "id": 1, "file_exists": "overwrite" }
{ "op": "confirm", "id": 4, "duplicate": "existing" }
{ "op": "list" }
{ "op": "pause_all" }
{ "op": "resume_all" }
{ "op": "add", "url": "https://...", "start": false, "queue": 1 }
{ "op": "add", "url": "https://...", "connections": 4, "checksum": "sha256:...", "ask": false,
  "duplicate": "new_name" }
{ "op": "save_queue", "queue": { "id": 0, "name": "Night", "max_concurrent": 2, "retries": 1,
  "when_done": "shut_down", "schedule": { "start": "23:00", "stop": "07:00", "days": [0, 1, 2, 3, 4],
  "date": null } } }
{ "op": "delete_queue", "id": 2 }
{ "op": "start_queue", "id": 2 }
{ "op": "stop_queue", "id": 2 }
{ "op": "set_queue_once", "id": 2, "when_done": "sleep" }
{ "op": "move_to_queue", "id": 7, "queue": 2, "position": 0 }
{ "op": "remove_from_queue", "id": 7 }
{ "op": "cancel_countdown" }
{ "op": "refresh_link", "id": 7, "download": { "url": "https://...", "cookies": "..." }, "restart": false }
{ "op": "refresh_link", "id": 7 }
{ "op": "cancel_refresh" }
{ "op": "add", "url": "https://.../master.m3u8", "filename": "Talk.mp4",
  "media": { "kind": "hls", "video": "https://.../720/index.m3u8", "audio": null } }
{ "op": "add_many", "items": [ { "url": "https://...", "start": false, "folder": "D:\\Photos" } ], "queue": 1 }
{ "op": "add_many", "items": [ ... ], "package": { "name": "Trip", "folder": "D:\\Downloads\\Trip" } }
{ "op": "add_many", "items": [ ... ], "group_archives": true }
{ "op": "unpack_package", "id": 3, "password": "..." }
{ "op": "ungroup_package", "id": 3 }
{ "op": "probe_media", "request": 3, "download": { "url": "https://.../manifest.mpd", "cookies": "..." } }
{ "op": "probe_media", "request": 4, "download": { "url": "https://videos.example/clip/42", "media": { "kind": "page" } } }
{ "op": "pick_videos", "page": "https://www.youtube.com/playlist?list=...",
  "playlist": { "title": "Talks", "entries": [ { "url": "https://www.youtube.com/watch?v=...", "title": "One", "duration": 61.0 } ] } }
{ "op": "pick_gallery", "page": "https://commons.wikimedia.org/wiki/Category:Cats", "user_agent": "Mozilla/5.0 ..." }
{ "op": "read_gallery", "request": 6, "page": "https://commons.wikimedia.org/wiki/Category:Cats" }
{ "op": "install_tool", "tool": "yt_dlp" }
{ "op": "list_tools" }
{ "op": "probe_links", "request": 5, "links": [ { "url": "https://.../a.zip", "referrer": "https://.../page" } ] }
{ "op": "stop_probe_links" }
{ "op": "add", "url": "https://www.youtube.com/watch?v=...", "filename": "Talk.mp4",
  "media": { "kind": "page", "video": "137", "audio": "140" } }
{ "op": "grab", "start": "https://example.com/docs/", "depth": 2, "same_site": true, "types": ["pdf"],
  "max_pages": 200, "max_bytes": 52428800, "delay_ms": 1000 }
{ "op": "cancel_grab" }
{ "op": "command_line", "download": { "url": "https://...", "folder": "D:\\Downloads", "start": false },
  "start_queue": true, "quit_after": false }
{ "op": "get_settings" }
{ "op": "set_settings", "settings": { "download_dir": null, "connections": 8, "max_concurrent": 3,
  "speed_limit_enabled": false, "speed_limit_kbps": 1024, "speed_limit_on_startup": false,
  "speed_limit_per_file_kbps": null, "resume_at_startup": false,
  "file_exists": "rename", "if_duplicate": "ask", "ask_where_to_save": true,
  "categories": [ { "name": "Compressed", "extensions": ["zip", "rar"], "folder": null } ],
  "capture_types": ["zip", "pdf"], "capture_min_size_kb": 0,
  "capture_excluded_sites": ["*.example.com"], "close_to_tray": true, "minimize_to_tray": true,
  "start_with_windows": false, "notify": "toast", "sound_on_done": false,
  "sound_on_failed": false, "scan_after_download": false, "scanner": "defender",
  "scanner_command": "" } }
{ "op": "remote_status" }
{ "op": "start_pairing" }
{ "op": "cancel_pairing" }
{ "op": "remove_device", "id": 2 }
{ "op": "check_debrid", "service": "real_debrid", "key": "..." }
{ "op": "set_debrid_key", "service": "real_debrid", "key": "..." }
{ "op": "remove_debrid_key" }
```

Daemon to clients:

```json
{ "op": "hello", "name": "udm-daemon", "version": "0.5.0", "api": 1 }
{ "op": "pong" }
{ "op": "ack", "id": 1, "url": "https://...", "status": "accepted" }
{ "op": "added", "id": 1, "url": "https://..." }
{ "op": "started", "id": 1, "url": "...", "path": "C:\\Users\\...\\f.zip", "total": 12345, "resumable": true, "connections": 8, "country": "US" }
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
{ "op": "show_done", "id": 1 }
{ "op": "queues", "items": [ { "id": 1, "name": "Main queue", "max_concurrent": 1, "running": false,
  "order": [7, 9], "schedule": null, "when_done": "nothing", "when_done_once": null, "retries": 0,
  "last_scheduled_day": null, "started_by_schedule": false } ] }
{ "op": "packages", "items": [ { "id": 3, "name": "Trip", "folder": "D:\\Downloads\\Trip",
  "downloads": [7, 9], "unpack": { "state": "failed", "reason": { "kind": "missing_part",
  "archive": "show.part1.rar", "part": 2 } } } ] }
{ "op": "countdown", "action": "shut_down", "seconds": 60, "queue": "Night" }
{ "op": "countdown_canceled" }
{ "op": "quit" }
{ "op": "refresh_waiting", "id": 7 }
{ "op": "name_taken", "id": 1, "url": "...", "path": "C:\\Users\\...\\f.zip", "total": 12345 }
{ "op": "refresh_mismatch", "id": 7, "url": "https://...", "reason": "The new link is for a 3.0 MB file; ..." }
{ "op": "progress", "id": 9, "url": "...", "done": 5242880, "total": null, "parts": [40, 120] }
{ "op": "done", "id": 9, "url": "...", "path": "C:\\Users\\...\\Video\\Talk.ts",
  "note": "Saved as a .ts video. With ffmpeg (set in Options) it would be an .mp4." }
{ "op": "added_many", "count": 120, "skipped": 3 }
{ "op": "media_info", "request": 3, "info": { "kind": "hls", "duration": 1520.0, "drm": false,
  "video": [ { "id": "https://.../1080/index.m3u8", "label": "1080p, 5.0 Mbps", "height": 1080,
  "bandwidth": 5000000, "size": 950000000 } ], "audio": [] } }
{ "op": "link_info", "request": 5, "index": 0, "total": 1048576, "content_type": "application/zip" }
{ "op": "tools", "tools": [ { "tool": "yt_dlp", "path": "...\\tools\\yt-dlp.exe", "installed": "2026.10.26",
  "installable": true, "command": "winget install yt-dlp.yt-dlp" } ], "done": "yt_dlp" }
{ "op": "grab_progress", "pages": 12, "bytes": 380211, "found": 31 }
{ "op": "grab_done", "files": [ { "url": "https://.../a.pdf", "name": "a.pdf", "page": "https://.../docs/" } ],
  "pages": 40, "stopped": "Stopped at the budget of 40 pages." }
{ "op": "command_line", "window_open": true }
{ "op": "remote", "state": { "listening": ["0.0.0.0:18443"], "page": "https://192.168.1.20:18443/",
  "fingerprint": "53:79:5D:...", "devices": [ { "id": 1, "name": "iPhone", "paired_at": 1762790000,
  "last_seen": 1762790400 } ], "pairing": { "link": "https://192.168.1.20:18443/#pair=...",
  "code": "...", "expires_at": 1762790300 } } }
{ "op": "debrid_refused", "refused": { "service": "all_debrid", "error": { "kind": "bad_key" } } }
{ "op": "debrid", "check": { "service": "real_debrid", "outcome": { "Ok": { "username": "ann",
  "premium": true, "premium_until": 1765000000 } } } }
{ "op": "error", "message": "..." }
```

`api` in the daemon's `hello` is the automation API's version; daemons
older than the API leave it out. `connections` on an `add` is how many connections
that download opens (brought into 1 to 16) instead of the setting's, and is
kept for its retries. `udm-cli` names itself `client: "udm-cli"`, and
`udm-mcp` `client: "udm-mcp"`.

`command_line` is what `udm-gui` sends for its switches. With a window
open the daemon runs it at once and answers `window_open: true`; otherwise
it holds it for up to a minute and runs it when the next window says
`hello`.

`remote_status`, `start_pairing`, `cancel_pairing` and `remove_device` are
Options' remote-control page (see [Remote control](remote-control.md)); each is
answered with `remote`: where the phone page listens (or its `fault`), the
paired devices (never their secrets), and the pairing code shown, if any.
`start_pairing` is refused with `error` while the page is not served. The
phone page itself speaks HTTPS, not these messages.

`check_debrid`, `set_debrid_key` and `remove_debrid_key` are Options' Debrid
page (see [Debrid accounts](debrid.md)). `check_debrid` asks the service about
the account with the key given, or the key stored for that service without
one, and is answered with `debrid`: the account, or `{"Err": {"kind":
"bad_key"}}` and the other kinds (`not_premium`, `host_not_supported`,
`link_dead`, `limit`, `ip_not_allowed`, `host_unavailable`, `key_for_other`
with the service the stored key is for, and `service` and `network` with the
`message`). `set_debrid_key` stores the key for `service` in place of any
other (a blank key is not a message the daemon reads), `remove_debrid_key`
removes it, and the daemon then sends every client `settings`, whose
`debrid.key_service` names the service a key is stored for. The key goes no
further than the daemon: no message from it carries the key, and a key is
only ever sent to its own service. `debrid_refused` goes to the windows when
the service refuses the account while the daemon reads its list of hosts;
downloads then go on without the account, and it is said at most once an
hour.

`ack`, `downloads`, `remote`, `debrid` and `error` go only to the client that asked, as does
`settings` when it answers `get_settings`; everything else goes to every
client, including `settings` after a change. A status is one of `queued`,
`downloading`, `done`, `failed`, `canceled`, `interrupted`, `confirming`
(waiting for the save-as dialog; `path` is the proposed file) or `waiting`
(added to start later). `updated` replaces a whole entry, for the changes no
other event covers. A `hello` with `client: "window"` tells the daemon a
window is open to show save-as dialogs. `confirm` answers the dialog of a
`confirming` download or starts a `waiting` one, keeping its id; `filename`
and `folder` default to the proposed ones, and `start: false` makes it
`waiting`. `elevate: true` saves it into a folder UDM may not write to,
with administrator rights. `file_exists` (`rename` or `overwrite`) decides what happens if
the name is taken, for this start and later ones; without it the setting
decides. `name_taken` means the name is taken under the `ask` setting:
nothing was written, and the download is `confirming` with `path` set to the
taken file. A `confirming` download whose address is already in the list
carries `duplicate_of`, the other download's id, and `confirm` answers it
with `duplicate`: `new_name` (the default), `overwrite` (the other one is
removed and its file is the target, whatever `filename` and `folder` say;
refused as `existing` while the other one runs) or `existing` (the new
download is `removed`; the other one is shown again with `show_done` if
finished, retried if stopped). Canceling a `confirming` download reports
`canceled`. `retry` removes
the old entry (`removed`) and queues a new one with a new id; it is refused
unless the download failed, was canceled or was interrupted. `remove` is
refused while a download is still running. `set_settings` with an invalid
value (`connections` outside 1-16, `max_concurrent` outside 1-10, a speed
limit outside 1-10,000,000 KB/s, or a folder that is not a full path) is
refused with an `error` that says why. `download_dir: null` means the
Downloads folder, `file_exists` is `rename`, `overwrite` or `ask`, and
`if_duplicate` is `ask`, `new_name`, `overwrite` or `existing`. A category
needs a unique name other than `General`, at least one file type, and a full
path or `null` for its folder (`null` means a folder named after it inside the
download folder). File types are lowercase letters and digits without the
dot. `notify` is `toast`, `dialog` or `off`; `scanner` is `defender` or
`custom`, and a custom `scanner_command` must name a program and contain
`{path}`. Turning `start_with_windows` on or off adds or removes the `UDM`
value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, which
runs `udm-gui.exe --minimized`.

A finished download's entry carries `"scan": { "result": "scanning" }`, then
`clean`, `threat` or `failed` (the last two with a `detail`, the end of the
scanner's output) when scanning is on. `pause_all` cancels every queued and
running download; `resume_all` retries those and every interrupted one. `show`
asks the window to come forward, after a click on a notification.

`add` with `"start": false` puts the download in `queue` (the main queue,
id 1, by default) as `waiting`, without probing it. `confirm` takes `queue`
too, for **Download later** in the save-as dialog, and starts a `waiting`
download now. `queues` goes to the client that sent `list` (after
`downloads`) and to every client when a queue changes. `save_queue` with id 0
creates a queue; with an id it changes the name, `max_concurrent` (1 to 10),
`retries` (0 to 10), `when_done` and `schedule` and keeps the queue's
downloads and state. Names must be unique, ignoring case. `delete_queue`
moves the queue's downloads to the main queue, which cannot be deleted.
`move_to_queue` with a `position` in the same queue only reorders; joining a
queue makes a failed, canceled or interrupted download `waiting` there.
`when_done` is `nothing`, `exit`, `sleep`, `hibernate` or `shut_down`.
`set_queue_once` overrides it for the current run; stopping the queue clears
it. A schedule's times are `HH:MM` local time; `days` count 0 = Monday to
6 = Sunday, and `date` (`YYYY-MM-DD`) replaces them for a single run.
`countdown` goes to every client, and to a window that connects while one is
running; `quit` tells windows to close.

`media` on an `add` downloads `url` as a stream: `kind` is `hls` or `dash`
(missing: found from the playlist itself), and `video` and `audio` are option
`id`s from `media_info` (missing: the best, or the default audio). A stream's
`progress` carries `parts`, segments done and in all, and its `total` is
`null`; entries keep `parts` and a finished download's `note`. `add_many`
answers with `added_many` and adds each item waiting, skipping items that are
not web addresses, and items already in the list unless `if_duplicate` is
`new_name`, or `overwrite` for a stopped download (`skipped` counts those);
`folder`, a full path, fixes where an item is saved. With `package` (a
`name`, and a `folder` every item is saved in) the items added become one
package; with `group_archives` the parts of each split archive among them
(`x.part1.rar`, `x.rar` with `x.r00`, `x.7z.001`, `x.zip` with `x.z01`)
become a package of their own. Older clients send neither, and nothing is
kept. `packages` goes to the client that sent `list` (after `queues`) and to
every client when one changes; a package's `unpack` `state` is `waiting`,
`unpacking`, `needs_password` (with `wrong` after a wrong one), `done` (with
the `folder` and `files`) or `failed` (with a `reason` whose `kind` is
`no_archives`, `not_complete`, `missing_part`, `needs_seven_zip`, `unsafe`,
`too_many_files`, `no_space`, `damaged` or `io`). `unpack_package` unpacks one
in the background, with `password` for encrypted archives (never logged or
saved), and is refused while it is unpacking; `ungroup_package` stops keeping
one, and its downloads stay. A download that leaves the list leaves its
package, and a package left empty goes.
`probe_media` echoes `request` in its `media_info`, whose `error` says why a
stream cannot be downloaded and `drm` whether that is because of DRM. With
`media.kind` `page` any http or https page is read with yt-dlp; a playlist
or channel answers with `playlist` (its title and videos) instead of
qualities, and `pick_videos` hands those videos to the window's playlist
dialog (no answer; at most 1000, http and https pages only). `kind: "page"`
on an `add` also takes `max_height` (the tallest video, for a playlist's
videos) and `audio_only` `mp3`, `m4a`, `opus` or `flac`. `pick_gallery`
(the browser's "Download gallery") opens the window's gallery dialog on an
http or https `page` (no answer). `read_gallery` lists the page's pictures
with gallery-dl and answers that client alone with a `gallery` echoing
`request` and `page`: `gallery` holds the `title` and `pictures` (each a
`url`, and when known a `name`, `extension`, `width` and `height`; at most
1000), or `error` and `error_kind` say why there are none. `install_tool`
(`yt_dlp`, `ffmpeg` or `gallery_dl`) downloads the tool's official build, checks it
against its release's published SHA-256 and installs it in UDM's tools
folder; it answers `tools` with `done` naming the tool and, when it was not
installed, `error` and `error_kind`. `list_tools` answers `tools` alone.
`probe_links` (the window's link picker) asks each link's server for its size
and type, four at a time, and answers each with a `link_info` naming its
`index` in `links` (or an `error`), to that client only; a new `probe_links`
or `stop_probe_links` stops the client's last, as does disconnecting.
`set_properties` with a `folder` for a stopped, unfinished download moves its
partial file and sidecar there. `grab`
answers with `grab_progress` after each page and one `grab_done`, to the
client that asked only; a client runs one crawl at a time, and its crawl
stops if it disconnects. `grab` is refused with an `error` unless `start` is
an `http(s)` address, `depth` is 0 to 10, `max_pages` 1 to 10,000,
`max_bytes` 1 to 2,000 MB, `delay_ms` 200 to 60,000 and `types` is not empty.
The settings gain `clipboard_monitor`, `video_button`,
`video_button_excluded_sites`, `ffmpeg_path` (a full path to
`ffmpeg.exe`, or empty to look in UDM's tools folder and on `PATH`) and
`ytdlp_auto_update` (update a yt-dlp UDM installed once a day).

---

[← Back to the help pages](README.md)
