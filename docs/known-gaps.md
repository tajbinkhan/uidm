# Known gaps

- On macOS and Linux: the drop target and proxy configuration scripts
  (PAC and WPAD) are Windows-only; the system proxy comes from the
  `http_proxy`, `https_proxy`, `ftp_proxy`, `all_proxy` and `no_proxy`
  variables. On macOS closing to the tray minimizes the window rather than
  hiding it. On Linux the window closes and UDM stays in the tray; a
  caught download's save-as dialog or download window opens the main
  window with it.
  macOS notifications have no buttons. Passwords and saved requests are
  sealed with a key in UDM's data folder that only your user can read,
  rather than the Keychain or the Secret Service. **Update now** installs
  the disk image, the AppImage or the `.tar.gz`, but not a `.deb` or `.rpm`,
  which the package manager updates. Microsoft Defender is Windows' own:
  choose a scanner program such as `clamscan`. These builds were tested on
  Linux; the macOS build is checked by the compiler and CI only.

- Debrid accounts (#259, [Debrid accounts](debrid.md)) are experimental:
  they were tested against stand-ins for Real-Debrid's, AllDebrid's and
  Premiumize's APIs, not the services themselves. A link AllDebrid has to
  prepare first ("delayed") fails with the service's note instead of
  waiting; retry it a few minutes later. Premiumize says why it refused a
  link in words only, so UDM reads the words.

- Safari (#268, [install](install.md#safari)): the extension was built for
  Safari but never run in it here; the Swift handler, the entitlements and
  the Xcode build are checked by CI's macOS runner only (and do not fail a
  run until that build has passed once), and the extension's side against a
  stand-in for Safari's API in its tests. What Safari does not offer, the
  Safari extension does not do:
  - No downloads API: a download the browser starts (a form's, a script's,
    a link that names no file) stays in Safari. UDM takes only a link whose
    address or `download` attribute names a file of a type in the captured
    types list, as it is clicked, and none while that list is empty; a type
    that shows only in the server's answer, the minimum size and the popup's
    list of the browser's own downloads do not apply. A link UDM declines at
    once is followed by Safari as usual; one UDM refuses later, or that was
    lost when the connection to UDM dropped, opens in a background tab for
    Safari to download.
  - No proxy settings: **Use the browser's proxy** gets nothing from
    Safari, and UDM uses its own proxy settings.
  - No notifications: the notices that UDM could not take a download or a
    page has no links do not show; the popup shows why UDM is not reached.
  - No live port: the extension keeps a request out to UDM's app extension,
    which answers it as soon as UDM has news (or after 20 seconds without),
    then asks again. Safari may stop the worker or the app extension when
    they are idle; the connection then starts again, with a fresh
    `udm-host`, the next time the worker runs, and the popup may show UDM as
    not connected for a moment. A crawl that asks the browser for cookies
    or a rendered page while the worker is stopped gets none.
  - The app extension runs `udm-host` in Safari's sandbox, where it cannot
    start the daemon: open UDM (or let it start at sign-in) before using
    Safari. It reaches the daemon only in the usual data folder, not a
    named copy's (`UDM_INSTANCE`), a portable copy's or a server's.
  - UDM is signed ad hoc, so Safari shows the extension only once unsigned
    extensions are allowed (**Develop > Allow Unsigned Extensions**, or in
    Safari 17 and later also **Settings > Developer > Allow unsigned
    extensions**), which Safari turns off each time it quits.
  - Safari may give a Manifest V3 worker no web requests to watch: then
    the video button lists only what its player's own addresses show, the
    recent videos menus stay empty, and form downloads are not rebuilt.
- FTPS data connections do not resume the control connection's TLS
  session (the operating system's TLS, through `native-tls`, offers no way
  to), so a server that requires it, such as vsftpd with
  `require_ssl_reuse` on, may refuse the transfers. An `ftp://` server is
  switched to TLS only when it refuses a plain login and asks for TLS, not
  whenever it offers `AUTH TLS`.
- Remote control from a phone (`docs/remote-control.md`): listening on
  every interface covers IPv4 only; name an IPv6 address under *Only these
  addresses* to listen on it. The page has no live push: it asks for the list
  every 2 seconds. It adds links into the folder the settings choose, with
  the server's name, and cannot pick a queue, a folder or a name, or pause
  and resume everything at once. Its text is English, German, Spanish or
  French; the daemon's own refusals are English. The page was not tried on
  a real iPhone or Android phone, and the certificate was served by OpenSSL
  only (SChannel and Security.framework were built, not run).
- Languages (#251, `docs/translating.md`): no right-to-left language (Arabic,
  Hebrew, Persian, Urdu) and no script that needs shaping (Hindi, Bengali,
  Marathi, Tamil, Telugu, Thai) is offered yet. The window's webview shapes
  text and lays it out right to left, so they can be added now; each needs a
  translated table, `Language::right_to_left` where it applies, and every
  window checked in that direction. Some strings still show in English in
  every language (`crates/ui/src/lang/untranslated.json`), most of them the
  IDM wording the window took on. Also missing: a language picker on first start (UDM follows the
  system's display language, and **View > Language** changes it), Setup's own
  languages (the installer is English), the phone page's languages beyond the
  first four, and plural forms (where a language needs them, its table words
  a count as "Downloads: 3"). The 13 newest tables were made by machine and have not been
  read by a native speaker. Chinese, Japanese, Korean and Vietnamese need a
  font of the system's (they show as boxes without one, and their names in
  the menu are in English until it is read); the macOS font files are
  guessed from macOS's usual fonts, not tried.
- The history is written about a second after each change, so a crash can
  lose the last second of progress; the sidecar still has it, so resuming is
  unaffected.
- Under the rename setting, the save-as dialog shows the proposed name, not
  the `name (1)` the engine may pick if that name is taken when the download
  starts.
- Starting a download from the browser popup while it waits for the save-as
  dialog answers no question: under the ask setting a taken name gets a new
  name, and an address already in the list is added with a new name.
- A copy added with a new name while the old download has a matching
  partial file resumes that partial file instead of starting a new one, so
  both entries point at it.
- Adding a category does not add its types to the captured types; that list
  is edited on its own.
- A schedule only works while the daemon runs. It stays up for a schedule
  once started, but after a reboot something has to start it: turn on Start
  with Windows, or open the window or the browser.
- A queue run started by hand with nothing in it ends at once, and a run that
  is only waiting on failed downloads ends too: failed downloads count as
  finished unless the queue retries them.
- Packages (`#258`): only the link picker keeps links as packages; the
  batch dialog, imports and the grabber do not, and a download cannot be
  moved into or out of a package afterwards (only the whole package can be
  ungrouped). The list itself is not grouped: a package's files are listed
  together when its node under **Packages** is picked. Unpacking a package
  again, after a password too, unpacks every archive in it again (taken
  names get numbers). The password dialog asks once each time a package comes to need
  one while the window is open; a package that needed it before the window
  connected is unpacked from its menu. 7-Zip was not run here: the tests use
  a stand-in that records its arguments and what it read on its standard
  input, and real RAR, 7z and spanned-zip archives were not tried. Unpacked
  files get the time they were written, not the archive's dates, and no
  permissions from the archive. bzip2 and `.tar.bz2` need 7-Zip.
- A form download that sends a file stays in the browser (the browser does
  not share the file), and a POST download cannot resume: a failure means
  sending the form again with Retry.
- Chrome does not let an extension cancel a download at its headers, so
  there it is still taken from the browser as it starts; Firefox cancels it
  at the headers.
- Streams: live streams, DASH manifests split into several periods (often
  ads), and HLS `SAMPLE-AES` without a key system are refused. The size
  shown for a quality is an estimate from its bit rate. Without ffmpeg,
  separate video and audio stay two files.
- Video pages: UDM does not give yt-dlp the browser's cookies, so videos
  that need signing in (private or members-only ones, most of Facebook and
  Instagram) are not offered. yt-dlp's automatic captions are not offered,
  only a page's own subtitles. A page on another site is tried with yt-dlp
  only when the browser saw no stream in its tab and only when the video
  button's menu opens; a page without any player gets no button, so its
  video can only be added by its address. A playlist's videos are added as
  one batch with one quality for all; the same video added twice is not
  asked about as a duplicate (video pages, like streams, are never
  duplicates), only a taken file name is. A pasted playlist address in Add
  URL is downloaded as a page, not listed.
- Installing yt-dlp and ffmpeg: UDM checks each download against the
  SHA-256 published in the same GitHub release, not against yt-dlp's GPG
  signature. There is no ffmpeg to install on macOS (no build there
  publishes a checksum), so Options names Homebrew. Unpacking ffmpeg needs
  the system's `tar` (Windows 10 version 1803 and later have it). A yt-dlp
  that is running cannot be replaced on Windows, so an update waits for the
  next day's look. Only a yt-dlp UDM installed is kept up to date.
- Image galleries: UDM does not give gallery-dl the browser's cookies or
  the user's gallery-dl configuration, so galleries that need signing in
  are not listed. At most 1000 pictures of a gallery are listed. There is
  no gallery-dl to install on macOS or on ARM processors (the project builds
  no program there), and an installed gallery-dl is not kept up to date on
  its own. The tests that run a stand-in gallery-dl are shell scripts, so
  they run on macOS and Linux only.
- The video button sits outside a video in full screen, so it is hidden
  then, and it only knows the media its tab loaded after the extension
  started; reloading the page fills the list.
- The site grabber reads pages as the server sends them unless **Find links
  that page scripts add** is on. That option needs the browser extension: it
  opens each page in a background tab, which is slower and shows the tabs
  briefly. Without the browser connected, pages are read as sent.
- Clipboard watching runs in the window, so it stops when the window is
  closed with **Exit**.
- Every client sees every download, not only the ones it started.
- A process that squats on the port cannot learn anything, but it does stop
  downloads from starting until it goes away. A per-user named pipe would close
  that gap.
- Other processes running as the same user can read the token. That is outside
  this design's threat model: such a process can already read the browser's
  cookies directly.

---

[← Back to the help pages](README.md)
