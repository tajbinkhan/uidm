# Changelog

Each release's full notes are in [`release-notes/`](release-notes/), and the same text is on its
[GitHub Release](https://github.com/tajbinkhan/UDM/releases). This page lists them, newest
first. UDM was called Pullbay in version 0.4 and UIDM up to version 0.3.3.

## Unreleased

UDM downloads torrents and magnet links (#249), through librqbit on the engine's own runtime. A
`magnet:` link goes in like any address: Add URL, the clipboard monitor, `udm-cli get` (which also
takes a `.torrent` file) and `udm-mcp`; the browser extension hands over the magnet links you
click (in Safari too) and the `.torrent` files the browser downloads, which UDM fetches with the
page's cookies. A torrent's metadata comes from its peers for a magnet link, and the save-as
dialog's answer carries which of its files to download (changeable later); without the dialog it
downloads every file. A torrent is a download like any other: it counts toward the downloads that
run at once, waits in queues, follows the speed limit and the quota, pauses, and resumes after a
restart from resume data in UDM's data folder. Once finished it seeds, outside the download slots,
until a share ratio of 1.0 or an hour (the settings' `torrents.seed_limits`, or a torrent's own),
and torrents that were seeding seed again when UDM starts, once their files check complete (or
once their network adapter is up). With a network adapter chosen, the
torrents' listener, peer connections, DHT and trackers are bound to it on Linux and macOS (UPnP and
local discovery are off), and on Windows, where that cannot be done, torrents do not run; with DNS
over HTTPS no tracker or DHT name goes to the system's resolver. Torrents whose file names could
not be saved safely, with more than 10,000 files or a `.torrent` file over 16 MB are refused; a
taken folder or file name follows **If the file exists**, and the same torrent is never added
twice. On Windows, a torrent's files get the Mark of the Web. In the window, the save-as dialog
lists a torrent's files to tick (only the folder for a torrent of one file), the list says
**Seeding (ratio …)**, **Checking** or **Getting file list**, its menu has **Torrent details** and
**Stop seeding**, and a torrent's download window has tabs for its files (ticked or unticked while
it runs), peers, trackers, a piece map and its seeding limits, sent to the window row by row.
**Options > Torrents** sets the port, the DHT, UPnP, local peers, peers per torrent, the upload
limit, the default seeding limits and whether `.torrent` files from the browser are taken.
**Tasks > Open torrent file...** adds a `.torrent` file from the computer, `udm-gui` takes a magnet
link or a `.torrent` file alone on its command line (and a magnet link after `/d`), and the
clipboard monitor offers copied magnet links and `.torrent` addresses
([Torrents and magnet links](docs/torrents.md)).

Downloads can go through one network adapter only, such as a VPN's (#261). **Options > Network**
lists the adapters with their addresses; with one chosen, every connection is bound to it
(downloads, probes, stream segments, FTP's control and data connections, proxies, the update
check, tool installs, debrid accounts), and when it is down nothing connects: downloads stop with
"The network adapter … is down" instead of going out another way, and a running download stops
within two seconds of the adapter going. Changing the adapter or the resolver reaches queued and
running downloads at once (running ones go on from where they are; a form's download stops rather
than send the form again), a `utun3` or `tun0` that is another tunnel after a reconnect counts as
down until **Use this connection** in Options, and a new address on Windows binds again. yt-dlp
and gallery-dl get the adapter's address with `--source-address` and are not run while it is down.
Host names can be looked up with DNS over HTTPS at Cloudflare, Google, Quad9 or a server you name,
failing closed. A new **Country** column and the download window show the server's country (with
its flag, except on Windows), from an address table built into UDM, so nothing is looked up
online; it contains data from the NRO under CC BY 4.0, whose notice is in the new
`THIRD-PARTY-NOTICES.txt` that every package ships, and in **About UDM**
([Network adapter, encrypted DNS and the server's country](docs/network.md)). A manual SOCKS5
proxy is now handed on as `socks5h`, so yt-dlp and gallery-dl leave name lookups to it too.

The browser extension is ready for the Chrome Web Store and Firefox Add-ons (#248). Each release
carries `UDM-<version>-chromium-store.zip` and `UDM-<version>-firefox-store.zip`, made by
`installer/make-store-zips.js`, and a tag submits them to the stores the repository has keys for.
The extension has a [privacy policy](docs/privacy.md) that explains each permission. UDM's
native host can let in several Chromium extension IDs, so the store's copy and the one loaded
by hand both work once the store's ID is added ([Publishing the extension](docs/extension-stores.md)).
Until the listings are up, the extension is still loaded by hand.

Safari on macOS gets the extension too (#268): UDM.app carries it as a Safari Web Extension,
built from `extension/safari` (`node installer/make-safari.js`) when the disk image is made on a
Mac with Xcode. It reaches UDM through the app extension's native messaging, which runs
`udm-host safari`, and takes links to files as they are clicked, since Safari has no downloads
API. It was built for Safari but not yet run in it; [Known gaps](docs/known-gaps.md) lists what
Safari leaves out, and [Install](docs/install.md#safari) how to turn it on.

The README, the help pages and the project page show UDM (#269): screenshots of the main window,
Download File Info, the download window with its connections, the video button, Options, the
Scheduler and the window on Linux, each light and dark, in [`docs/images/`](docs/images/). Five
step-by-step guides in [`docs/guides/`](docs/guides/) cover a first download, catching browser
downloads, downloading overnight, a video from a page and resuming a broken download.
`node tools/screenshots/shoot.js` takes the screenshots again after a window changes, from the
preview page and the sample list the frontend's tests use, which now has two running downloads,
a queued one and an interrupted one. The download window no longer wraps **Resume capability**
in a font wider than Segoe UI, which pushed the address out of view on Linux, and the help pages
and UDM's own messages about ffmpeg, yt-dlp and gallery-dl send you to **Options > Tools**, where
the window has kept those programs' buttons since 0.5.0.

Links to file hosts such as Rapidgator and 1fichier can download through a Real-Debrid, AllDebrid
or Premiumize account (#259), an experimental option on the new **Options > Debrid** page. A link
to a host the service supports is unlocked into a fast direct link through the service's official
API each time the download starts, so a resumed download or a **New link** gets a fresh one when
the old one expires. If the service refuses a link, the download fails and says why instead of
falling back to the slow host. **Check account** shows whether the key works and until when the
account is premium. The API key is stored sealed like the proxies' passwords, together with the
service it is for, so it never goes to another service, and never in
`settings.json`, the window or the logs; [Debrid accounts](docs/debrid.md) says which links go to
the service.

## [0.5.0](release-notes/v0.5.0.md) - 2026-11-14

Minor. Pullbay is now called UDM, with a new icon and a new window drawn with Tauri and styled
like Windows 11. Adds a command-line tool, an MCP server for AI assistants, control from a phone,
a server mode, video sites through yt-dlp, image galleries, packages, FTPS and 13 more languages.

Update now installs only files the release's signed checksum list names (#267). Each release
carries `SHA256SUMS` and its minisign signature, `SHA256SUMS.minisig`, and UDM refuses an update
when the signature is missing, is not by UDM's release key, or the file's SHA-256 is not on the
list. Update now also works on macOS and Linux: it replaces `UDM.app` from the disk image, the
AppImage, or a `.tar.gz` copy installed with `install.sh`, and starts the new copy. A `.deb` or
`.rpm` copy says to update through the package manager.

## [0.4.0](release-notes/v0.4.0.md) - 2026-11-09

Minor. UIDM is now called Pullbay, with a new icon, and keeps your settings, list and
half-finished downloads. The background service's errors show in your language, and videos from
sites that check the player's headers download.

## [0.3.3](release-notes/v0.3.3.md) - 2026-11-08

Patch. Fixes two problems with the windows on Linux under Wayland.

## [0.3.2](release-notes/v0.3.2.md) - 2026-11-08

Patch. On Linux, closing the window leaves UIDM in the tray, as in IDM, and its icon shows in the
dock.

## [0.3.1](release-notes/v0.3.1.md) - 2026-11-08

Patch. Fixes problems found on Linux, a download that failed after fetching every byte, and test
copies that interfered with the installed copy.

## [0.3.0](release-notes/v0.3.0.md) - 2026-11-06

Minor. The tray and dialogs move closer to IDM's. Adds HTTP/2, NTLM logins, subtitles from
streams, and keeping the computer awake for downloads and schedules.

## [0.2.1](release-notes/v0.2.1.md) - 2026-11-04

Patch. Fixes a browser extension error from 0.2.0.

## [0.2.0](release-notes/v0.2.0.md) - 2026-11-04

Minor. A menu bar, toolbar, category tree and IDM's columns, gaps found by comparing with IDM
6.43 closed, and security holes found in a code review fixed.

## [0.1.0](release-notes/v0.1.0.md) - 2026-10-31

The first release. It catches downloads in Chrome, Edge and Firefox, fetches each file over
parallel connections, resumes after interruptions, and keeps downloading after the browser
closes, on Windows, macOS and Linux.
