# Command line and automation

Scripts and other programs reach UDM in four ways:

- **[`udm-gui` with IDM's switches](#idms-switches)**: `/d URL` and the rest, for scripts
  written for Internet Download Manager. The download shows in the window as one added by hand.
- **[`udm-cli`](#the-command-line-tool)**: UDM's own command-line tool. It downloads a file
  and shows its progress on the terminal, lists, pauses, resumes and removes downloads, and starts
  and stops queues, with exit codes a script can test. No window needs to be open.
- **[`udm-host send`](#sending-links-from-other-programs)**: hands one link, or a list for the
  link picker, to UDM as JSON, the way IDM's `SendLinkToIDM2` and `SendLinksArray` do.
- **[The automation API](#the-automation-api)**: the messages UDM's own programs exchange with
  its background service, a documented and versioned part of them, for programs that talk to it
  directly.

Each starts UDM's background service (the daemon) if it is not running. The service keeps
downloading after the program that asked has exited.

## Where the programs are

| | Programs |
|---|---|
| Windows (Setup) | `%LOCALAPPDATA%\Programs\UDM\` (`udm-gui.exe`, `udm-cli.exe`, `udm-mcp.exe`, `udm-host.exe`) |
| Windows (portable zip) | the copy's `target\release\` folder |
| macOS | `/Applications/UDM.app/Contents/MacOS/` |
| Linux (.deb, .rpm) | `udm` (the window), `udm-cli` and `udm-mcp` are on the `PATH`; the rest are in `/usr/lib/udm/` |
| Linux (tar, `install.sh`) | `udm`, `udm-cli` and `udm-mcp` in `~/.local/bin`; the rest in `~/.local/lib/udm/` |
| Linux (AppImage) | `UDM.AppImage udm-cli ...`, `UDM.AppImage udm-mcp`, `UDM.AppImage udm-host ...`; `UDM.AppImage /d URL` for the switches |

Setup does not add UDM's folder to the `PATH`. Call the programs by their full path, or add
the folder yourself.

`udm-mcp` is for AI assistants, which start it themselves; see
[AI assistants (MCP)](ai-assistants.md).

## IDM's switches

`udm-gui` takes the switches IDM's `idman.exe` takes:

```
udm-gui /d URL [/p folder] [/f file_name] [/c checksum] [/a] [/n] [/q] [/h]
udm-gui /s
```

| Switch | Long form | What it does |
|---|---|---|
| `/d URL` | `--download` | Add URL (http, https, ftp or ftps, or a torrent's magnet link) to the list and show the usual download dialog |
| `/p folder` | `--path` | Save in this folder (relative to the current one) |
| `/f file_name` | `--file` | Save under this name |
| `/c checksum` | `--checksum` | Compare the finished file with this checksum (`sha256:...` and the like, or a bare hex digest); a mismatch fails it. UDM's own, not IDM's |
| `/a` | `--add` | Add it to the main queue without starting it |
| `/n` | `--silent` | Ask nothing: no dialog, and a taken name gets a numbered one |
| `/q` | `--quit` | Exit UDM once that download finishes, but only when the call started UDM, as in IDM |
| `/h` | `--hang-up` | Hang up a dial-up connection; accepted and does nothing |
| `/s` | `--start-queue` | Start the main queue |

A magnet link or a `.torrent` file given alone (`udm-gui "magnet:?xt=urn:btih:..."`,
`udm-gui album.torrent`) is downloaded as `/d` would; `udm-gui` reads the `.torrent` file (at most
2.75 MB) and hands it to UDM ([Torrents](torrents.md)).

`/p`, `/f`, `/c`, `/a`, `/n`, `/q` and `/h` need `/d`; without it they are ignored, as in IDM. Each
switch also works as `-d` and the like, and the slash forms take either case. With UDM already
open, the call hands the switches to that window and exits; otherwise it starts UDM in the
tray and runs them there. For example:

```
udm-gui.exe /n /d "https://example.com/files/setup.exe" /p D:\Downloads
```

`udm-gui` returns at once, before the download is done, and shows any problem in the window.
A script that waits for the file, or needs to know whether it arrived, uses `udm-cli get`.

## The command-line tool

```
udm-cli get URL [options]
udm-cli list [--json]
udm-cli pause ID | --all
udm-cli resume ID | --all
udm-cli remove ID [--delete-files]
udm-cli queue start|stop [QUEUE]
udm-cli pair [--host HOST[:PORT]]
udm-cli devices
udm-cli unpair ID
udm-cli help | version
```

### get

`udm-cli get URL` downloads URL now and follows it until it ends. URL may also be a torrent's
magnet link, or a `.torrent` file (`udm-cli get ~/Downloads/album.torrent`), which `udm-cli` reads
and hands to UDM; a torrent ends when it has downloaded, and then seeds in UDM
([Torrents](torrents.md)). It shows no dialog,
whatever **Options** says about asking where to save, and an address already in the list is
downloaded again under a numbered name. The download is added to the list like any other, so the
window shows it too.

| Option | What it does |
|---|---|
| `-d DIR`, `--folder DIR` | Save in DIR (relative to the current folder) instead of the category's folder |
| `-n NAME`, `--name NAME` | Save as NAME instead of the server's name |
| `-c N`, `--connections N` | Open N connections (1 to 16) instead of the number in **Options > Connection** |
| `-H 'Name: value'`, `--header 'Name: value'` | Send this header too; repeat it for more (a `Cookie`, a `Referer`, an `Accept-Language`) |
| `--checksum SUM` | Compare the saved file with SUM (`md5:`, `sha1:`, `sha256:`, `sha512:` or `crc32:` and the digest, or a bare digest); a mismatch fails |
| `--no-wait` | Print the download's id and return as soon as UDM has taken it |
| `-q`, `--quiet` | Show no progress |

Options also take the `--name=value` form. On a terminal, `get` prints a line when the download is
added and when it starts, then one line that keeps changing with the percentage, the bytes in and
the speed. Progress goes to standard error. Once the file is saved its full path goes to
standard output, so a script can read it:

```sh
file=$(udm-cli get -q https://example.com/files/report.pdf -d ~/Reports) || exit 1
echo "saved $file"
```

If the download waits for an answer in the window (its folder cannot be written, or its name is
taken and **Options** says to ask), `get` says so and goes on waiting. Stopping `get` (Ctrl+C)
does not stop the download; `udm-cli pause ID` does.

### list

`udm-cli list` prints a table of every download (id, status, progress, size and name) and the
queues. With `--json` it prints the list as JSON instead:

```json
{ "downloads": [ { "id": 7, "url": "https://example.com/a.iso", "path": "D:\\Downloads\\a.iso",
    "total": 1048576, "done": 1048576, "status": "done", "error": null, "resumable": true,
    "connections": 4 } ],
  "queues": [ { "id": 1, "name": "Main queue", "max_concurrent": 1, "running": false,
    "order": [] } ] }
```

Each download and queue has the shape the automation API's `downloads` and `queues` messages give
them (`entry` and `queue` in the [schema](automation-api.schema.json)).

### pause, resume and remove

`pause ID` stops a running download and keeps what it has, as **Stop** in the window does, and
waits until it has stopped. `pause --all` stops every download and started queue, as **Stop all**
does.

`resume ID` starts it again. A stopped, failed or interrupted download goes on from its partial
file as a new download with a new id, as **Retry** does in the window; `resume` prints the id it
goes on as. A download waiting in a queue, or for its save-as dialog, starts now with its own id.
`resume --all` starts what `pause --all` stopped, and every interrupted download.

`remove ID` takes a download that is not running out of the list. With `--delete-files` its file,
or its partial file, goes to the Recycle Bin or the trash.

### queue

`queue start` starts the main queue, and `queue start Night` (or the queue's id, `queue start 2`)
another; `queue stop` stops one, putting its running downloads back to waiting in it. A queue's
name is matched ignoring case, and may have spaces without quotes.

### pair, devices, unpair

For [remote control from a phone](remote-control.md) without a window, as on a
[server](server.md): `pair` prints a QR code (in the terminal, with block characters), the page's
address, the one-time code and the certificate's fingerprint, as Options' **Pair a phone** shows
them. Remote control must be on (`"remote_enabled": true` in `settings.json`). The link holds the
address the daemon sees; where the phone reaches the computer by another one (a container's
published port), `--host 192.168.1.20` or `--host nas.local:8443` puts that one in the link and the
QR code. `devices` lists the paired phones (number, name, when paired and last seen, in UTC), and
`unpair 2` unpairs phone 2 at once.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Done: the file is saved (and matches its checksum), or UDM did what was asked |
| 1 | The download failed or did not match its checksum, or UDM refused the request (no such download or queue, a download that cannot be paused or resumed, removing a running one) |
| 2 | The command line is wrong |
| 3 | UDM's background service could not be reached or started, or went away |
| 4 | The download stopped before it finished: paused, removed, or put in a queue from the save-as dialog |

What went wrong is on standard error. `udm-cli` writes its own log, `udm-cli.log`, in the
data folder beside the others (see [How UDM fits together](architecture.md)).

A PowerShell script that downloads a file and checks it:

```powershell
$cli = "$env:LOCALAPPDATA\Programs\UDM\udm-cli.exe"
$file = & $cli get "https://example.com/setup.exe" -d D:\Installers -c 8 `
  --checksum sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
if ($LASTEXITCODE -ne 0) { throw "download failed ($LASTEXITCODE)" }
Start-Process $file
```

## Sending links from other programs

`udm-host send` takes what IDM's `SendLinkToIDM2` and `SendLinksArray` COM calls take, as
JSON in a file or on standard input (`-`):

```
udm-host send links.json
udm-host send -
```

One link is an object with `url` and any of `referer`, `cookies`, `post_data` (the form body as
text; it makes the download a POST, sent as `application/x-www-form-urlencoded` unless
`content_type` says otherwise), `user`, `password`, `folder`, `filename` and `user_agent`.
`"silent": true` adds it with no dialog and `"add_only": true` adds it to the main queue without
starting it, like `/n` and `/a`; IDM's `flags` number (1 and 2) works too. It is added as a link
from the browser is, and its download id is printed. A list is an object with `links`, each an
address or an object with `url` and an optional `text` and `cookies`, plus one `referer` for them
all; it opens the link picker with them. A relative `folder` is under the current one, and an
empty string counts as left out. The exit code is 0 when UDM took the links, 1 when it refused
them or could not be reached, and 2 for bad input. From PowerShell:

```powershell
$udm = "$env:LOCALAPPDATA\Programs\UDM\udm-host.exe"
@{ url = "https://example.com/report"; referer = "https://example.com/";
   cookies = "session=abc"; post_data = "id=42"; folder = "D:\Reports";
   filename = "report.pdf"; silent = $true } | ConvertTo-Json | & $udm send -
@{ referer = "https://example.com/files/";
   links = @("https://example.com/a.zip", "https://example.com/b.zip") } |
   ConvertTo-Json | & $udm send -
```

Or from Python:

```python
import json, os, subprocess
udm = os.path.expandvars(r"%LOCALAPPDATA%\Programs\UDM\udm-host.exe")
link = {"url": "https://example.com/a.zip", "user": "me", "password": "pw", "add_only": True}
subprocess.run([udm, "send", "-"], input=json.dumps(link), text=True, check=True)
```

## The automation API

UDM's programs (the window, the browser extension's host, `udm-cli`) are clients of its
background service, which owns the downloads. A program of your own can be one too. The
automation API is the part of their messages that is documented for that and kept stable: version
**1**, described by the JSON Schema [`automation-api.schema.json`](automation-api.schema.json). The
other messages in [Messages](protocol.md) are UDM's own and may change between releases.

### Connecting

The service listens on `127.0.0.1:51515` (another port for a copy started with
`UDM_INSTANCE`). Every frame either way is a 4-byte little-endian length, then that many bytes
of UTF-8 JSON, at most 4 MiB. Before any message, the program proves it runs as the same user:
it reads the token the service wrote to `daemon.token` in UDM's data folder
(`%LOCALAPPDATA%\UDM` on Windows, `~/Library/Application Support/UDM` on macOS,
`~/.local/share/udm` on Linux) and runs the handshake that
[Host-daemon authentication](architecture.md#host-daemon-authentication) describes: each proof is
the hex HMAC-SHA256, keyed by the token, of the label (`udm-daemon` for the service's,
`udm-host` for the program's), the program's nonce and the service's nonce, each written as its
length in 4 little-endian bytes and then its bytes. If nothing listens, start `udm-daemon`,
which sits beside the other programs, and try again for a few seconds.

Then the program says `hello` with a `client` name of its own:

```json
{ "op": "hello", "version": "1.0", "client": "backup-script" }
```

The service answers `{ "op": "hello", "name": "udm-daemon", "version": "0.5.0", "api": 1 }`.
A program checks `api`: a service without it is older than this API. A `hello` without `client`
is taken for the browser extension, and `client: "window"` for the desktop window, which changes
what the service sends; a program uses neither.

### Messages

Requests: `hello`, `ping`, `add`, `list`, `cancel` (stop a download, keeping its partial file),
`retry`, `confirm` (start a waiting download), `remove`, `pause_all`, `resume_all`, `start_queue`
and `stop_queue`.

Events: `hello`, `pong`, `ack`, `rejected` and `error`, which answer the program that asked;
`downloads` and `queues`, which answer `list`; and `added`, `started`, `progress`, `done`,
`failed`, `canceled`, `removed`, `updated` and `queues`, which go to every client as downloads and
queues change.

The schema lists each message's fields and what they mean. The rules around them:

- `add` takes an http, https, ftp or ftps address, or a torrent's magnet link; a `.torrent` file
  goes in `torrent_file`, base64-encoded, with its magnet link as `url`.
- `add` is answered with `ack`, naming the new download's `id`, or `rejected`. Events about the
  download (`added`, even `started` and `done` for a small file) may come before the `ack`, so a
  program keeps what it hears until it has the id. With `"ask": false` no save-as dialog opens,
  and `"duplicate": "new_name"` downloads an address already in the list again. `connections`
  and `checksum` set the download's own; a checksum is compared after `done`, and a mismatch
  turns the download `failed` with an `updated`.
- `cancel`, `confirm`, `remove`, `pause_all`, `resume_all`, `start_queue` and `stop_queue` have no
  answer of their own when they work, and an `error` when refused. The service handles a
  program's requests in order, so sending `ping` after one and waiting for its `pong` shows it was
  handled: an `error` before the `pong` is its refusal.
- `retry` is answered with an `ack` naming the new id the download goes on as, or an `error`.
- A program ignores fields and messages it does not know. Later versions add them without changing
  the API's version; it goes up only for a change an older program would misread.

`udm-cli` is built on these messages alone, and its source (`crates/app/src/bin/udm-cli.rs`
and `crates/application/src/remote/`) shows each command's exchange.

---

[← Back to the help pages](README.md)
