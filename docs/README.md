# UDM: help pages

UDM is a free download manager for Windows, macOS and Linux. It works the way Internet
Download Manager users expect: a browser extension that hands downloads over, a list of every
download, categories, queues and a scheduler. Its downloads run in a background program, so they
keep going after the browser closes.

These pages were the README's manual. Each covers one part of UDM.

## Step by step

Short guides for the first things people do, most with a screenshot:

- **[Your first download](guides/first-download.md)**: adding an address, the save-as dialog,
  the download window and opening the file.
- **[Catch browser downloads](guides/catch-browser-downloads.md)**: connecting the extension, and
  why the browser sometimes keeps a download.
- **[Download overnight](guides/download-overnight.md)**: a queue that starts at night and turns
  the computer off when it is done.
- **[A video from a page](guides/video-from-a-page.md)**: ffmpeg and yt-dlp, the video button, and
  picking a quality.
- **[Resume a broken download](guides/resume-a-broken-download.md)**: carrying on after a crash, and
  giving an expired download a new link.

## Start here

- **[Install](install.md)**: Setup, the portable zip, the `.dmg`, the Linux packages, loading the
  browser extension (Chrome, Edge, Brave, Vivaldi, Opera and Firefox), ffmpeg and yt-dlp, updates,
  and moving over from Pullbay or UIDM.
- **[Portable mode](portable.md)**: running from a USB stick with everything in UDM's own
  folder.
- **[Run UDM on a server](server.md)**: the daemon's server mode, the Docker image and its
  Compose file and Unraid template, and adding downloads with `udm-cli`.
- **[Using UDM](using-udm.md)**: the window, the save-as dialog, categories, queues, the
  tray, the download window, the video button, adding many links and Options.
- **[Themes and interface size](themes.md)**: the built-in themes, high contrast, an interface
  up to 200%, and making a theme of your own.
- **[Keyboard shortcuts](keyboard-shortcuts.md)**: every key the window answers, and what a
  screen reader reads in the download list.
- **[Command line and automation](command-line.md)**: IDM's switches, the `udm-cli` tool,
  sending links from other programs, and the automation API for programs of your own.
- **[AI assistants (MCP)](ai-assistants.md)**: letting Claude Desktop, Claude Code, Cursor or VS
  Code list, add, pause, resume and remove downloads through `udm-mcp`.
- **[Remote control from a phone](remote-control.md)**: turning on the page phones on the home
  network use, pairing one with a QR code, removing it, the firewall, and how it is protected.

## Downloading

- **[What the browser extension takes](browser-capture.md)**: which downloads the extension hands
  to UDM, which it leaves with the browser, and the keys that change that.
- **[The extension's privacy policy](privacy.md)**: what it reads, that it goes only to UDM on the
  same computer, and why it asks for each permission.
- **[Streams, batches and the site grabber](streams-batches-and-grabber.md)**: HLS, DASH and HDS
  videos, YouTube and other video sites through yt-dlp, subtitles, numbered batches, and crawling
  a site for its files.
- **[Queues and the scheduler](queues-and-scheduler.md)**: running downloads a few at a time,
  starting and stopping them by the clock, and what happens when a queue is done.
- **[Proxies and logins](proxies-and-logins.md)**: the system proxy, manual and SOCKS proxies, PAC
  scripts, and site logins.
- **[Network adapter, encrypted DNS and the server's country](network.md)**: downloading only
  through a VPN's adapter, the kill switch when it goes down, DNS over HTTPS, and the country
  column.
- **[Torrents and magnet links](torrents.md)**: adding magnet links and `.torrent` files, choosing
  their files, seeding and its limits, the torrent settings, and what peers can see.
- **[Debrid accounts](debrid.md)** (experimental): fast downloads from file hosts such as
  Rapidgator and 1fichier through a Real-Debrid, AllDebrid or Premiumize account, and which links
  go to the service.
- **[Notifications and virus scans](notifications-and-virus-scans.md)**: what UDM tells you
  when a download ends, and scanning finished files.

## How it works

- **[How UDM fits together](architecture.md)**: the daemon, the host and the window, how the
  daemon checks its clients, and how the engine splits and resumes a file.
- **[Messages](protocol.md)**: every message the extension, the window and the daemon exchange.
  The stable part of them is the [automation API](command-line.md#the-automation-api), with its
  [JSON Schema](automation-api.schema.json).
- **[Building UDM](building.md)**: where the code lives, building and packaging it on each
  system, and adding a language.
- **[Publishing the extension](extension-stores.md)**: for maintainers, listing the extension in
  the Chrome Web Store and on Firefox Add-ons, and what changes once it is.
- **[Help translate](translating.md)**: the languages, which ones wait for review, and how to
  review or add one.

## Status

- **[Known gaps](known-gaps.md)**: what doesn't work yet, or works differently on macOS and Linux.
- **[Verified](verified.md)**: what each change was checked against.
- **[Benchmark](bench/README.md)**: the harness that times UDM against IDM and other download
  managers on servers that misbehave, and its results (none against IDM yet).
- **[Releases](https://github.com/tajbinkhan/UDM/releases)**: download the latest version.
- **[Problem reports](problem-reports.md)**: what UDM writes down when a program crashes or
  freezes, where it keeps it, and Help > Report a problem.
- **[Report a problem](https://github.com/tajbinkhan/UDM/issues)**: or ask a question.
