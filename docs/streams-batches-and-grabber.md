# Streams, batches and the site grabber

**Seeing videos.** The extension's service worker reads the headers of every
response a tab gets (`webRequest.onHeadersReceived`) and keeps, per tab, the
direct video and audio files (a `video/` or `audio/` type, or a media file
name with a generic type; anything under `video_min_size_kb`, 128 KiB by
default, is left out) and the HLS, DASH and HDS playlists, of the types in
`video_types` (`m3u8`, `mpd` and `f4m` for the streams; a file whose type neither
its name nor its MIME type says is kept). A settings file from before HDS
support gets `f4m` added once, recorded in `video_types_hds`. Segments (`.ts`, `.m4s`, `video/mp2t`,
HDS fragments' `video/f4f`) are left out, and
the list starts over when the tab loads a new page, or when a single-page
site moves to another (`webNavigation.onHistoryStateUpdated`; what came in
the second before stays, and the tab's buttons are reset). A content script
(`content.js`) shows **Download this video** over a playing `<video>` or
`<audio>`, or a media `<object>`/`<embed>` the pointer is over, in a closed
shadow root, so the page's scripts and styles cannot reach it, and shows
page and server text with `textContent` only. It tells the worker when each
player starts loading or playing (`video:started`, with a number per
player). When clicked it asks the worker for its tab's list for that player
(`video:menu` with the player's number and its `currentSrc`, `src` and
`<source>` addresses): the files and streams at those addresses, and the
playlists its frame asked for while it was the player that started last
(or that started within 1.5 s after), which tells players fed through
`blob:` addresses apart; with nothing of its own, the whole list. The worker
asks UDM (`probe_media`) what each stream offers. The button hides when, at
five points of the player, what is on top lies outside the player's box
(its furthest ancestor not much bigger than it, so a custom player's
controls and overlays do not count), when `document.hidden`, and when
something is full screen; it looks again on scroll, resize and once a
second. Tabs open before the extension was installed, updated or turned
back on get the content script injected (`scripting.executeScript`, every
frame that lacks it) on `runtime.onInstalled` and on the first connection
to UDM in a browser session. A content script can only pick from what the worker saw
in its tab, by number, never pass in an address. A video element that raised
an `encrypted` event or holds `MediaKeys` is DRM protected: the menu says so
and offers nothing, or with `video_protected_note` off the button does not
show on it. `video_view` (`full` or `mini`) sets how the button and the
selected-links button look; the mini one is a round arrow with its text as
a tooltip. Playlists that are themselves one quality of a listed
master playlist are folded into it.

**YouTube, Facebook, Instagram and Vimeo.** YouTube's player streams over
SABR: POST requests to `googlevideo.com/videoplayback` whose bodies hold
video and sound in YouTube's own framing (`application/vnd.yt-ump`), so no
response is a file, a playlist or a manifest; Facebook and Instagram fetch
signed pieces, and Vimeo reads its streams from its player's configuration.
For these sites the worker keeps the tab's video page instead
(`kind: "page"`, one address per video):

| Site | Pages | Kept as |
|---|---|---|
| YouTube | `/watch?v=ID`, `/shorts/ID`, `/live/ID`, `youtu.be/ID` | `https://www.youtube.com/watch?v=ID` |
| Facebook | `facebook.com/<page>/videos/ID`, `facebook.com/watch?v=ID`, `facebook.com/reel/ID`, `fb.watch/CODE` | `https://www.facebook.com/watch/?v=ID`, `.../reel/ID`, `https://fb.watch/CODE/` |
| Instagram | `instagram.com/p/CODE`, `/reel/CODE`, `/reels/CODE`, `/tv/CODE` | `https://www.instagram.com/p/CODE/` (or `reel/`, `tv/`) |
| Vimeo | `vimeo.com/ID` (with an unlisted video's hash), `vimeo.com/channels/<name>/ID`, `player.vimeo.com/video/ID` | `https://vimeo.com/ID`, the player's address with its `h=` hash |

The page is taken from the tab's address as these sites move between videos
without loading a page (`tabs.onUpdated` and
`webNavigation.onHistoryStateUpdated`). The daemon sends these addresses to
yt-dlp (`ytdlp::supports`), and any other web page the extension marks as
one (`media: {"kind": "page"}`); everything else goes to the stream reader.
`probe_media` on such a page runs `yt-dlp -J --flat-playlist` (given up
after 55 seconds, just under the extension's 60) and offers one video per
height (H.264 first, then the highest bit rate), the sound per language,
and the page's own subtitle tracks (not the automatic ones). The download
runs yt-dlp with the chosen formats and `--ffmpeg-location`, so ffmpeg joins
them into an `.mp4`; the sound alone is `-x --audio-format
mp3|m4a|opus|flac` (Opus keeps a site's Opus sound as it is). Ticked
subtitles are fetched with `--write-subs --sub-langs ^LANG$` (only ids made
of letters, digits, `-` and `_`) and, with ffmpeg, put in the video
(`--embed-subs`); without it they are saved beside the video under its name
(`Video.en.vtt`). UDM reads yt-dlp's progress lines, and canceling ends
yt-dlp and its children (`taskkill /T` on Windows); its `.part` files let a
retry carry on. Without ffmpeg the best format with both video and sound in
one file is saved, with a note. Live streams are refused. UDM does not
send the browser's cookies to yt-dlp, so only videos anyone can watch signed
out are offered (on Facebook and Instagram that is public posts only).

**Any other site.** When the video button's menu opens on a page that is
none of the sites above and the browser saw no file or stream in that tab,
the worker asks UDM to read the page (the frame that holds the player)
with yt-dlp. This runs only when the menu is opened, never on a page load,
so no yt-dlp starts for pages nobody asked about; one probe per address is
in flight at a time, a find is kept for the tab, and a page yt-dlp found
nothing on is not asked about again for ten minutes. The menu shows the page
("Video on this page") only when yt-dlp found a video or a playlist.

**Playlists and channels.** A page yt-dlp reads as a playlist (a YouTube
playlist or channel, or a page with several videos) answers with its
videos instead of qualities (`playlist`: its title and each video's page,
title and length, at most 1000, only http and https pages, without
repeats). On a YouTube video that is part of a playlist (`list=` in its
address, not a personal mix), the menu also asks about the playlist's own
page. The menu's "Choose videos in UDM..." sends the list to the window
(`pick_videos`), which opens the playlist dialog: every video ticked, a
filter by title, one quality for all (best, up to 1080p, 720p, 480p or
360p, or the sound alone as MP3, M4A, Opus or FLAC) and the queue they
wait in. **Add** adds the ticked videos as one batch, which asks the batch
questions (a name already taken in its folder) like the link picker's.
Each video is read by yt-dlp when its download starts, with the quality as
a height limit (`bestvideo*[height<=720]+bestaudio/...`).

**Image galleries.** gallery-dl lists the pictures of a gallery page (a
Wikimedia Commons category, a Flickr album, a DeviantArt gallery and the
other sites gallery-dl knows), and UDM's own engine downloads them. The
browser's menu has **Download gallery with UDM** on a page, a frame or
an image (`pick_gallery`, for the tab's http or https page), and Add URL has
**Download gallery** for the address typed. Either opens the gallery
dialog, which asks the daemon to read the page (`read_gallery`): nothing
runs until one of these clicks. The daemon runs `gallery-dl --config-ignore
--no-input --resolve-json -o output.jsonl=true --range 1-1000 --http-timeout
20 -- PAGE`, with the browser's user agent and the settings' proxy, and gives
it up after 55 seconds, keeping what it listed by then. At most two pages
are read at once, the same page once at a time, and a page gallery-dl does
not know or found nothing on is not read again for ten minutes. The dialog
lists each picture (http and https addresses only, each once, at most 1000)
by the name gallery-dl gives it, with its size in pixels when known; every
picture starts ticked, and a filter keeps those whose name holds every word
typed. The pictures are saved in one folder, by default a folder named
after the gallery in the General category's folder, and wait in a queue.
**Add** adds the ticked ones as one batch with the page as their referrer,
which asks the batch questions like the link picker's; two pictures with
the same name are numbered apart (`a.jpg`, `a (1).jpg`). When gallery-dl is
missing, the dialog offers **Install gallery-dl** and **Retry**.

**yt-dlp, ffmpeg and gallery-dl.** Options > Tools has **Install yt-dlp**,
**Install ffmpeg** and **Install gallery-dl** (or **Update**) buttons, which the daemon carries out
(`install_tool`, answered with `tools`). Nothing is downloaded without a
click, except that **Keep yt-dlp up to date** (off unless turned on) looks
once a day for a newer release of a yt-dlp UDM installed. What is
downloaded, from where:

| Tool | System | From | Checked against |
|---|---|---|---|
| yt-dlp | Windows (x64, ARM64, x86) | `github.com/yt-dlp/yt-dlp/releases/latest`: `yt-dlp.exe`, `yt-dlp_arm64.exe`, `yt-dlp_x86.exe` | `SHA2-256SUMS` in the same release |
| yt-dlp | macOS | the same release: `yt-dlp_macos` | `SHA2-256SUMS` |
| yt-dlp | Linux (x64, ARM64) | the same release: `yt-dlp_linux`, `yt-dlp_linux_aarch64` | `SHA2-256SUMS` |
| ffmpeg | Windows (x64, ARM64, x86) | `github.com/yt-dlp/FFmpeg-Builds/releases/latest`: `ffmpeg-master-latest-win64-gpl.zip` (or `winarm64`, `win32`) | `checksums.sha256` in the same release |
| ffmpeg | Linux (x64, ARM64) | the same release: `ffmpeg-master-latest-linux64-gpl.tar.xz` (or `linuxarm64`) | `checksums.sha256` |
| ffmpeg | macOS | nothing: no build there publishes a checksum, so Options shows `brew install ffmpeg` instead | |
| gallery-dl | Windows (x64, x86) | `codeberg.org/mikf/gallery-dl/releases/latest`: `gallery-dl.exe`, `gallery-dl_x86.exe` | `SHA256SUMS` in the same release |
| gallery-dl | Linux (x64) | the same release: `gallery-dl.bin` | `SHA256SUMS` |
| gallery-dl | macOS, and ARM processors | nothing: the project builds no program there, so Options names a package manager's command instead | |

The release is read from GitHub's API (Codeberg's, which answers in the
same shape, for gallery-dl, whose development moved there in 2026), and
only files served from that repository's releases on `github.com` or
`codeberg.org` are used. The file is downloaded to
`tools/.downloads` under a name nothing runs, its SHA-256 compared with the
one the release's checksum file lists (and with GitHub's own digest of the
file, when the API gives one), and only a match is put in place and made
executable, in the `tools` folder of UDM's data folder (the copy's
`Data` folder when it is portable). A mismatch is deleted and nothing is
installed; a release without a listed checksum is not downloaded at all.
ffmpeg's archive is unpacked with the system's `tar` (System32's on Windows
10 and later), keeping only `ffmpeg` and `ffprobe`. `tools/tools.json`
records each tool's release and last check. Both checksums come from the
same release as the file, so they catch a damaged or swapped
download, not a release the project's own account published; yt-dlp's GPG
signature (`SHA2-256SUMS.sig`) is not checked. UDM uses a tool from its
own folder first, then the one set in Options (ffmpeg), then `PATH`.

**HLS.** UDM reads master playlists (qualities, separate audio tracks) and
media playlists (segments, `#EXT-X-BYTERANGE`, `#EXT-X-MAP` headers,
`#EXT-X-MEDIA-SEQUENCE`). Segments encrypted with AES-128 are decrypted with
the key from the playlist's key URL, fetched once, and the IV from the
playlist or the segment's sequence number. Any other key system
(`SAMPLE-AES`, or a `KEYFORMAT` other than `identity`, as FairPlay and
Widevine use) is DRM and refused, as is a live playlist without
`#EXT-X-ENDLIST`. Subtitle tracks (`#EXT-X-MEDIA:TYPE=SUBTITLES`) in the
quality's `SUBTITLES` group are offered by name and language.

**DASH.** UDM reads a static manifest's `SegmentTemplate` (with a fixed
duration or a `SegmentTimeline`, `$Number$`, `$Time$`, `$Bandwidth$`,
`$RepresentationID$` and `%0Nd` widths), `SegmentList` (with byte ranges)
and `SegmentBase` or a plain `BaseURL` (the whole file), inheriting them
from the period and adaptation set, and resolving `BaseURL`s at every level.
It picks the best video and audio, or the ones chosen. `ContentProtection`
marks DRM, refused; live (`dynamic`) manifests and manifests split into
several periods are refused too. WebVTT subtitle sets (`text/vtt`) are
offered by their `Label` or language; one with the `main` role is the
default. TTML and WebVTT inside MP4 segments are left out.

**HDS.** Adobe HTTP Dynamic Streaming (`.f4m`, `application/f4m+xml`, or XML
whose root is `<manifest>` in the `ns.adobe.com/f4m` namespace). UDM reads
F4M 1.0 manifests (`<media url bitrate width height bootstrapInfoId>`, each
offered as a quality, `<bootstrapInfo>` inline in base64 or at a `url`,
`<baseURL>`, `<metadata>`, `<duration>`) and F4M 2.0 manifests whose
`<media href>` points at another manifest. The bootstrap's `abst` box gives
the segment run table (`asrt`) and fragment run table (`afrt`), from which
every fragment's address is made: `<baseURL>/<media url>Seg<s>-Frag<n>`. The
manifest's own query string (Akamai's `hdnea`/`pvtoken` signature) and any
`<pv-2.0>` token go on every fragment, bootstrap and child manifest. Each
F4F fragment's `mdat` box holds FLV tags; the joined video is an FLV header,
the media's metadata as an `onMetaData` tag, then every fragment's tags.
`<drmAdditionalHeader>` marks DRM and is refused, as are live streams
(`<streamType>live</streamType>` or the bootstrap's live flag).

**Downloading.** Segments go one file each into `<file>.udmparts` next to
the output, written as `.tmp` and renamed once complete, up to **Connections
per file** at once, through the shared speed limit, each retried with the
usual backoff. Progress counts segments and bytes, and the window's bar
follows the segments. Canceling keeps the finished segments; a retry fetches
only the missing ones. Then each track is joined in order. With ffmpeg,
`ffmpeg -i video [-i audio] -c copy` makes one `.mp4` (or `.webm`) without
re-encoding; without it, an HLS video is saved as its `.ts` (or `.mp4` for
fragmented MP4 segments), an HDS video as its `.flv`, and a stream with
separate audio as two files, and
the entry carries a note saying so.

**Subtitles.** The video button's menu lists a stream's subtitle languages
with a checkbox each, the stream's default ones ticked; the ticked ones go
with whichever quality is clicked (not with the sound alone), as IDM keeps
them. Their WebVTT segments are fetched with the video's and joined into one
file, each segment's own header (and HLS's `X-TIMESTAMP-MAP`) dropped. With
ffmpeg they go into the video as subtitle tracks (`mov_text` in an MP4,
WebVTT in a WebM) with a three-letter language tag and the track's name;
without it each is saved beside the video as `<name>.<language>.vtt`. A
track that is not WebVTT text is skipped without failing the download. ffmpeg runs without a console window and
its error, if any, becomes the download's error. Streams land in the Video
category's folder whatever their extension.

**Batches and imports** go through `add_many`, which adds every item waiting
in one queue and announces the queue once, so 10,000 downloads do not send
10,000 queue updates. An item with a full-path `folder` is saved there under
its own name.

**Packages** come from the link picker's `add_many`: all its items as one
package, or each split archive among them as its own. The daemon keeps them
in `history.json` (an older file has none and still loads) and unpacks one
on a thread of its own, when asked or, with Options' **Unpack a package as
soon as all its files are in**, when its last download finishes and no try
was made yet. The archive rules are in `udm_domain::package`; zip, tar,
gzip and xz are unpacked in-process (`udm_infrastructure::unpack`, with
the pure-Rust `zip`, `tar`, `flate2` and `lzma-rust2` crates), and the rest
go to 7-Zip, which first lists the archive (`7z l -slt`) so a path that would
leave the folder, a link, too many files or too little room is refused before
it runs, then extracts it (`7z x -aou`) into a hidden staging folder whose
files are checked again before they move.

**The site grabber** runs in the daemon, so it uses the proxy settings, and
reports to the window that started it. It reads `robots.txt` once per site
(the group for `UDM` if there is one, else `*`; longest match wins, with `*`
and `$`), waits at least the set time between requests, and caps a site's
`Crawl-delay` at 30 seconds. Pages are parsed with html5ever, the HTML parser
from Servo, so markup inside scripts or comments is not mistaken for links.
It follows `<a>`, `<area>` and frames, keeps files linked from those and from
`<img>`, `<source>`, `<video>`, `<audio>`, `<embed>` and `<object>`, honors
`<base href>`, and only ever follows or lists `http` and `https` addresses.
It reads at most 5 MiB of any page, follows only page-like links (no
extension, or `.html`, `.php` and the like), and lists files without
fetching them. Closing the dialog or **Stop** ends a crawl within a tenth of
a second.

Pages read at once are fetched on their own threads, one batch at a time,
with the usual wait between batches; robots.txt is read before a site's
first batch. With `*` in the types, every link with a file name that is not
a page counts as a file. A project's login is stored in Credential Manager
as `UDM/grabber/<start host>/<user name>`, so projects on one site can use
different accounts (a password an older version kept under
`UDM/grabber/<start host>` is still read), sent to the start site when it asks, and
given to the downloads on that site. A login page's cookies come from the
extension's `cookies_wanted` request at crawl start, as the start site's do
with **Use my browser's sign-in**, or else from the pasted ones, which
project files keep encrypted for the user (as saved requests are) and
templates never keep. Each site's cookies go only to pages on that site's
host (and its subdomains when the crawl explores them), never to other
hosts the crawl reads; the downloads it adds for files on the login site
carry that site's cookies too, as captured browser downloads do, and
files on other hosts get none. Named filters are kept in
`%APPDATA%\UDM\grabber\filters.json` and named templates in
`grabber\templates\`. With scripts run, the extension closes every window
the page opens and reports its address; the crawl explores those only when
**Ignore the windows page scripts open** is off. A scheduled run sets the
project's when-done action, file and force-close choice on its queue for
that run only (`when_done_once`, `open_when_done_once`, `force_close_once`),
so the queue's own settings come back afterwards; a run that adds nothing
does them at once. With a stop time it also sets `stop_at_once`, and the
scheduler stops that queue run (its downloads wait again, progress kept)
once the time passes.

---

[← Back to the help pages](README.md)
