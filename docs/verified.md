# Verified

- Screenshots and step-by-step guides (#269). `node tools/screenshots/shoot.js`
  ran here (Ubuntu 24.04, Playwright's Chromium) and wrote every picture in
  `docs/images/` except the Linux ones, each under 200 KB at twice the
  window's size; each was looked at in both looks. The richer sample list
  (`crates/ui/src/fixtures.rs`) was written with `UDM_WRITE_FIXTURES=1`, and
  the frontend's tests pass on it (their expectations follow the sample's
  new file names and category). The download window's labels were drawn in
  English, Spanish, Brazilian Portuguese, Ukrainian and German on the preview
  page: the labels' column grows for the longest label and none runs into
  its value, on the status and the Speed Limiter tabs. The video button is the extension's content
  scripts on a page the script serves to the browser itself, with the
  worker's menu built by `media.js`; the browser reached no network. The
  Linux pictures are the debug `udm-gui` and `udm-daemon` under Xvfb and
  openbox with a scratch home, fed by `tools/bench/faultserver.py` and
  `udm-cli get` ([how](../tools/screenshots/README.md#the-linux-shot)). The
  project page was put together as `pages.yml` does and drawn at 1200 and 390
  pixels wide in both looks, without sideways scrolling. Every menu, button
  and setting the guides name was checked against the frontend's components
  and `crates/ui/src/lang/en.json`. Not tried: the script on Windows or
  macOS, where the windows get those systems' fonts, and the Pages workflow
  itself.

- The extension made ready for the browser stores (#248). The extension's
  tests build both store zips into a temporary folder and read them back with
  a zip reader of their own (each entry's local header, size and CRC): every
  file of `extension/chromium` and `extension/firefox` at the root, sorted,
  with one fixed time, byte for byte as in git; the Chromium manifest without
  its `key` and otherwise unchanged; the Firefox one with `udm@udm.local`; the
  same bytes on a second run; no zip from a stale Firefox copy; no hidden
  files. Python's `zipfile` and `unzip -l` read the zips too. A Rust test
  checks that the host manifests, Setup, `install-host.ps1`, `AppRun` and
  `extension/firefox.json` name the same extension IDs as `browsers.rs`, and
  that the unpacked ID is the one the manifest's key gives; it failed with a
  made-up ID added to one file only. It reads Setup's `ChromiumIds` define
  and `install-host.ps1`'s `$ExtensionId` default themselves, not every
  word that looks like an ID. A manifest that lacks an ID UDM lets in is
  written again, and one that lets in more is kept. A Node test checks the
  extension's version is the workspace's. Not tried: Inno Setup and
  PowerShell (no Windows here), including Setup rewriting the data folder's
  manifests; the Release workflow's new jobs, `web-ext sign` and the Chrome
  Web Store API v2, which need the stores' accounts; and the listings
  themselves.

- More interface languages (#251). Thirteen tables were added to the window and
  the extension: Indonesian, Italian, Dutch, Polish, Brazilian Portuguese,
  Vietnamese, Turkish, Russian, Ukrainian, Japanese, Korean and Simplified and
  Traditional Chinese. The window's tests check every table has every string
  with its placeholders, that each language has one line whose code Options can
  save and that resolves to it (`zh-HK` and `zh-Hant` to Traditional Chinese,
  `pt-PT` to Brazilian Portuguese), that decimals and dates follow the language,
  and that every character of a language without a system font of its own has
  a glyph in egui's fonts (which caught Vietnamese's stacked accents, now drawn
  with a system font). On Linux, fontconfig's Noto Sans CJK faces (JP, SC, TC)
  and Noto Sans were loaded through the window's font path and covered every
  character of the Japanese, Korean, both Chinese and the Vietnamese tables. A
  file that is not a font is refused before egui could stop on it. The
  extension's tests check every new locale. The Windows font files (Microsoft
  YaHei, JhengHei, Yu Gothic, Malgun Gothic, Segoe UI) and macOS's were not
  tried, the window was not looked at in each language, and no native speaker
  has read the new tables.

- Packages and unpacking (#258). Domain tests cover every split-archive
  naming (`.part1.rar`, `.part01.rar`, `.rar` with `.r00`, `.7z.001`,
  `.zip.001`, `.tar.001`, `.zip` with `.z01`), single archives (`.zip`,
  `.7z`, `.rar`, `.tar`, `.tar.gz`, `.tgz`, `.tar.xz`, `.txz`, `.tar.bz2`,
  `.gz`, `.xz`, `.bz2`), names that are not archives, parts in any order,
  repeated and missing parts, archives of one name in different families
  kept apart, package names (trimmed, too long, not a folder, a device name),
  completeness, the space margin, numbered free names, a password that never
  shows in `Debug`, and the entry paths unpacking refuses (`..`, absolute
  paths, `\`, drives, NTFS streams, NUL). Use case tests, on in-memory ports,
  keep links as one package or each split archive as its own, unpack from the
  first part into the folder the settings name, ask for a password and go on
  with the right one, refuse missing and unfinished parts and a package
  without archives, keep a failure, recycle the archives only when asked
  (through a fake, never the real Recycle Bin), and start only a complete
  package with unpacking on. Infrastructure tests unpack into temporary
  folders: a zip with folders, an AES-encrypted zip (no password, a wrong
  one, the right one), `.tar.gz` and `.tar.xz` without their symbolic link,
  a `.gz`, and a zip cut into three pieces; files already there kept and
  the new ones numbered, a folder already there merged; zip and tar entries
  leaving the folder (`../`, `/tmp/`, `C:\`) refusing the whole archive with
  nothing written; eight megabytes of zeros refused before writing when they
  would leave too little room. A stand-in 7-Zip (a shell script) shows the
  password arriving on its standard input and not in its arguments, an
  encrypted entry asking for the password before extracting, a listed path
  leaving the folder refused before extracting, and a link in its output
  refused with nothing moved. `history.json` keeps packages, drops missing
  downloads and empty packages, waits again after unpacking was cut off,
  and loads (and saves) a file from before packages without them. The wire
  tests pin `add_many`'s `package` and `group_archives`, `unpack_package`
  (its password hidden from `Debug`), `ungroup_package` and `packages`, and
  an older client's `add_many` keeps no package. A daemon test downloads a
  tar cut into two pieces from a local server, keeps them as one package,
  and unpacks it on its own once both are in. The window was built and its
  strings translated into German, Spanish and French, but not run: the link
  picker's package box, the Packages branch, its menu, the password dialog
  and the Options rows were not tried on screen. No real 7-Zip, RAR or 7z
  archive was tried.

- Remote control from a phone (#254). Domain tests cover pairing codes (128
  bits, read back however typed), credentials, verifiers, device names, a code
  that works once and runs out, a new code replacing the old, the wrong-try
  limits per address and overall, removal, the device limit and the settings
  (off in an old `settings.json`, odd ports and addresses refused). Use case
  tests run pairing, checking, removing and each phone request against
  in-memory ports and a pretend service: list, add (no folder, no name, no
  dialog), pause, resume, remove with files kept unless asked, nothing reaching
  the daemon without a paired credential, each request on a connection of its
  own. Infrastructure tests run the real HTTPS server on 127.0.0.1 with its
  own certificate: the page and its safety headers, no CORS header, 401 for
  every API request without a paired credential, wrong and expired codes, the
  rate limits (429 even for the right code), each action, a removed device
  refused at once, other sites' requests and form posts refused, nothing
  listening once turned off, a port in use reported, no response holding a
  token-like secret planted in the list, the pairing code or a folder, and
  (on Linux) the certificate verifying for `localhost`. The certificate and
  device files are checked sealed and 0600. A daemon test pairs through the
  window's `start_pairing` and drives the real service: list, add, pause,
  resume, pause, remove, then the window's `remove_device` refuses the phone.
  On Linux, a named copy with remote control on 127.0.0.1 made its
  certificate, listened, answered curl (`200` for the page with its headers,
  `401` and `403` for the API), showed the SAN and serverAuth in `openssl
  s_client`, stayed running past the idle minute, and logged no token. The
  phone page ran in a browser at phone size against a stand-in API: pairing
  (wrong and right code), the list (a name with markup shown as text), add,
  pause, remove with the file kept, and an unpaired credential sending it
  back to pairing. Options' Remote control page was built and its strings
  translated, but not looked at on screen; no real phone (iOS Safari, Android
  Chrome) was tried; the Windows and macOS code (SChannel and
  Security.framework serving the certificate) was linted from Linux only.
  After server mode (#253) came in, the daemon test runs on a server's
  daemon too, and `udm-cli pair`, `devices` and `unpair` have use case
  and report tests (the QR code, `--host` in the link); the Docker
  `ports` lines were not run.

- Server mode and the Docker image (#253). Unit tests cover the daemon's
  command line (`--server` and its folders, a folder without `--server`,
  a missing or repeated one, unknown words, the elevated move), folders
  named on the command line winning over the environment, `Seat`, and, on
  a server, no notification or window whatever the settings say and a
  save-as dialog starting as proposed. A behavior test on the real adapters
  checks that a server's daemon with nothing to do is idle but never may
  exit. On Linux, a named copy ran `udm-daemon --server` with its
  folders from the command line and then from the environment alone: it
  logged to standard output, listened on 127.0.0.1 only, downloaded a file
  `udm-cli` asked for (with `UDM_DATA_DIR` set) into the downloads
  folder, kept running past the idle minute, kept the list across a
  restart, exited 0 after saving on `SIGTERM`, refused to start (exit 1)
  beside a running one, and left the XDG folders empty. Docker was not
  installed, so the `Dockerfile`, the Compose file, the Unraid template
  and the release workflow's image job were not built or run; the build
  command in the `Dockerfile` was run outside it. The Windows and macOS
  code was linted from Linux only.

- Screen readers and the keyboard (#252). AccessKit was already on through
  eframe's default features; it is now named in `crates/ui/Cargo.toml`, and
  Cargo.lock is unchanged. Unit tests cover what a row reads (name, status,
  progress, speed, time left, why it failed), where Up, Down, Page Up, Page
  Down, Home and End land, and, on a headless egui context, that the keys
  select rows with the focus following, Shift extends and Ctrl moves the
  focus alone, Space selects, and Shift+F10 opens the row's menu with the
  focus in it and Escape gives it back. A test checks that
  `docs/keyboard-shortcuts.md` names every key the window's code answers.
  On Linux, a named copy ran on a private session bus with its own AT-SPI
  bus (no setting of the real session changed), and the tree read through
  AT-SPI named the window, every menu and toolbar button, the categories
  as a tree, the list as a list box with each row read as its name,
  status, progress and failure, the column headers, and the column and
  panel grips; nothing that takes the focus was left unnamed. Moving the
  focus through AT-SPI was accepted but could not be seen, since the window
  was not in front. Orca itself was not run, keys were not typed into the
  real window, and the focus ring was not looked at on screen. Narrator,
  NVDA and VoiceOver were not tried; the Windows and macOS code was linted
  from Linux only.

- Fault injection and the benchmark harness (#270). The engine's test server
  drops connections, limits connections per client, starts slowly, answers
  wrong ranges and limits speed; engine tests against it check that dropped
  connections resume and fetch no byte twice, that refused connections hand
  their ranges over (each refused once, the download finished with fewer
  connections), that wrong ranges never reach the file, that more
  connections beat a per-connection limit, and that a server with every
  fault at once still gives a byte-identical file. Two of them failed on
  the engine as it was, and pass now. On Linux, `tools/bench/bench.py` ran
  a release build of `udm-cli` and its own daemon (a named copy in a
  scratch folder, notifications off), curl and wget against
  `tools/bench/faultserver.py` in every scenario; every UDM run was
  byte-identical, curl failed `drops` as expected, and the daemon was gone
  afterwards. The IDM side (`IDMan.exe /d`, its CPU time through
  `GetProcessTimes`) and the harness on Windows and macOS were not run:
  there is no IDM result yet.

- Themes, high contrast and an interface size up to 200% (#264). Unit tests
  cover colors and theme files (a base, overrides, comments, every wrong
  line), the High contrast theme's 7:1 contrast, which look each Theme
  setting gives with and without the system's contrast colors and with a
  theme file gone, reading the old `settings.json` theme and odd values,
  and the themes use case (sorting, a name two files give). On Linux, a
  named copy started at 200% with High contrast, then with a theme file at
  100%: the main window and Options drew in those looks, Options' pages
  fit the screen with Save in view, and a broken theme file was reported
  under the Theme list. Windows' contrast colors were linted from Linux but
  not run there, and macOS was linted only.

- The MCP server for AI assistants (#255). Use case tests run each tool's
  request against a pretend daemon (one connection kept between requests,
  opened again when it went between them, a request that lost it failed,
  files kept unless deleting them was asked for). Protocol tests feed
  JSON-RPC lines through the server: the handshake and version choice, the
  tool list and its schemas and hints, each tool's arguments and results,
  wrong arguments as the tool's error, refusals, JSON-RPC errors and
  batches. A daemon test runs `add_download` (now, into a folder, and into
  the main queue), `list_downloads` with a status, `remove_download` and a
  refused `pause_download` through the server on the real service and
  engine against a local server. On Linux, the built `udm-mcp`, fed
  `initialize`, `tools/list` and tool calls on standard input, started a
  named copy's daemon, downloaded a file from a local server, queued another
  in the main queue, listed them, refused to pause a finished one, removed
  one keeping its file, refused an unknown queue and a `file://` address,
  and exited when its input closed. It was not tried in a real assistant,
  and the Windows and macOS code was linted from Linux but not run there.

- The command-line tool and the automation API (#256). Use case tests run
  each `udm-cli` command against a pretend daemon; daemon tests run
  `get`, `list`, `remove`, `pause` and `resume` on the real service and
  engine against a local server (no dialog under ask, a second copy under a
  new name, the checksum read back, the download's own connections kept for
  its retry). On Linux, the built tool started a named copy's daemon, saved
  a file with a matching checksum (exit 0) and failed one that did not
  (exit 1), reported a 404 (exit 1), paused, resumed, paused all and resumed
  all a running download, refused to remove a running one, started the main
  queue and refused an unknown one, and drew the progress line under a
  pseudo-terminal. A test checks the JSON Schema's examples and the messages
  built in Rust against each other. The Windows and macOS code was linted
  from Linux but not run there, and the installers were not built.

- Portable mode (#265). Unit tests cover finding `portable.txt` beside the
  programs and above `target/release`, and every folder a portable copy
  uses, named copies included. On Linux, a daemon copied into
  `target/release` under a folder holding `portable.txt` and started as a
  named copy wrote its token and log in that folder's `Data-<name>` and
  nothing in `~/.config` or `~/.local/share`. The Windows code (no
  notification registration or sign-in entry, registering the browsers it
  finds, `install-host.ps1` and the release zip's marker) was linted for
  Windows from Linux but not run on Windows, and the greyed-out Options
  checkbox was not tried on screen.

- Duplicate addresses (#30). Daemon tests run the real engine against a local
  server: with a new name the copy is saved as `f (1).bin` and the old file
  is untouched; replacing writes over the old file in the folder it was saved
  to and drops the old entry, but keeps a running one and adds nothing;
  using the existing download sends `show_done` for a finished one, retries
  a failed one, and adds the new one when the finished file is gone; under
  ask the download waits as `confirming` with `duplicate_of`, then follows an
  `existing` or `overwrite` answer, and no answer gives a new name;
  `add_many` skips a duplicate under ask and adds it under new name; a form
  POST is never a duplicate. Unit tests cover address matching, the setting's
  default and `confirm` with `duplicate`. The window's dialog and Options
  choice were built but not tried on screen, and nothing ran through the
  host or the browser.

- The installer. Built with Inno Setup 6.7.3, then installed and uninstalled
  silently on Windows 11. The install put the three programs, the extension
  (without its tests) and the icon in `%LOCALAPPDATA%\Programs\UDM`. It
  wrote a host manifest pointing at the installed `udm-host.exe`, pointed
  the Chrome and Edge keys at it, and added the Start menu shortcut and the
  Installed apps entry. Started the way Chrome starts it (the manifest's
  path, the extension origin as argument), the installed host started the
  installed daemon, which downloaded a file intact. Uninstalling while that
  daemon ran closed it, then removed the folder, both keys, the shortcut, the
  Installed apps entry and the notification registration, leaving
  `%LOCALAPPDATA%\UDM` alone. Installing over a running daemon closed it
  and replaced the files without a restart. The wizard pages and their
  prompts were not tried on screen.
- Asking about taken file names (#8). Engine tests against a local server:
  under ask, an existing file and one with a stale sidecar both stop with
  `name_taken` before anything is written, then rename and overwrite answers
  save as `name (1)` and replace the file; a free name and a matching sidecar
  (a resume) go through without asking; a POST is renamed; a stream stops
  before fetching a segment. Unit tests cover `name_taken` in the download
  table and `confirm` with and without `file_exists`. The extension test
  shows a `name_taken` download in the popup and starts it with a new name.
  End to end, a daemon and host run from a scratch folder (with
  `LOCALAPPDATA` and `APPDATA` pointed there) against a local server, driven
  through the host as the browser does: overwrite and rename answers, no
  answer (no window could open, so after 20.0 s it was saved as
  `name (2)`), and **Download later** with rename, then started, keeping
  the choice. The window's dialog was built but not tried on screen.
- Streams, batches, lists and the grabber (milestone 6). Unit tests cover URL
  patterns (padding, steps, counting down, letters, several ranges, other
  brackets kept, the 10,000 cap and overflow), IDM's `*` form (numbers with
  a wildcard size, letters, From after To and other mistakes refused, the
  same cap), pasted lists, the clipboard
  link rule, list export and import (round trips in both formats, exports
  with exactly the five list fields, bad folders and repeats dropped, newer
  formats refused), HLS (attribute lists, master playlists with audio groups,
  keys and IVs, byte ranges, `#EXT-X-MAP`, FairPlay, Widevine and session-key
  DRM, live playlists), DASH (ISO durations, template filling, fixed-duration
  and timeline templates including `r="-1"`, segment lists with ranges,
  single files, DRM, live and multi-period manifests), HDS (F4M 1.0 with
  inline and linked bootstrap info and several bit rates, F4M 2.0 child
  manifests, the manifest's query and `<pv-2.0>` carried to every fragment,
  hand-built `abst` boxes with several segment runs, fragment addresses,
  `mdat` extraction from F4F fragments, FLV headers with `onMetaData`, DRM
  and live refused), the yt-dlp sites (YouTube, Facebook, Instagram and
  Vimeo video pages accepted, their other pages refused), AES-128 decryption,
  `robots.txt` (groups, wildcards, `$`, `Crawl-delay` and its cap), link
  extraction (only web links, nothing from script text, `<base href>`), and
  crawls (depth, same site, robots, one wait before every request, budgets,
  stopping). Engine tests against a local server: an HLS master playlist
  downloads its best quality, decrypting its last two segments, and joins
  them byte-identical, with progress up to 5 of 5 segments and the segment
  folder removed; another quality by its id; a download with two segments on
  disk and a half-written third fetches only the three missing (progress
  starts at 2 of 5, and the key is fetched once); a DASH stream without
  ffmpeg is saved as video and audio files with a note; with a stand-in
  `ffmpeg.bat` the two tracks go to it with `-map 0:v:0? -map 1:a:0? -c
  copy` and come back as one file, and a failing ffmpeg fails the download
  with its message and keeps the segments; a DRM stream is refused, and
  probing reports DRM, qualities, sizes and the length; an HDS manifest
  signed with a query is probed for its two bit rates and downloads its two
  F4F fragments into one valid `.flv` (directly and through an F4M 2.0
  manifest), or an `.mp4` with ffmpeg.
- Under Wine against the real host and daemon: `add_many` adds five pattern
  files waiting in the main queue with paths in the chosen folder (and drops
  a `javascript:` item), starting the queue saves them there; `probe_media`
  answers with two qualities and their sizes, and DRM for a FairPlay
  playlist; an HLS download with AES-128 segments lands in the Video folder
  byte-identical with `parts` and its note; a DASH download is saved as two
  files with a note; a DRM stream fails with the DRM message; a crawl lists
  the two PDFs it may see, skips the folder `robots.txt` disallows, sends the
  grabber's user agent and never fetches the files; stopping a crawl with a
  five-second wait ends it within 0.1 s; a request with a 1 ms wait is
  refused.
- In the window under a virtual display: **More** with its four items; **Add
  many** previewing 40 addresses for `photo[001-120:3].jpg`; the site grabber
  crawling a local site, showing progress and then its four files to tick,
  and adding them to the main queue with a notice; ticking two downloads and
  exporting them through the Windows save dialog to the JSON shown above;
  importing that file back into the **Import downloads** dialog; the new
  Options fields; and the clipboard prompt for a copied `.exe` link, not
  shown again for the same link after **Ignore**, nor for an `.html` link.
- In Chromium (Playwright's build, which loads unpacked extensions) with the
  extension and a native-messaging manifest pointing at the Windows host
  under Wine: the button appeared over a playing video; its menu listed the
  page's HLS stream with 720p and 360p and their sizes (the variant
  playlist the player had loaded folded into it); choosing 720p downloaded
  all four segments into `Video\Canvas Test Video.ts`, named after the page;
  after the video raised an `encrypted` event the menu said it is DRM
  protected; the popup's per-site switch added and removed the site in
  UDM's settings.
- 20 more extension tests (27 in all): classifying responses (playlists by
  type and name, files with their size from `Content-Range`, segments, small
  files and errors left out), per-tab lists (one entry per URL, a size learned
  later, the oldest dropped past 50), the menu (folding a master's variants,
  DRM), the per-site switch, safe file names from titles, and the real worker
  probing a stream once, sending the chosen quality as `media` with the
  page's title and referrer, refusing another tab's entry, forgetting a tab's
  list on a new page, ignoring the popup's commands from content scripts,
  and changing the video button settings from the popup.
- 102 unit tests: segment splitting, the resume sidecar, framing,
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
  Re-splitting: with one connection served at 128 KiB/s, a 4 MiB download
  over 4 connections took 8.0 s with fixed ranges and 4.1 s with
  re-splitting, byte-identical; one canceled after its ranges were split saved
  the extra ranges in its sidecar and resumed byte-identical. A server that
  sends a file only to a POST with the right form, and an HTML page to a GET,
  gives the file to the POST in one request, named by its
  `Content-Disposition`.
- 19 extension tests: each capture rule, and the real `background.js` run
  with a stand-in `chrome` API: it asks for the rules on connect, takes or
  leaves downloads by type, size and site, applies changed rules at once,
  skips exactly one download after **Skip next download**, and starts a
  download waiting for its dialog from the popup; **Download later with
  UDM** adds with `start: false`. Form downloads: URL-encoded forms rebuild
  as the browser encodes them, multipart forms with a boundary of their own
  and without their file fields, and raw bodies byte for byte; forms that
  send a file and bodies the browser withheld stay in the browser; a
  download matches the latest POST to its URL within a minute; a form's
  download reaches UDM as the same POST with its content type while a link
  next to it stays a GET; and a POST UDM cannot start goes back to the
  browser as a POST with its form. A web player's file a script asked for
  with a POST goes as the same POST. The browser's whole proxy setting goes
  along with a download, read from Chrome's and Firefox's shapes. A
  download's request headers (without cookies), redirects, size and type go
  along; Chrome hides the bubble from the headers until the download is
  taken or left, Firefox cancels at the headers, a tab opened only for the
  download is closed, and a link in an `about:blank` frame takes the frame
  above it as referrer. While a
  download waits for a new link, the next browser download (even one the
  capture rules would leave) goes to it as `refresh_link`, once.
- Proxies, logins and new links. Unit tests cover site matching, proxies by
  scheme with credentials and bypass rules, Windows' proxy setting (read from
  the registry, off and on), Digest against the worked examples of RFC 2617
  (MD5) and RFC 7616 (SHA-256), NTLMv2 against the MS-NLMP test vectors and
  a local server that keeps its challenge per connection, Basic, the most
  specific login, Credential
  Manager and DPAPI round trips, passwords never reaching the settings file,
  proxy and login validation, the browser's proxy setting as proxy rules
  (direct, fixed, script by address or as text, detection, system, and its
  bypass list), a script given as text served on the loopback address
  alone, which browser request headers a download repeats, the limits on
  what a browser request may carry, and whether a new link fits a partial file
  (size, ETag, unknown size, no partial file). Engine tests against local
  servers: a site that needs a login fails at once with "This site needs a
  login", a wrong password is refused, the right one downloads every range;
  a login for one host is never sent to the host a redirect leads to; and a
  download through an HTTP proxy with a login sends every request through
  it with `Proxy-Authorization`. Under Wine against the real daemon: a
  download from a site that needs a login fails with that message, succeeds
  after a login is saved and retried, and the password appears in neither
  the reply nor `settings.json`; a manual proxy with a login carries a
  download and gets the credentials, with the proxy password kept out of
  `settings.json`; a canceled download given a new link to the same file
  resumes into its file byte-identical, one given a link to a smaller file
  gets "The new link is for a 3.0 MB file; the partial download is of a 4.0
  MB file." and, told to start over, downloads the new file; waiting for a
  link from the browser is announced and can be canceled.
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
  Windows writes `"...\udm-gui.exe" --minimized` to the Run key and removes
  it again; the app id is registered; a scanner reporting a threat and one
  reporting clean show as such; **Pause all** stops a running and a queued
  download and **Resume all** finishes both, the running one in its own
  partial file. Under a virtual display, `--minimized` starts with no visible
  window; in dialog mode a finished download brought the hidden window
  forward with its completion dialog and scan result; **Open** on a file with
  a threat asked first; closing the window hid it while the process kept
  running, and the next finished download brought it back.
- Queues. Unit tests cover the calendar (round trips over ±5,000 years, leap
  days, invalid dates and times, weekdays), schedules (an overnight run open
  across midnight and closed at its stop time, same-day runs, runs without a
  stop time, the next start over a week and for a single date), queue
  validation, history loading (the main queue added, missing, repeated and
  finished ids dropped, waiting downloads kept in their queue) and the Queues
  dialog's fields. Under Wine against the real daemon: three downloads added
  with `start: false` wait in the main queue; one moves to another queue and
  another to the top; a second queue named the same in another case is
  refused; the main queue runs its two one at a time in the new order and
  stops when done, leaving the other queue's untouched; the main queue cannot
  be deleted; stopping a queue mid-download puts it back to waiting with 1 MB
  kept, and starting it again resumes the same file byte-identical; a queue
  with one retry runs a failing download twice and then ends; a "sleep when
  done" queue sends a 60-second countdown that `cancel_countdown` stops, and
  a once-only "hibernate" replaces it for one run, runs out and (dry run)
  hibernates while the queue keeps "sleep"; a schedule starts its queue within
  seconds and stops it at its stop time with progress kept, and does not
  start it again in the same window; a schedule whose window opened while the
  daemon was not running starts the queue as soon as the daemon starts; the
  queues survive a restart. In the window: the sidebar's queues, a queue's
  header and order, Down, the Queues dialog with a schedule and its next run,
  and the countdown bringing the minimized window forward, with Cancel
  reaching the daemon.
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
  download UDM cannot start goes back to the browser; the context menu works
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

Chrome itself (Google's build) was not tried: it ignores the
`--load-extension` flag. Playwright's Chromium loaded the extension and
launched the host, as described above.

Not verified: ffmpeg itself. The tests use a stand-in that records its
arguments; no real ffmpeg was run on real video.

---

[← Back to the help pages](README.md)
