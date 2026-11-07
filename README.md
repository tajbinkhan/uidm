# UIDM

A download manager for Windows, macOS and Linux, rebuilt from scratch in
Rust. It catches downloads in Chrome, Edge, Firefox and other browsers, fetches each file over parallel connections,
resumes after interruptions, keeps downloading after the browser closes, and
shows everything in a desktop window. It also downloads streamed videos (HLS,
DASH and HDS) from a button over the video, adds many files at once from a
pattern, a list or a site crawl, and watches the clipboard for links.

## Install

Every release on the
[Releases page](https://github.com/tajbinkhan/uidm/releases) has a file for
each system:

| System | File | |
|---|---|---|
| Windows 10 and 11 | `UIDM-Setup-<version>.exe` | the installer, below |
| | `UIDM-<version>-windows-portable.zip` | the programs and `install-host.ps1` |
| macOS 11 or later | `UIDM-<version>-macos.dmg` | Apple Silicon and Intel |
| Ubuntu, Debian, Mint | `uidm_<version>_amd64.deb` | `sudo apt install ./uidm_*.deb` |
| Fedora, openSUSE, RHEL | `uidm-<version>-1.x86_64.rpm` | `sudo dnf install ./uidm-*.rpm` |
| Any Linux | `UIDM-<version>-x86_64.AppImage` | make it executable and run it |
| | `uidm-<version>-linux-x86_64.tar.gz` | unpack and run `./install.sh` |

The Linux files need glibc 2.35 and OpenSSL 3 (Ubuntu 22.04, Debian 12,
Fedora 36 or newer) and GTK 3. The tray icon needs
`libayatana-appindicator3` (and on GNOME the AppIndicator extension);
notifications use `notify-send`. [Build](#build) says how the files are made.

### Windows

Run `UIDM-Setup-<version>.exe`. It installs for your user only, without administrator rights, into
`%LOCALAPPDATA%\Programs\UIDM`. It registers UIDM with Chrome, Edge and
Firefox, adds
**UIDM** to the Start menu (and, if you tick it, the desktop), and lists UIDM
under **Installed apps**. Setup is not code-signed, so Windows SmartScreen
may warn before it runs.

To install from a source checkout instead, with the build toolchain described
under [Build](#build), run this from this folder:

```powershell
.\install-host.ps1
```

That builds the three programs if needed, registers UIDM with Chrome, Edge and
Firefox, makes the Firefox copy of the extension, and adds **UIDM** to the
Start menu.

Either way, load the extension once:

1. Open `chrome://extensions` (or `edge://extensions`) and turn on
   **Developer mode**.
2. Click **Load unpacked** and choose the `extension` folder: the one inside
   the install folder (Setup's last page shows it and can open it), or the
   one in this checkout.
3. Check that its ID reads `epmjakacaejhdhldkpbjnakhfmjjbhfm`. The manifest
   carries a fixed public key, so the ID is the same on every machine.

After updating, reload the extension on the extensions page. It asks for
`webRequest` (to send downloads started by a form as the same POST, to send
UIDM the browser's whole request, and to notice the videos a page loads),
`webNavigation` (to find the frame a link in an `about:blank` frame belongs
to), `downloads.ui` (to keep the download bubble hidden for a download UIDM
takes) and `proxy` (to read the browser's proxy setting, which it never
changes), in addition to native messaging, downloads, cookies, context
menus, storage, scripting and notifications. A small
content script runs on every page for the video button; it does nothing
until a video plays.

For streamed videos to end up as one `.mp4`, install
[ffmpeg](https://ffmpeg.org/) and put `ffmpeg.exe` on `PATH`, or choose it in
**Options > General**. Without it UIDM still downloads streams, as `.ts`
files or as separate video and audio files.

Videos on YouTube, Facebook, Instagram and Vimeo pages need
[yt-dlp](https://github.com/yt-dlp/yt-dlp) as well:
`winget install yt-dlp.yt-dlp` installs it with ffmpeg and Deno, which
yt-dlp uses to read YouTube's player. Setup offers to run that command for
you when winget is there and yt-dlp is not. UIDM finds `yt-dlp.exe` on
`PATH`, next to UIDM's programs or next to ffmpeg. Restart UIDM after
installing it yourself, so it sees the new `PATH`.

To remove UIDM, uninstall it from **Installed apps**. That closes UIDM if it
is running, removes the program, its browser registration, shortcuts, the
**Start with Windows** entry and the notification registration, and asks
whether to remove your settings and download list too. Installing a newer
setup over an older one closes UIDM first and keeps your settings. With the
script, `.\install-host.ps1 -Uninstall` removes what it added. Downloaded
files are left alone either way.

**Updates.** UIDM looks for a newer release on GitHub once a week (Options >
General can turn that off), and **About** (which also shows the version,
the copyright, and a support link to the issue tracker) checks on demand. When a newer
release has a Setup attached, **Update now** (in About, and next to the
notice in the status bar) has the daemon download `UIDM-Setup-<version>.exe`
through your proxy settings, only from this repository's release downloads.
It checks the file's size, that it is a Windows program, and its SHA-256
when GitHub lists one, saves it to the temp folder and runs it with
`/fromuidm=yes`. UIDM then stops its downloads, saves its list and quits;
Setup closes whatever is left without asking again and replaces the files.
Stopped downloads resume from the list as usual. The notice's **×** hides it
for that version (remembered in `window.json`); a newer release shows it
again.

### macOS

Open the disk image and drag **UIDM** to **Applications**. UIDM is not
signed with an Apple Developer ID, so the first time, right-click it in
Applications, choose **Open**, then **Open** again (or allow it under
**System Settings > Privacy & Security**). The extension folders are inside
the app: in Finder, right-click UIDM, **Show Package Contents**, then
`Contents/Resources/extension` (Chrome, Edge, Brave) or
`Contents/Resources/firefox`. In Chrome's **Load unpacked** dialog,
Cmd+Shift+G and `/Applications/UIDM.app/Contents/Resources/extension` gets
there directly.

### Linux

Install the `.deb` or `.rpm` with your package manager, or run the
AppImage, or unpack the `.tar.gz` and run `./install.sh` (it installs into
`~/.local` for your user; `./install.sh --remove` takes it away). UIDM is
then among your applications, and `uidm` starts it from a terminal. The
extension folders are `/usr/lib/uidm/extension` and `/usr/lib/uidm/firefox`
for the packages, `~/.local/lib/uidm/...` for the archive, and inside the
AppImage (`./UIDM-*.AppImage --appimage-extract` unpacks them).

### Browsers on macOS and Linux

There is no Setup to register UIDM with the browsers, so the first time UIDM
runs it registers itself with every browser it finds: Chrome, Edge,
Firefox, Brave, Chromium and Vivaldi (Opera reads Chrome's). Each browser
reads `com.uidm.host.json` from its own `NativeMessagingHosts` folder, such
as `~/.config/google-chrome/NativeMessagingHosts` or
`~/Library/Application Support/Google/Chrome/NativeMessagingHosts`.
**Options > Browser capture** lists them to add or remove one later. When an
AppImage moves, UIDM points the browsers at its new place the next time it
runs.

Then load the extension as described for Windows above, from the folder for
your system.

### Firefox

Firefox 128 or later loads the `firefox` folder, a copy of the extension with
its own manifest and the ID `uidm@uidm.local`. Firefox runs background scripts
where Chrome runs a service worker, and Chrome warns about Firefox's keys, so
the two cannot share one `manifest.json`. Setup and the macOS and Linux
packages install the `firefox` folder next to `extension`. In a checkout it
is made from `extension`, not kept in git, so a `firefox` folder made before
you updated or changed the extension lacks those changes: `install-host.ps1`
makes it again each time it runs, and so does
`.\installer\make-firefox.ps1 -Out firefox` (or
`node installer/make-firefox.js firefox`). Run one of them after every
update, then reload the add-on. To tell whether a copy is out of date,
`node installer/make-firefox.js --check firefox` (or
`.\installer\make-firefox.ps1 -Check firefox`) lists each file that differs,
is missing or is extra, and fails if there is any; CI checks that both
makers produce the same copy. Setup and `install-host.ps1` register UIDM with
Firefox (under
`HKCU\Software\Mozilla\NativeMessagingHosts`, with a host manifest that names
that ID). Until the extension is signed by Mozilla, load it for each Firefox
session:

1. Open `about:debugging`, choose **This Firefox**, then **Load Temporary
   Add-on...**, and pick `manifest.json` in the `firefox` folder.
2. In `about:addons`, open UIDM's **Permissions** and allow **Access your
   data for all websites**. Firefox asks for this separately; without it the
   video button, the selection button, form downloads and **Download all
   links** do not work, though plain downloads are still caught.

Known gaps in Firefox:

- A temporary add-on is removed when Firefox closes.
- Firefox has no way to hide its download panel, as Chrome's `downloads.ui`
  does. It needs it less: it lets the extension cancel a download at its
  headers (`webRequestBlocking`, which only the Firefox copy asks for), so
  a download UIDM takes there never starts in Firefox. One it only takes
  later (a link with a `download` attribute, say) may show the panel.

These were checked against a stand-in for the browser API in the extension's
tests, not in a running Firefox.

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
  downloads. At its top, **Capture in this tab** turns capture off for the
  tab it was opened over only, as IDM's toolbar button does: that tab's
  downloads, forced links and web players' files stay with the browser, and
  its toolbar icon turns grey with an "X" badge until it is turned on again
  or the tab closes. (IDM's button toggles on a click; UIDM's button keeps
  opening the popup, which holds the rest of these features, so the switch
  is its first row.) Under **Downloading in the browser** it lists the browser's own
  downloads in progress (ones it kept, or started before UIDM ran): **Send to
  UIDM** cancels and erases the browser's copy and starts it in UIDM, with its
  referrer, name and cookies, as a captured download. This stands in for
  IDM's menu on a started download, which Chrome does not let extensions
  add; it works the same in Firefox.

- **A save-as dialog** opens for each new download, with its real name and
  size, its category, and the folder that category picks. Picking another
  category moves the file to that category's folder and lists the download
  under it whatever its type; **+** next to the list adds a category on the
  spot (saved to Options) and picks it. A **Description** typed here is kept
  with the download and shows in the list and in Properties. Change the name
  or folder, then **Start download**, **Download later** (it waits in the list with a
  **Start** button) or **Cancel**. If the window is closed, UIDM opens it; if
  no window shows up within 20 seconds, the download starts as proposed.
  Options can turn the dialog off, or skip it for some file types
  (**Start these types without asking**, such as `exe, msi`; the type is
  checked again once the server names the file). **Start downloading while
  the save-as dialog is open** fetches the file while you decide, into a
  folder of its own (`UIDM early` in the Windows temporary folder), and the
  dialog shows how much has come. Answering keeps those bytes: a finished
  file moves to the name and folder chosen, and an unfinished one carries on
  from where it got to (and **Download later** keeps it to carry on later).
  Only files the server sends in ranges start early; canceling drops what
  was fetched. **Only add them to the Download later queue, without
  starting them** makes every new download wait in that queue, without the
  dialog, until the queue or the download is started. **Remember this path for <category>**
  sends the category's new files to the folder last chosen for it, General
  included (the categories' **Last folder** box in Options, and the box under
  them for General). The dialog asks which queue **Download later** uses,
  even when only the main queue exists; its **+** adds a queue by name and
  chooses it. **Don't ask me again** makes that queue the
  answer for the dialog, **Later** and the browser's **Download later with
  UIDM** alike, until **Download later puts files in** in Options changes
  it. **Start the queue now** starts that queue as soon as **Download
  later** has put the download in it.
- **When a file name is taken**, UIDM saves the new file as `name (1).ext`
  by default. Options (**If the file exists**) can make it overwrite the old
  file instead, or ask. With **Ask me**, the save-as dialog says the name is
  taken and offers **Save with a new name** or **Overwrite it**. This holds
  even with the dialog turned off: the download stops before writing anything
  and opens the dialog. If no window answers within 20 seconds, it gets a new
  name.
- **When UIDM may not write to the folder** (one under `C:\Program Files`,
  say), the save-as dialog opens even when it is turned off and says so.
  Pick another folder with **Browse...**, cancel, or tick **Save there with
  administrator rights** (Windows only). With that ticked, the download
  still runs as you, in a staging folder (`UIDM\elevated-<id>` in the
  temporary folder), and when it is done Windows asks for administrator
  rights and a helper (`uidm-daemon --move-elevated`) moves the file into
  place. If you say No, the download fails with a message naming the
  finished file, which stays in the staging folder; starting it again asks
  again without downloading it again. IDM does this with its elevated
  helper, `idmfsa.dll`.
- **When the address is already in the list**, UIDM asks by default: the
  save-as dialog (shown for this even when it is turned off) says which
  download has it and offers **Add it anyway** (with a new name if the old
  file is in the way), **Replace the old download** (it leaves the list and
  the new one overwrites its file; not while the old one is running) or
  **Don't add it** (a finished one shows its completion dialog again, a
  stopped one resumes, a running one is left alone). Options (**If the
  address is in the list**) can make one of these the answer every time, and
  so can **Remember my selection** in the dialog.
  Addresses match ignoring `#fragments` and the case of the host. Form
  downloads and streams are never treated as duplicates. If no window
  answers within 20 seconds, the download is added with a new name.
- **Adding many at once** (**Add many**, imports, the link picker and the
  site grabber) asks one question at a time, before anything is added, about
  each address already in the list (while that setting is **Ask me**: add it
  again, with a new name if needed, or leave it out) and each file name
  already taken in its folder (while **If the file exists** is **Ask me**:
  overwrite it, or save with a numbered name). Each question has **Yes**,
  **Yes to all**, **No** and **No to all**; the "to all" answers cover the
  rest of that kind of question in this batch only, and **Cancel** adds none.
  Under the other settings nothing is asked: addresses in the list are
  skipped unless the setting adds them with a new name or replaces stopped
  ones, and taken names follow the setting. A scheduled grabber run never
  asks. The name checked is the one the address gives; a server that names
  the file otherwise is still asked about in the save-as dialog. **Options >
  General > Adding many** can hide the queue panel of **Add many** and the
  link picker; their downloads then go to the main queue without starting
  it.
- **Categories** sort downloads by file type into their own folders:
  Compressed, Documents, Music, Programs and Video go into folders of those
  names inside the download folder, and everything else (**General**) goes
  into the download folder itself. The window's sidebar filters the list by
  category. Options edits the categories, their file types and folders.
  General has a type list of its own too, empty at first: a type listed
  there is General's even when a category lists it.
- **Queues** hold downloads to run later. **Later** (next to Download), the
  save-as dialog's **Download later**, and the browser's **Download later
  with UIDM** menu item add a download to a queue without opening a
  connection. Pick a queue in the sidebar to see its downloads in order, with
  **Up** and **Down** to reorder them, **Start**/**Stop** for the queue, and
  what happens when this run ends. A download's **Move to queue** menu moves
  it to another queue, and **Remove from queue** takes it out of its queue
  but keeps it in the list: it waits outside every queue, also after a
  restart, until you start it by hand or move it to a queue again. A started queue runs its waiting downloads in order, as many
  at once as it allows. **Stop** pauses its running downloads back into it,
  progress kept. When nothing is left the queue stops by itself. A queue is
  **One-time downloading** (finished downloads leave it) or **Periodic
  synchronization**: finished downloads stay in it, and each run asks the
  server about them and downloads again, over the same file in the same row,
  those whose size, ETag or Last-Modified date changed (or whose file is
  gone). The new copy is written beside the old file and replaces it only
  once complete; a check that fails leaves the file alone and notes why.
  Streams and form answers are not checked. **Manage queues...** creates,
  renames and deletes queues (the main queue stays) and sets each one's
  type, downloads at once, retries for failed downloads, and what to do when
  it is done, including a file or program to open (**Browse...** picks it).
  Its **Start now** and **Stop** buttons start or stop the selected queue
  as saved, and **Help** opens the scheduler's help.
- **Scheduler**: a queue can start at a set time on chosen days of the week,
  or once on a date, and optionally stop at a set time (the next morning if
  earlier than the start). **Start again every** N hours M minutes starts
  it again that long after each start, until the stop time (or midnight
  without one); the last start is saved, so a restart keeps the rhythm.
  Grabber projects have the same choice, and the same **Browse...** for the
  file to open when a run is done. The queue's header shows its next
  run. If UIDM was not running at the start time, the queue starts as soon
  as it is, while the time window is still open. The Queues dialog says so
  next to a schedule until **Don't show this again** hides the note.
  **Wake the computer to start** (on by default, for queues and grabber
  projects) sets a Windows wake timer, so a sleeping or hibernating PC
  wakes 30 seconds before the start time, as IDM does. UIDM must be
  running, and the power plan must allow wake timers; the schedule shows a
  warning with the setting's location when it does not.
- **When a queue is done**, UIDM can do nothing, exit, or put the computer to
  sleep, hibernate or shut it down, per queue or for the current run only.
  Sleeping, hibernating and shutting down wait for a 60-second countdown in
  the window, which **Cancel** stops and **Shut down now** (or **Sleep
  now**, **Hibernate now**) skips. **Force apps to close** shuts down even
  when a program asks to save its work first. Failed downloads count as
  finished, unless the queue retries them.
- **The tray icon** keeps UIDM at hand. Closing the window hides it there;
  click the icon to bring it back. Its tooltip shows how many downloads run
  and how fast, and its menu has **Open UIDM**, **Add URL from clipboard**,
  **Add batch download from clipboard**, **Pause all**, **Resume all**, **Speed limit** and **Exit**. Exit closes the
  window; downloads keep going and the daemon stops on its own once idle.
  **Pause all** stops every queued and running download, keeping its partial
  file; **Resume all** starts them again, along with any interrupted ones.
  **View > Tray icon** picks the **Colored icon** or a **White icon** (a
  white silhouette of it, for a dark taskbar), or **Don't show**; the style
  changes at once and is kept with the window's layout.
- **Start with Windows** (in Options) starts UIDM hidden in the tray when you
  sign in, ready for the browser. A download caught while the window is in
  the tray opens its save-as dialog on its own, as IDM's does. The save-as
  and completion dialogs have a taskbar button and stay on top of other
  windows until closed, but leave the keyboard with the program you are in,
  so nothing you type goes into them by accident.
- **Keep the computer awake while downloading** (in Options, on by default)
  stops Windows from going to sleep on its own while a download, a site grab
  or a conversion runs, as IDM does. The screen can still turn off, and a
  queue's own sleep or shutdown still happens.
- **The download window** (double-click a download's name, or **More >
  Download window**) shows its address, status, size, how much is in, the
  transfer rate, the time left and whether the server lets it resume, with
  **Pause** or **Resume** and **Cancel** (which stops it and closes the
  window; the download stays in the list). **Show details** adds the
  connection bars and table from **More > Connections...**, and the **Speed
  limit** tab sets the download's own limit, with **Remember on stop and
  resume** (on by default; untick it to have the limit dropped when the
  download is stopped and resumed). The **When done** tab (also in
  **Properties**) sets what happens once this download finishes: show the
  completion dialog whatever Options says, then exit UIDM, sleep, hibernate
  or shut down (after the same 60-second countdown a queue uses). It runs
  once, after the virus scan, and not for a download that fails.
  It opens whenever a download starts (**Options > General** can turn that
  off), behind the program you are in, and opens even while UIDM is in the
  tray, without bringing up the main window. A download a queue starts opens
  its window hidden in the tray, as IDM's do. **Minimize** sends the window
  to the taskbar; **Hide** (shown while the tray icon is on) puts it in the
  tray behind a second icon, green arrows, whose menu lists the hidden
  windows by title, with **Show all** once there are two. Pick one to bring
  it back. A hidden window closes when its download finishes or fails. The
  icon goes when the last of them does, and turning the tray icon off brings
  them all back. **Options > General** can also
  hide either tab (as can its **Hide tab** button), hide the **Hide tab**
  buttons themselves, and choose the tab the window opens on and whether it
  opens with its details shown. The window closes when the download
  finishes.
- **When a download ends**, UIDM shows a Windows notification: **Open** and
  **Show in folder** for a finished file, **Retry** for a failed one. Options
  can show an IDM-style dialog in the window instead (a notification still
  appears if the window is not running), or nothing. **Options > When done >
  Sounds** plays a sound when a download finishes, when one fails, when a
  queue starts (by hand or on its schedule) and when a queue is done, each
  turned on by itself, with a `.wav` file of your own
  (**Browse...**, **Play**) or Windows' sound for the event. The dialog's
  **Don't show this dialog again** goes back to notifications. Canceled
  downloads are never announced. On Windows, drag the dialog's file icon,
  or a finished download's row in the list, into a folder to copy the file
  there, or into another program to hand it the file, such as a player's
  playlist. A file that is gone drags nothing.
- **Virus scan** (off by default) checks each finished file with Microsoft
  Defender or a scanner command you set, with `{path}` where the file goes;
  **Browse...** picks the scanner program and keeps the arguments typed.
  The list shows **No threat found**, **Threat found** or **Scan failed**,
  and opening a file with a threat asks first. With scanning on, the
  notification waits for the scan and says what it found.
- **New link** on a failed, canceled, interrupted or waiting download gives
  it a fresh link when the old one expired: paste the new address, or choose
  **Use my next download in the browser** and start the download again on its
  web page; the extension hands that download to UIDM as the new link instead
  of starting another. The part already downloaded is kept only if the new
  link is for the same file (same size, and the same ETag or Last-Modified
  when the server sends one); otherwise the window says why and asks before
  starting over.
- **Proxies and site logins** (Options, **Connection**): no proxy, the
  Windows proxy setting, or HTTP, HTTPS, FTP and SOCKS proxies, each with
  its own optional user name and password; **Get System** fills them in from
  the Windows setting. The SOCKS proxy carries what has no proxy of its own,
  `ftp://` downloads too when no FTP proxy is set, and can carry every
  `https://` address while plain `http://` goes through the HTTP proxy.
  Downloads from the browser can use the browser's proxy, always or only to
  retry one that failed. Site logins pair a site with a user name and
  password; a site can name one of its folders, such as
  `example.com/private/*`, to use another login there. When a server asks
  for a login (HTTP Basic, Digest or NTLM), UIDM sends the matching one to that
  site and no other. A saved login goes over plain http as Basic, or to an
  FTP server, only with its **Plain connections** box ticked, since there
  the password can be read on the way; a login saved from **Log in to the
  site** for a plain http or FTP link has it ticked. Without one (or when the
  server refuses it) the download stops, and the window asks: **Log in to
  the site** shows the site and the realm it named, takes a user name and
  password, and retries the download with them. They stay the download's
  own login, or with **Save this password** become the site's login in
  Options (always, when the file is on another site than its link). Other
  downloads from the same site waiting for a login go with the same answer.
  **Cancel** leaves it stopped, with a **Log in...** button in its row. A
  login can also go with an address from the start: **Login** next to the
  URL bar shows user name and password fields for the addresses added
  there, and **Add many** has **Use a login** for every address in it.
- **Download this video**: a bar in IDM's style appears over a video playing
  in the browser, with **?** for help and **x** to close it for that video
  until the page loads again. It also appears over `<audio>` players and `<object>`/`<embed>` media players (an
  audio player gets it just above). It lists what that player loaded:
  direct video and audio files with their sizes, and streams (HLS `.m3u8`,
  DASH `.mpd`, HDS `.f4m`) with each quality UIDM read from the playlist and its
  estimated size; on a feed with many players each button lists its own
  player's streams, and the page's whole list when it cannot tell. The
  button hides while its player is covered by something else on the page,
  while the tab is in the background or its window minimized, and in full
  screen. Pick one and UIDM downloads it, named after the page (its
  `og:title`, else the tab's title, also for a player in a frame) without the
  site's name after it ("Cute cat - YouTube" becomes "Cute cat.mp4", on any
  site). A stream's segments are fetched in parallel
  and joined into one video (see [Streams](#streams-batches-and-the-site-grabber)).
  Videos protected by DRM say so and offer nothing: UIDM does not get around
  DRM. The popup turns the button off everywhere or on the current site;
  **Options > Videos and menus** has the same switch and the site list, the
  video and audio types the button lists (with **Check all**, **Clear all**
  and **Add**; M3U8 and MPD stand for streams), the smallest file it lists
  (128 KB by default), a full or mini look for it and for the button by
  selected links, and whether it shows on protected videos to say so.
  **Web players** (off by default) hands a web player's media of the listed
  types (MP4, M4V, WebM, FLV, MP3 and M4A at first) to UIDM as soon as the
  page's `<video>` or `<audio>` loads it, without the button, as IDM's
  "automatically start downloading from web players" does: only whole
  files, not HLS or DASH streams, and each address once per tab.
- **Context menu**: the extension adds **Download with UIDM**, **Download
  later with UIDM**, **Download page with UIDM** (on a page or a frame: that
  page's own address, or the frame's), **Download all links**, **Download
  selected links**, **Download the last video with UIDM** (the video or audio
  the browser loaded last, in any tab, in its best quality) and **Recent
  videos**, a submenu of the last 10. **Options > Videos and menus** chooses
  which of them show, apart for Chromium browsers (Chrome, Edge and the like)
  and Firefox; the extension knows Firefox by its `moz-extension://` pages.
  A link sent from the menu takes its `download` attribute as the file name
  (only its last part, made safe) and its title, else its text, as the
  download's description, as IDM does. Firefox shows **Download selected
  links** only when the selection holds links (Chrome cannot hide a menu item
  as it opens, so there it shows for every selection). When an item cannot
  do its job, a browser notice says so: no links found on the page or in the
  selection, a page the browser does not let extensions read, or **Cannot
  reach UIDM** (with the browser's reason when it gave one; a link then
  downloads in the browser instead).
- **More > Add many...** adds downloads from a pattern such as
  `https://example.com/photo[001-120].jpg` (with steps like `[0-100:5]` and
  letter ranges like `[a-z]`), or from a pasted list or a text file with one
  address per line. IDM's form works too: type one `*` in the address, as in
  `https://example.com/photo*.jpg`, and the dialog shows IDM's settings:
  replace the `*` with **Numbers** or **Letters (a to z)**, **From** and
  **To**, and a **Wildcard size** that pads numbers with zeros to that many
  digits (1, the default, adds none; letters keep the case typed and ignore
  it). From 1 to 120 with size 3 gives `photo001.jpg` to `photo120.jpg`, the
  same list as IDM; From after To is refused, as in IDM, and only one `*` is
  allowed. An address without a `*` uses the bracket ranges. It previews the addresses, skips blank lines, repeats and
  anything that is not a web address, refuses more than 10,000 at once, and
  adds them waiting to the queue you pick, optionally into one folder and
  optionally starting the queue.
- **More > Import list... / Export list...** move the list to another
  computer or a backup: tick downloads in the list's first column to export
  only those, pick a queue to export the downloads waiting in it (in its
  order), or export all, as a UIDM list (`.json`, with names, folders,
  sizes and status), plain text (one address per line) or an IDM export
  file (`.ef2`, with referrers, which IDM's **Import > From IDM export
  file** reads). Imports read any of them, in UTF-8 or UTF-16, and add
  everything waiting, so nothing starts by itself. An IDM export file brings each download's
  referrer, cookies, user agent and form data, and a login if it has one.
  Exports never hold cookies, headers or passwords.
- **More > Site grabber...** crawls a site from a start page, as many links
  deep as you say, on the same site or not, and lists the files of the types
  you want (PDFs, say) with the page each was found on. It obeys
  `robots.txt`, waits between pages (longer if `Crawl-delay` asks), and stops
  at a page and size budget it shows before starting. Tick the files to keep
  and add them to a queue. It can also:
  - explore the whole start site with no depth limit (the budgets still
    stop it), and follow a set number of links on other sites;
  - read up to 10 pages at once, and set how many files its queue
    downloads at once;
  - keep each file's link text as its description;
  - add the masks of named filters (a name and a mask such as
    `*.jpg;*.png`) to the types kept or to those left out, as IDM's Add
    Filter does (**Filters** next to each field; it also adds, edits and
    deletes them), keep every type with `*`, and keep only files on the
    start site. A bare type (`jpg` or `*.jpg`) matches the extension, and a
    type of several parts (`tar.gz`) matches names ending in it; a mask
    with a name part (`photo_*.jpg`, `img??.png`) matches the whole file
    name, ignoring case, with `*` for any run of characters and `?` for one;
  - save every file to one category's folder, or to one folder, with or
    without the site's subfolders (**Use original relative subfolders**:
    the folders below the start page's folder, or the host and path for a
    file elsewhere), and overwrite files already there or not, whether or
    not it saves for offline browsing;
  - start downloading each match as soon as it is found;
  - log in to the start site with the project's own user name and password;
  - sign in on a login page: **Open** opens it in your browser (UIDM has no
    browser of its own) so you sign in there, and when the crawl starts
    the extension hands over that site's cookies; without the extension,
    paste them in **Cookies**;
  - when scripts run, ignore the windows they open or explore them too.

  While it runs with downloading as found, it shows how many of the files
  it added are downloaded so far. The results show as a list or as a tree
  by page or by folder, with pages processed and file links looked at,
  matched and downloaded; **Stop** stops the running or waiting downloads
  of the ticked files. **Update all** explores the pages again with their
  scripts run, keeps the list and its ticks, and adds what that finds. When
  the crawl downloaded its finds as it went, closing the grabber adds
  nothing more. **Save as
  template** keeps the settings under a name, and **New** starts a project
  from a saved template or one that comes with UIDM (pictures, audio,
  video, documents, or archives and programs from a site). A scheduled
  project explores only, explores and downloads the new files (starting the
  queue), or downloads the ticked files without exploring. Like a queue, it
  is one-time downloading or periodic synchronization, which also checks the
  files earlier runs found (or the ticked ones) and downloads again those
  changed on the server. It then exits UIDM,
  sleeps, hibernates or shuts down (with its own **Force apps to close**),
  and can open a file, after the crawl or once the queue is done. Its stop
  time ends the queue's downloads too, as it ends the crawl.
  **Add the ticked files when the grabber closes** adds them on closing as
  **Add** does. Closing, starting a crawl or switching to another project
  asks to save one that changed since it was saved (or was never saved),
  unless **Don't ask if the name is unique** is ticked and no other saved
  project has its name; then it is saved without asking. **Open > From
  another folder...** opens a project file kept anywhere, such as one copied
  from another computer; saving it keeps it with the other projects.
- **Clipboard** (off by default; **Options > Browser capture**): copying a
  link whose file type is on the capture list brings up a small **Download
  the copied link?** prompt with **Download** and **Ignore**, and the same
  link is not offered twice in a row. **Start downloading it at once** skips
  the prompt: the link is added as a pasted one is, save-as dialog included.
  **More > Add batch download from clipboard** (also in the tray menu) opens
  **Add many** with every http, https and ftp link in the copied text, of any
  file type, to check and add.
- **Download all links with UIDM** (the browser's menu on a page or a text
  selection, and the button over a selection) lists the links in the window
  to tick. As IDM does, it collects more than a page's links: its frames'
  addresses, addresses in its inline scripts (absolute ones, and quoted
  relative paths ending in a captured file type; the first 1 MB of script
  per page), the pictures images' onclick handlers open, its images (with
  their largest srcset picture), and the links in frames made from nothing
  (about:blank or srcdoc, up to three deep). **Download selected links**
  takes the same from what the selection touches, plus the addresses in its
  text, or in the text selected in a text box. Only http, https and ftp
  addresses are kept, each once (a `#fragment` makes no other file), at most
  5000 a page. In the window: filters hide web pages, images, repeated links, or keep some file
  types, and **Edit...** changes a link's address. Meanwhile the daemon asks
  each link's server for its type and size, four links at a time and with
  the link's cookies, referrer and proxy, and the **Type** and **Size**
  columns fill in as the answers come (`...` while asking); closing the
  picker stops the asking. The ticked files go to a
  queue, saved to each one's category folder, all to **one category** (listed
  under it and saved to its folder), or to one folder. **Rename** names them
  by a pattern in list order: `photo_*` gives `photo_01.jpg`, `photo_02.jpg`
  and so on (the number is padded to the count, each file keeps its type
  unless the pattern names one).
- **Options** (top right of the window) has five tabs. **General**: the
  download folder, connections per file (1 to 16), downloads at once (1 to
  10), a total speed limit (with, while it is on, a limit for each file
  that has no limit of its own, and **Always turn the speed limit on when
  UIDM starts**), a download quota (no more than so many MB every
  so many hours, with a warning at 90% that can be turned off), what to do when a file name is taken or an
  address is already in the list, the save-as
  dialog, whether to resume unfinished downloads when UIDM starts, Start with
  Windows, whether UIDM shows a tray icon (without one, closing the window
  quits and the Start menu opens UIDM again), whether closing the window
  keeps UIDM in the tray, whether minimizing the window hides it in the
  tray (on by default, as in IDM), where ffmpeg
  is, the **Language**, the **Theme** (follow Windows, light or dark) and the
  **Text size** (80% to 150%, scaling the whole window). Language, theme and
  text size change as soon as you save. **More > Hide categories** hides
  the categories and queues panel, and **More > Hide the tray icon** does
  what the Options box does; both are kept. **Categories**: as above. **Browser
  capture**: the browsers to capture from, the rules below, the special
  keys and the clipboard prompt. **Videos and menus**: the video button and
  the context menu items. **When done**: notifications, sounds and the virus
  scan. **Connection**: proxies and site logins. The status bar shows the
  speed limit and toggles it.
- **Browsers** (**Options > Browser capture**): lists Google Chrome,
  Microsoft Edge, Mozilla Firefox, Brave and Chromium when they are
  installed for you (their profile folder exists) or registered. Ticking
  one writes its native-messaging key under `HKEY_CURRENT_USER` and the
  manifest it names (`%LOCALAPPDATA%\UIDM\com.uidm.host.json`, or
  `com.uidm.host.firefox.json` for Firefox), pointing at the `uidm-host.exe`
  next to the window; unticking removes the key, and the manifest once no
  browser names it. Setup registers Chrome, Edge and Firefox. Vivaldi and
  Opera read Chrome's registration, so the list says so when it finds them
  instead of listing them. Restart a browser after a change: its
  **Restart** button asks first, then closes it the polite way (as closing
  its window does, so it keeps its tabs), ends what is left of it after 8
  seconds (Chromium keeps helper processes, and with "Continue running
  background apps" the browser itself), and starts it again. **Add
  browser...** adds any other browser by its program file: UIDM tells its
  family from the program's name and registers it where that browser reads
  registrations, or, for a Chromium browser it does not know, where Chrome
  does, which most of them read (a Firefox fork gets Firefox's). Added
  browsers stay in the list, each with **Remove**. A line above the list
  says whether the extension is connected, or when it last was.

- **Properties** (a row's **More**) shows a download's status, its file type
  (what Windows calls it, such as "PDF Document (.pdf)"), size and last
  try, and the web page it came from, and edits its link, referrer,
  description, name, folder and login. A finished download's file is
  renamed or moved; a stopped, unfinished one (waiting, failed, canceled or
  interrupted) can change folder with **Browse...**, and its partial file and
  resume sidecar move along so it resumes there (one in the temporary
  folder stays put). A running download must be stopped first. A finished
  download has an **Open** button.
- **Find** (Ctrl+F, then F3 and Shift+F3 to step) narrows the list to the
  matches. **File name**, **Description** and **Links (address/referrer/page)**
  are each a place to look, at least one ticked. **Whole name only** wants the
  whole file name, and in descriptions and links whole words: "example"
  finds `https://www.example.com/x` (a host label; a host or a path segment
  works too) but not `https://myexample.com`.
- **The list's columns**: a click on a header sorts by it (again to
  reverse, a third time for the order added), and dragging the thin edge
  after a header sets the column's width. **More > Columns...** shows,
  hides, orders and sizes them (**Fit** sizes a column to what it holds;
  the list follows each change at once, and **Cancel** puts it back);
  the columns include **Parent web page**, the page a download came from
  (the browser tab's address), and **Referrer**, the address sent to the
  server as its Referer, which can be a frame inside that page. Both sort
  like the rest, and **View > Arrange files** has **By Referrer** and **By
  Parent Web Page**. Downloads from before the two were kept apart use
  their referrer as their page. **View > Font > Choose Font...** sets the
  list's text: the window's text font or its fixed-width font (Segoe UI and
  Consolas on Windows; there is no system font picker), at 8 to 24 points,
  with a sample; **Reset to default** goes back to the window's font. The
  layout and font are kept in `%LOCALAPPDATA%\UIDM\window.json`.
- **Help** (next to **More**): **Help contents** opens this README on
  GitHub, two more items open its sections on queues and the scheduler and
  on the site grabber, **Tutorials** opens its first steps (this **Use**
  section), **Tip of the day** shows a tip under a "Do you know..." header
  with **Next tip**, and **Contact support** opens the issue tracker. A tip shows when the window
  opens unless its **Show tips on startup** box (also in **Options >
  General**) is cleared; the next start shows the next tip. **About** sits
  beside **Help**, and **More > Manage queues...** opens the queues even with
  the categories panel hidden.
- **Command line**: `uidm-gui` takes IDM's switches, so scripts and other
  programs can start downloads:

  ```
  uidm-gui /d URL [/p folder] [/f file_name] [/a] [/n] [/q] [/h]
  uidm-gui /s
  ```

  `/d` adds URL to the list (http, https or ftp) and shows the usual
  download dialog; `/p` and `/f` set its folder and file name, `/a` adds it
  to the main queue without starting it, and `/n` asks nothing (a taken name
  gets a numbered one). `/q` exits UIDM once that download finishes, but only
  when the call started UIDM, as in IDM. `/s` starts the main queue. `/h`
  (hang up a dial-up connection) is accepted and does nothing. `/p`, `/f`,
  `/a`, `/n`, `/q` and `/h` need `/d`. Each switch also works as `-d` and as
  `--download`, `--path`, `--file`, `--add`, `--silent`, `--quit`,
  `--hang-up` and `--start-queue`. With UIDM already open, the call hands the
  switches to that window and exits; otherwise it starts UIDM in the tray
  and runs them there. For example:

  ```
  uidm-gui.exe /n /d "https://example.com/files/setup.exe" /p D:\Downloads
  ```
- **Sending links from other programs**: `uidm-host send` takes what IDM's
  `SendLinkToIDM2` and `SendLinksArray` COM calls take, as JSON in a file or
  on stdin (`-`):

  ```
  uidm-host send links.json
  uidm-host send -
  ```

  One link is an object with `url` and any of `referer`, `cookies`,
  `post_data` (the form body as text; it makes the download a POST, sent as
  `application/x-www-form-urlencoded` unless `content_type` says otherwise),
  `user`, `password`, `folder`, `filename` and `user_agent`. `"silent": true`
  adds it with no dialog and `"add_only": true` adds it to the main queue
  without starting it, like `/n` and `/a`; IDM's `flags` number (1 and 2)
  works too. It is added as a link from the browser is, and its download id
  is printed. A list is an object with `links`, each an address or an object
  with `url` and an optional `text` and `cookies`, plus one `referer` for them
  all; it opens the link picker with them. A relative `folder` is under the
  current one, and an empty string counts as left out. UIDM starts if it is
  not running. The exit code is 0 when UIDM took the links, 1 when it refused
  them or could not be reached, and 2 for bad input. The host talks to the
  daemon over the same authenticated connection as the browser (see
  [Host-daemon authentication](#host-daemon-authentication)), so only
  programs running as the same Windows user can send links. From PowerShell:

  ```powershell
  $uidm = "$env:LOCALAPPDATA\Programs\UIDM\uidm-host.exe"
  @{ url = "https://example.com/report"; referer = "https://example.com/";
     cookies = "session=abc"; post_data = "id=42"; folder = "D:\Reports";
     filename = "report.pdf"; silent = $true } | ConvertTo-Json | & $uidm send -
  @{ referer = "https://example.com/files/";
     links = @("https://example.com/a.zip", "https://example.com/b.zip") } |
     ConvertTo-Json | & $uidm send -
  ```

  Or from Python:

  ```python
  import json, os, subprocess
  uidm = os.path.expandvars(r"%LOCALAPPDATA%\Programs\UIDM\uidm-host.exe")
  link = {"url": "https://example.com/a.zip", "user": "me", "password": "pw", "add_only": True}
  subprocess.run([uidm, "send", "-"], input=json.dumps(link), text=True, check=True)
  ```
- **Languages**: the window comes in English, German, Spanish and French.
  It starts in the first Windows display language it has, else English;
  **Options > General > Language** picks another. The extension follows the
  browser's language, with the same four. Messages from the daemon (errors,
  notifications) are still in English.

Files are saved to your Downloads folder, sorted by category, unless Options
says otherwise.
Closing the browser or the window does not stop a download. The list survives
restarts: downloads that were running when UIDM stopped show as
**Interrupted**, and **Retry** resumes them from their partial file.

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
  clients and no queued or running downloads. It also shows the
  notifications, plays the sounds and runs the virus scans, since it is the
  one process running whenever a download ends. Its log is
  `%LOCALAPPDATA%\UIDM\uidm-daemon.log`, beside the host's `uidm-host.log`
  and the window's `uidm-gui.log` (`~/Library/Application Support/UIDM` on
  macOS, `~/.local/share/uidm` on Linux). A log over 5 MB moves to `.old` when
  the daemon starts. Passwords in addresses are hidden as `***`. It keeps its
  files in:
  - `%APPDATA%\UIDM\settings.json`: the settings. A missing file or field
    means the default.
  - `%LOCALAPPDATA%\UIDM\history.json`: the download list and the next id,
    written about a second after each change and again on exit. It never
    holds cookies or request headers.
  - `%LOCALAPPDATA%\UIDM\requests.bin`: the original requests of downloads
    that can still be retried, cookies included, encrypted with Windows DPAPI
    for the current user. It is deleted when none is left.
- **`uidm-host`** is what the browser launches. It connects to the daemon,
  starting it if nothing answers, then copies frames between the browser's
  stdin/stdout and the daemon without parsing them. It waits up to 10 seconds
  for a daemon it just started, so it gives up within about 15 seconds if no
  genuine daemon answers. When the browser closes the port, the host closes
  its side, delivers any reply still in flight for up to 2 seconds, and exits.
  The daemon is started with the host's stdio marked non-inheritable, so it
  never holds the browser's pipes open. `uidm-host send` connects the same
  way to hand the daemon links from other programs.
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
  list, unless **Capture downloads in private windows too** is on in
  **Options > Browser capture** (off by default; only the window changes it).
  Then private windows' downloads, links, page buttons and context menus work
  as in normal windows, with the private window's own cookies. The browser
  must let the extension in too: **Allow in Incognito** in the extension's
  details in Chrome and Edge, **Run in Private Windows** in Firefox;
- capture is not turned off in the tab it comes from (**Capture in this tab**
  in the popup);
- the extension did not start it itself (that is how hand-backs avoid loops);
- it passes the capture rules from **Options > Browser capture**:
  - its file type is in the list (by default every category's types plus
    `iso`, `img`, `dmg`, `deb`, `rpm` and `bin`, which **Default** brings
    back; an empty list takes every type; **Default** next to the excluded
    sites empties that list). The type comes from the file name, else the
    MIME type the server sent, else the URL. A download that says only "a file"
    (`application/octet-stream`) is taken;
  - it is not smaller than the minimum size, when the browser already knows
    the size (often it does not yet, and then only the type and site count);
  - neither its URL nor the page it came from is on an excluded site.
    `example.com` and `*.example.com` both cover the site and every subdomain;
  - its URL matches none of the excluded addresses, where `*` matches any
    run of characters and an address without `http://` or `https://`
    matches both. UIDM starts with a short list of site sounds and browser
    and Windows update files that sites fetch on their own (taken from IDM's
    own list). Remove any you want captured; **Restore defaults** adds them
    back and keeps your own;
- **Skip next download** in the popup was not pressed; it lets exactly one
  download stay in the browser;
- the skip keys were not held while clicking its link (Alt by default), nor
  while its tab started it some other way (a form sent, a page's script).

Holding the force keys (Ctrl by default) while clicking a link sends it to
UIDM whatever the rules say, instead of opening it. Each is any mix of Ctrl,
Alt and Shift, plus Insert or Del for forcing, set in **Options > Browser capture**,
and must be held exactly: Alt+Shift is not Alt. They work with a left or
middle click and with Enter on a link, or with the option set only with a
left click. By default the force keys leave links to web pages, pictures and
scripts (`.html`, `.php`, `.jpg`, `.js`, a path ending in `/` and the like)
to open as usual.

As in IDM, the keys also count for downloads their tab starts while they
are held without a click on a link, such as a form sent with Enter or a
script's download: the page reports the keys going down and up to the
extension, which applies them to that tab's next downloads. Switching tabs,
the page losing the focus or being hidden forgets them, and a report not
refreshed for 30 seconds (the page went away) is dropped.

The extension asks for the rules when it connects and gets every change as it
is saved, so they are edited only in the window.

**Caught at the headers.** As IDM does, the extension looks at each page
and frame response when its headers arrive (`webRequest.onHeadersReceived`):
one marked `Content-Disposition: attachment`, or of a type the browser
cannot show, is a download, and the capture rules are applied to its name,
type and size there. Firefox lets the extension cancel it then
(`webRequestBlocking`), so the browser never starts it and UIDM gets it at
once. Chrome lets extensions like this one only look, not cancel, so there
the download still appears in the browser and is taken from it at once
(`downloads.onCreated`), with the size, type and name the headers gave
(the download item often has none yet); the download bubble is hidden from
the moment the headers say UIDM takes it (`downloads.setUiOptions`) until
the browser's copy is canceled and erased, and comes back after ten seconds
whatever happens. A tab another tab opened a moment before only for the
download (it still shows nothing or the download's address, and never
showed a page) is closed once UIDM takes the download.

**The whole request.** The extension follows the requests of pages and
frames: the headers the browser sent (`onSendHeaders`, with Chrome's
`extraHeaders` so nothing is hidden), the redirects it followed, and what
the response said. A download goes to UIDM with them: `request_headers`
(without `Cookie`, which goes as `cookies`), `redirects`, `size` and
`content_type`. The daemon repeats the headers that are safe to: not the
ones about the connection (`Connection`, `Keep-Alive`, `TE`, `Upgrade`,
...), not `Host`, `Content-Length`, `Range`, the conditional `If-*` ones or
`Accept-Encoding` (the engine asks for ranges and decompresses what it asks
for itself), not `Authorization` (logins are UIDM's), and not `Cookie`,
`Referer` and `User-Agent`, which come in their own fields. It keeps at most
64 headers, 20 redirects and values of 8 KB. The size and type are shown
until the server is asked, and are all a form download has before it
starts. The referrer is the document that started the download; a frame
made from nothing (`about:blank`, `about:srcdoc`) is skipped for the
nearest frame above it with an address (`webNavigation.getAllFrames`), else
the page.

A download a form started, with a POST, is sent to UIDM as the same POST.
The extension records the method and body of recent form submissions in
frames and of scripts' requests (`webRequest`, `requestBody`) and matches a
new download to the request it came from, else the latest form to the same
URL in the last minute (a script's XMLHttpRequest or fetch is matched only
as itself, as when a web player's file is taken). It rebuilds URL-encoded
forms, multipart forms (with a boundary of its own) and raw bodies such as
a script's JSON. The browser does not share a multipart form's files, so
the page tells the extension which of its fields are files as the form is
sent: they are left out, and a form that sends a chosen file stays in the
browser, as does a body the browser did not share. UIDM sends the POST once
and saves the response as one stream; it is not resumable, and it is not
repeated on a failure, since a form may not be safe to send twice.
Downloads without a recorded POST are fetched with GET as before.

If UIDM takes a download but cannot even start it, for example because the
server refuses the request, the extension removes it from UIDM's list and
hands it back to the browser, so it is never silently lost. A POST goes back
as a POST with its form, when the form is text.

Either way the user hears of it: a browser notice, "UIDM could not take over
the download", names the file and why (UIDM's error, or that UIDM is not
running or not connected, for a download the rules would have taken). Its
**Don't show again** button turns these notices off for good; Firefox's
notices have no buttons, so there a click on the notice does it.

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

The engine talks HTTP/2 when the server offers it (through TLS ALPN, as IDM
does) and HTTP/1.1 otherwise. On HTTP/2 the segments of a download are
streams on one connection, and **Maximum connections per server** counts
them as it counts connections. The client is reqwest's async one on a small
shared runtime; the engine's threads block on each step, and a response that
sends nothing for 60 seconds fails its segment, which is retried.

Given an `add`, the engine:

1. Probes the URL with a one-byte range request. A `206` reply means the server
   supports ranges and reports the total size; anything else falls back to a
   single stream.
2. Splits the total into up to 8 contiguous byte ranges (one connection per
   ~256 KiB, capped at 16), and fetches them on parallel threads. Each thread
   opens its own file handle and writes at its own offset, so writes never race.
   A connection that finishes its range early takes over the second half of
   the range with the most left, as long as both halves keep at least 256 KiB.
   The connection that had it stops at the new end of its first half without a
   new request, so the download no longer ends at the pace of its slowest
   range. The split ranges are saved in the sidecar like the first ones.
3. Records progress in a `<file>.uidmpart` JSON sidecar next to the output,
   flushed about every 1 MiB and again on cancel or failure. Re-running the
   download reads the sidecar and refetches only the missing bytes of each
   segment. The sidecar is checked against the server's ETag, Last-Modified and
   size, so stale bytes are never stitched onto a changed file, and it is
   deleted on success. Some servers report a new Last-Modified on every
   request; for them, **Options > General > Ignore file modification time
   changes when resuming** drops the date check, keeping the size and ETag
   checks.
   **Give finished files the date the server reports** sets a finished
   file's modified time to the server's Last-Modified date, and **Use the
   server's date for the file's creation time too** sets its creation time
   as well. Only Windows and macOS keep a creation time that can be set;
   on Linux that option is greyed out.
4. Retries a segment that fails with exponential backoff (400 ms doubling to a
   10 s ceiling, up to 5 times), resuming from that segment's own cursor.

The output filename is, in priority order: an explicit name on the request, the
server's `Content-Disposition` (including the RFC 5987
`filename*=UTF-8''...` form, percent-decoded), the URL path, then a generic
fallback. The file's folder is its category's, from that final name, unless
the save-as dialog chose one.

A new download never writes into an existing file unless that file is a
partial copy of the same download, which its `.uidmpart` sidecar proves (same
size, and same ETag or Last-Modified when the server sends them). Otherwise it
is saved as `name (1).ext`, `name (2).ext`, and so on, or replaces the file
when Options says to overwrite. When Options says to ask, the engine stops
before writing and reports `name_taken`. The daemon holds the download as
`confirming` until the save-as dialog answers with rename or overwrite, or
gives it a new name when no window answers within 20 seconds. A form
download (POST) is renamed without asking, since asking would send the form
again. Two downloads never write the same file at once. A retry reuses the
file its earlier attempt wrote.

Before it writes anything, the engine checks that it may create files in the
folder; if not, the download fails with `Saving here needs administrator
rights: <folder>`. A request with `elevate` set is downloaded into a staging
folder instead, and the finished file is moved into its folder by `uidm-daemon
--move-elevated <from> <to>`, started with the `runas` verb so Windows shows
its UAC prompt. The helper copies the file, so it takes the folder's
permissions, then deletes the staged one.

Downloads run up to the **Downloads at once** setting (3 by default) and the
rest queue in order. Changing the setting takes effect at once: raising it
starts queued downloads, lowering it lets running ones finish. Canceling a
queued download takes it off the queue at once.

The speed limit is one token bucket shared by every connection of every
download. Each read takes tokens for the bytes it read and sleeps when the
bucket runs dry, and reads shrink to about a tenth of a second's worth so
throttled downloads flow evenly. A new limit, or turning it off, applies to
running downloads within 100 ms. Each download also has a bucket of its own,
at its own limit or, while the speed limit is on, at the limit per file;
a read waits for both, so the lower limit wins.

The daemon keeps each download's original request, cookies included, while it
can still be retried, so a retry resends the same headers. It drops the request
once the download finishes or is removed. When a retried download is refused
with HTTP 401, 403, 404 or 410, the error says the link or its sign-in may
have expired.

Ids keep increasing across restarts, so a new download never reuses a saved
id. With **Resume unfinished downloads at startup** on, the daemon retries
every interrupted download as soon as it starts.

## Messages

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
{ "op": "probe_media", "request": 3, "download": { "url": "https://.../manifest.mpd", "cookies": "..." } }
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
{ "op": "grab_progress", "pages": 12, "bytes": 380211, "found": 31 }
{ "op": "grab_done", "files": [ { "url": "https://.../a.pdf", "name": "a.pdf", "page": "https://.../docs/" } ],
  "pages": 40, "stopped": "Stopped at the budget of 40 pages." }
{ "op": "command_line", "window_open": true }
{ "op": "error", "message": "..." }
```

`command_line` is what `uidm-gui` sends for its switches. With a window
open the daemon runs it at once and answers `window_open: true`; otherwise
it holds it for up to a minute and runs it when the next window says
`hello`.

`ack`, `downloads` and `error` go only to the client that asked, as does
`settings` when it answers `get_settings`; everything else goes to every
client, including `settings` after a change. A status is one of `queued`,
`downloading`, `done`, `failed`, `canceled`, `interrupted`, `confirming`
(waiting for the save-as dialog; `path` is the proposed file) or `waiting`
(added to start later). `updated` replaces a whole entry, for the changes no
other event covers. A `hello` with `client: "window"` tells the daemon a
window is open to show save-as dialogs. `confirm` answers the dialog of a
`confirming` download or starts a `waiting` one, keeping its id; `filename`
and `folder` default to the proposed ones, and `start: false` makes it
`waiting`. `elevate: true` saves it into a folder UIDM may not write to,
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
`{path}`. Turning `start_with_windows` on or off adds or removes the `UIDM`
value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, which
runs `uidm-gui.exe --minimized`.

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
`folder`, a full path, fixes where an item is saved.
`probe_media` echoes `request` in its `media_info`, whose `error` says why a
stream cannot be downloaded and `drm` whether that is because of DRM.
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
`video_button_excluded_sites` and `ffmpeg_path` (a full path to
`ffmpeg.exe`, or empty to look on `PATH`).

## Queues and the scheduler

Queues live in the daemon, above the engine, and are saved in
`history.json`. A scheduler thread runs a pass whenever a download ends or a
queue changes, and every 5 seconds for the clock. Each pass:

1. starts a queue whose scheduled run is open now and has not run today (it
   records the run's day, so a queue stopped by hand or finished early does
   not start again in the same window);
2. stops a queue its schedule started once that run's stop time passes; a
   run without a stop time that never got any downloads ends with its day;
3. in each started queue, starts waiting downloads in order until the queue's
   own limit is reached (the engine's **Downloads at once** still caps the
   total), and puts a download that failed in this run back to waiting while
   it has retries left;
4. ends the run of a started queue with nothing waiting or running, stops it,
   and if the run started anything, does its **when done** action. A
   scheduled run that has not started anything yet stays open until its stop
   time, so downloads added during the window still run.

A stopped queue's running downloads are canceled and their `canceled` event
becomes `waiting`, so they stay in the queue with their partial file and
sidecar. Starting one again resumes that file under the same id. After a
restart, a queue's downloads that were running wait in it again, a queue
that was started carries on, and a schedule whose window is still open
starts its queue. The daemon does not exit when idle while a queue is
started, a schedule has a run ahead, or a countdown runs. Without Start with
Windows nothing starts the daemon, though; see Known gaps.

Sleep and hibernate use `SetSuspendState`; shutting down runs
`shutdown.exe /s /t 0`. The daemon saves the history as the countdown starts.
With the environment variable `UIDM_POWER_DRY_RUN` set, the daemon only logs
what it would do, which is how these were tested.

`refresh_link` with a `download` gives download `id` (failed, canceled,
interrupted or waiting) that request as its new link. If the download had
started, the daemon probes the new link first and compares it with the
partial file's sidecar; for another file it replies `refresh_mismatch` and
changes nothing, unless `restart` is true. A waiting download keeps waiting
with its new link; any other starts again (as `retry` does, with a new id).
Without `download`, the daemon waits for the browser: it broadcasts
`refresh_waiting`, and the extension sends its next captured download as that
`refresh_link`, whatever the capture rules say. `cancel_refresh` stops
waiting; `refresh_waiting` with `id: null` says nothing waits.
`add` may carry `browser_proxy`, the browser's proxy setting, used when the
`use_browser_proxy` setting is on: `mode` (`direct`, `fixed`, `pac`,
`auto_detect` or `system`), the fixed proxies `http`, `https` and `socks`
(each `scheme://host:port`), the configuration script's `pac_url` or its
text `pac_script`, and the `bypass` list. Older extensions send only
`proxy`, the fixed proxy, which `browser_proxy` replaces when both come.

## Proxies and logins

The settings hold `proxy_mode` (`none`, `system`, `script` or `manual`), the
manual `proxy_http`, `proxy_https`, `proxy_ftp` and `proxy_socks5` (each
`host:port`), each one's user name (`proxy_username` for HTTP, then
`proxy_https_username`, `proxy_ftp_username` and `proxy_socks_username`),
`use_browser_proxy`, `browser_proxy_after_errors` and `site_logins` (`site`
and `username`). Passwords are write-only: `proxy_password`,
`proxy_https_password`, `proxy_ftp_password`, `proxy_socks_password` and each
login's `password` travel in `set_settings` only when changed, and the daemon
keeps them outside the settings file and never sends them back. Proxy
passwords are encrypted with DPAPI in `%LOCALAPPDATA%\UIDM`: `proxy.bin` for
HTTP, `proxy-https.bin`, `proxy-ftp.bin` and `proxy-socks.bin`. A settings
file from before each proxy had a login of its own gives its shared login to
the HTTPS and SOCKS proxies, which used it, on the daemon's first start. Login
passwords are generic credentials named `UIDM/login/<site>` in Windows
Credential Manager, removed with their login.

**Proxies.** The system proxy is read from the Internet settings in the
registry each time a download starts: `ProxyServer` (one proxy, or
`http=…;https=…;ftp=…;socks=…`) and the `ProxyOverride` bypass list, including
`<local>`, and its configuration script or automatic detection (WPAD), which
WinHTTP runs to pick the proxy per address. **Get System** in Options copies
the fixed part of that setting (the proxies and the bypass list, not the
script) into the manual fields, the FTP proxy only when Windows names one
with `ftp=`; Windows' SOCKS proxy speaks SOCKS4. The
manual HTTPS proxy is the proxy for `https://` URLs (an HTTP proxy reached
with `CONNECT`; empty means the HTTP one, with its login). A SOCKS4 or
SOCKS5 proxy carries the `http://` and `https://` URLs that have no proxy of
their own, and with `proxy_socks_https` every `https://` URL even when an
HTTP or HTTPS proxy is set. `ftp://` downloads go through the FTP proxy:
UIDM logs in to it first with its own login, when it has one (`USER`,
`PASS`), then as `user@host`. Without an FTP proxy they go through the SOCKS
proxy (`proxy_socks_ftp`, on by default): UIDM opens both the control and
the data connections with a SOCKS CONNECT itself (SOCKS4, SOCKS4a or SOCKS5,
with the SOCKS login), and always in passive mode, since the server cannot
connect back through the proxy.
A proxy's login goes to the proxy only (`Proxy-Authorization`, or the
SOCKS handshake). UIDM follows redirects itself, dropping `Cookie` and
`Authorization` on every hop, so a site's cookies and logins never reach
another server. With
`use_browser_proxy`, the extension reports the browser's whole proxy
setting (`chrome.proxy`, Firefox's `browser.proxy`) with each download, and
it wins for downloads that carry one: no proxy, its fixed HTTP and SOCKS
proxies (an HTTPS or QUIC proxy, which the engine cannot speak, is left
out), its configuration script (WinHTTP fetches one at an address; one the
browser was given as text is served to WinHTTP from a loopback-only address
of the daemon's), automatic detection (WPAD), or the system proxy, as
**Use system proxy settings** reads it. The bypass list goes along:
Chrome's `<local>`, `*.corp.net` and `host:port` entries and Firefox's
`.corp.net`; address ranges have no counterpart and are dropped. Without
WinHTTP (macOS, Linux) a script or detection cannot run, so those downloads
connect directly. With `browser_proxy_after_errors` too, a
download goes through UIDM's own proxy setting first; when it fails, the
daemon retries it once through the browser's proxy, and that address keeps
using it until the daemon restarts.

**Logins.** A site matches the way capture rules do: `example.com` covers its
subdomains, and `*` works within a name. A path after the site limits the
login to it: `example.com/private/*` matches with `*` across the whole path,
and `example.com/private` covers that folder and everything below it, both
ignoring case. The longest matching pattern wins, so a folder's login beats
its server's. A download's own login (from **Log in to the site** or its
properties) replaces the site logins for its host. A request goes out
without credentials. On a 401, UIDM picks the login for the address that
answered and sends the request again, once, straight to that URL,
with `Authorization` for the `WWW-Authenticate` challenge: Digest (MD5,
MD5-sess, SHA-256, with `qop=auth`) when offered, else NTLM, else Basic.
NTLM (offered as `NTLM`, or as `Negotiate`, under which Windows servers take
NTLM messages too) is a handshake on one connection: a negotiate message,
the server's challenge in its 401, then an NTLMv2 answer, which proves the
password without sending it, so plain http is fine. A user name written
`DOMAIN\user` gives the domain. Each segment's connection does its own
handshake. Signing in as the current Windows user without a saved login is
not supported. Basic over
plain http, and any FTP login, needs a login allowed on plain connections
(a download's own login always is); otherwise the download asks for one,
so a server offering only Basic cannot draw out a password saved for
https. `Authorization` is never carried across a redirect, so a login
never reaches another host. No login for the host fails with "This site needs a login: <host>",
and a refused login with "The login for <host> was refused"; neither is
retried.

## Streams, batches and the site grabber

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
asks UIDM (`probe_media`) what each stream offers. The button hides when, at
five points of the player, what is on top lies outside the player's box
(its furthest ancestor not much bigger than it, so a custom player's
controls and overlays do not count), when `document.hidden`, and when
something is full screen; it looks again on scroll, resize and once a
second. Tabs open before the extension was installed, updated or turned
back on get the content script injected (`scripting.executeScript`, every
frame that lacks it) on `runtime.onInstalled` and on the first connection
to UIDM in a browser session. A content script can only pick from what the worker saw
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
`webNavigation.onHistoryStateUpdated`). The daemon sends only these
addresses to yt-dlp (`ytdlp::supports`); any other goes to the stream
reader. `probe_media` on such an address runs
`yt-dlp -J` and offers one video per height (H.264 first, then the highest
bit rate) and the sound per language. The download runs yt-dlp with the
chosen formats and `--ffmpeg-location`, so ffmpeg joins them into an `.mp4`;
the sound alone is `-x --audio-format mp3|m4a`. UIDM reads yt-dlp's progress
lines, and canceling ends yt-dlp and its children (`taskkill /T` on
Windows); its `.part` files let a retry carry on. Without ffmpeg the best
format with both video and sound in one file is saved, with a note. Live
streams are refused. UIDM does not send the browser's cookies to yt-dlp, so
only videos anyone can watch signed out are offered (on Facebook and
Instagram that is public posts only).

**HLS.** UIDM reads master playlists (qualities, separate audio tracks) and
media playlists (segments, `#EXT-X-BYTERANGE`, `#EXT-X-MAP` headers,
`#EXT-X-MEDIA-SEQUENCE`). Segments encrypted with AES-128 are decrypted with
the key from the playlist's key URL, fetched once, and the IV from the
playlist or the segment's sequence number. Any other key system
(`SAMPLE-AES`, or a `KEYFORMAT` other than `identity`, as FairPlay and
Widevine use) is DRM and refused, as is a live playlist without
`#EXT-X-ENDLIST`. Subtitle tracks (`#EXT-X-MEDIA:TYPE=SUBTITLES`) in the
quality's `SUBTITLES` group are offered by name and language.

**DASH.** UIDM reads a static manifest's `SegmentTemplate` (with a fixed
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
whose root is `<manifest>` in the `ns.adobe.com/f4m` namespace). UIDM reads
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

**Downloading.** Segments go one file each into `<file>.uidmparts` next to
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

**The site grabber** runs in the daemon, so it uses the proxy settings, and
reports to the window that started it. It reads `robots.txt` once per site
(the group for `UIDM` if there is one, else `*`; longest match wins, with `*`
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
as `UIDM/grabber/<start host>/<user name>`, so projects on one site can use
different accounts (a password an older version kept under
`UIDM/grabber/<start host>` is still read), sent to the start site when it asks, and
given to the downloads on that site. A login page's cookies come from the
extension's `cookies_wanted` request at crawl start, as the start site's do
with **Use my browser's sign-in**, or else from the pasted ones, which
project files keep encrypted for the user (as saved requests are) and
templates never keep. Each site's cookies go only to pages on that site's
host (and its subdomains when the crawl explores them), never to other
hosts the crawl reads; the downloads it adds for files on the login site
carry that site's cookies too, as captured browser downloads do, and
files on other hosts get none. Named filters are kept in
`%APPDATA%\UIDM\grabber\filters.json` and named templates in
`grabber\templates\`. With scripts run, the extension closes every window
the page opens and reports its address; the crawl explores those only when
**Ignore the windows page scripts open** is off. A scheduled run sets the
project's when-done action, file and force-close choice on its queue for
that run only (`when_done_once`, `open_when_done_once`, `force_close_once`),
so the queue's own settings come back afterwards; a run that adds nothing
does them at once. With a stop time it also sets `stop_at_once`, and the
scheduler stops that queue run (its downloads wait again, progress kept)
once the time passes.

## Notifications and virus scans

Windows only shows notifications from an app it knows, so at start the daemon
registers the app id `UIDM.DownloadManager` under
`HKCU\Software\Classes\AppUserModelId` with UIDM's name and an icon it writes
to `%LOCALAPPDATA%\UIDM\uidm.png`. A copy started with `UIDM_INSTANCE=test`
registers `UIDM.DownloadManager.test` with the name `UIDM (test)` and the icon
in its own data folder instead, so it leaves the installed copy's entry alone.
Each button carries an argument such as
`open:12`, which the daemon acts on while it runs. A failure is announced one
second late, and only if the download is still failed then, so a download the
extension hands back to the browser is not announced. A file with a threat
is never opened from a notification; its Open button brings up the window,
which asks first.

The scanner runs as a program with separate arguments, never through a shell.
A custom command line is split into arguments first (double quotes group;
backslashes are literal) and `{path}` is replaced afterwards inside each
argument, so a file name cannot add arguments or run anything, whatever it
contains. Defender runs as `MpCmdRun.exe -Scan -ScanType 3 -File <path>
-DisableRemediation` and reports a threat with exit code 2. A custom scanner
reports a threat with exit code 1 (ClamAV's convention). Any other exit code
is a failed scan, as is one that runs over 10 minutes.

## Layout

| Path | What it is |
| --- | --- |
| `crates/` | The workspace, one crate per layer of `AGENTS.md`: `domain`, `application`, `infrastructure`, `ui`, `app` |
| `crates/app/src/bin/uidm-gui.rs` | Desktop window: builds its adapters and starts `uidm-ui` |
| `crates/app/src/bin/uidm-host.rs` | Native-messaging relay, `send` for other programs, and CLI test mode |
| `crates/app/src/bin/uidm-daemon.rs` | Daemon: server, auth, download list, retry/remove, idle exit |
| `crates/infrastructure/src/state.rs` | `DownloadTable`, kept by both the daemon and the window |
| `crates/infrastructure/src/settings.rs` | Settings, their defaults, validation and file |
| `crates/infrastructure/src/history.rs` | Download history and saved requests on disk |
| `crates/infrastructure/src/dpapi.rs` | DPAPI encryption for saved requests |
| `crates/infrastructure/src/registry.rs` | Start with Windows and the notification app id |
| `crates/infrastructure/src/browsers.rs` | The browsers found, and registering the host with each |
| `crates/infrastructure/src/notify.rs` | Windows notifications and sounds |
| `crates/infrastructure/src/scan.rs` | Virus scans of finished downloads |
| `crates/infrastructure/src/queue.rs` | Queues, schedules and their calendar arithmetic |
| `crates/infrastructure/src/power.rs` | Sleep, hibernate and shut down |
| `crates/infrastructure/src/net.rs` | Proxies, site logins, HTTP authentication, Credential Manager |
| `crates/infrastructure/src/media/` | HLS playlists, DASH and HDS manifests: qualities, segments, keys, DRM; yt-dlp pages |
| `crates/infrastructure/src/batch.rs` | URL patterns, pasted lists and the clipboard link rule |
| `crates/infrastructure/src/listfile.rs` | Exporting and importing the download list |
| `crates/infrastructure/src/grab.rs` | The site grabber's crawler and `robots.txt` |
| `crates/ui/src/` | The window's views, state and messages: the main window, its dialogs, tray icon, queue controls, tools (batch, import, export, grabber, clipboard) and Win32 helpers |
| `crates/infrastructure/src/ipc.rs` | Daemon address, start-up, token file, HMAC handshake |
| `crates/infrastructure/src/app.rs` | Request building and event mapping |
| `crates/infrastructure/src/protocol.rs` | Framing and message types |
| `crates/ui/src/i18n.rs`, `crates/ui/src/lang/` | The window's text in other languages, one JSON table each |
| `crates/infrastructure/src/engine/` | Queue, segments, resume sidecar, speed limiter, HTTP, streams (`stream.rs`) |
| `extension/` | MV3 extension: capture, context menu, popup |
| `extension/_locales/` | The extension's text in each language |
| `extension/firefox.json` | What the Firefox copy changes in `manifest.json` |
| `extension/rules.js` | Capture rules (file types, minimum size, excluded sites) |
| `extension/links.js` | The links of a page for "Download all links" and "selected links" |
| `extension/post.js` | Matching downloads to the form POSTs that started them |
| `extension/request.js` | The request behind a download: headers, redirects, response, proxy setting |
| `extension/media.js` | Which responses are videos and streams, per tab, and the video menu |
| `extension/content.js` | The video button over playing videos |
| `extension/test/` | Node tests for the rules and the service worker |
| `assets/icon-64.rgba` | Window icon, raw RGBA made from `extension/icons/128.png` |
| `host/com.uidm.host.json` | Template of the native-messaging manifest |
| `install-host.ps1` | Builds, registers with Chrome, Edge and Firefox, adds the Start menu entry |
| `installer/uidm.iss` | Inno Setup script for `UIDM-Setup-<version>.exe` |
| `installer/build.ps1` | Builds the programs and the installer into `dist\` |
| `installer/make-firefox.ps1` | Makes the Firefox copy of the extension, or with `-Check` tells whether one is out of date |
| `installer/make-firefox.js` | The same for macOS and Linux, with `--check` |
| `installer/uidm.ico` | Setup and shortcut icon, packed from `extension/icons` by `make-icon.js` |

## Build

On macOS and Linux, a stable Rust toolchain is enough, plus on Linux the GTK 3
and OpenSSL development packages (`sudo apt install libgtk-3-dev libssl-dev`
on Ubuntu):

```bash
cargo test
cargo build --release
```

`installer/linux/package.sh` makes the `.deb`, `.rpm`, AppImage and
`.tar.gz` in `dist/` (it needs `dpkg-deb`, `rpmbuild` and Node, and
downloads `appimagetool` unless `$APPIMAGETOOL` names one);
`installer/macos/package.sh` makes the universal `.dmg` on a Mac. Name
formats to make only those: `installer/linux/package.sh --skip-build deb`.

Rust on Windows needs a linker. This project builds with the
`stable-x86_64-pc-windows-gnu` toolchain plus MinGW-w64 (WinLibs) on `PATH`.

```bash
cargo test
cargo build --release
node --test extension/test/*.test.js
```

To try a build next to the installed UIDM, give it a name in
`UIDM_INSTANCE`. That copy keeps its settings and list in `UIDM-<name>`
folders, runs its daemon on a port of its own, and shows the name in the
window's title. It does not end other daemons when it restarts its own.

```powershell
$env:UIDM_INSTANCE = "test"; .\target\debug\uidm-gui.exe
```

A string the window shows needs its German, Spanish and French translations
in `crates/ui/src/lang/`; a test fails for one without them.
`lang/untranslated.json` lists the strings that were untranslated before
that test came in, and an entry leaves the list once it is translated.

GitHub Actions builds and tests UIDM on Windows, macOS and Linux on every
push and pull request (`.github/workflows/build.yml`), and the extension's
tests with Node. Those runs keep no files.

Each run also fails on a clippy warning or on code that rustfmt would
change. Run both before pushing:

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
```

`rustfmt.toml` sets the line width to 120. Clippy checks only the code for
the platform it runs on, so a warning in a macOS-only or Linux-only block
shows up on that runner.

The installers come from `.github/workflows/release.yml`. Set the version in
`Cargo.toml`, commit, and push a tag that matches it:

```bash
git tag v0.2.0
git push origin v0.2.0
```

It builds every file in the table under [Install](#install) on Windows,
macOS and Linux runners and attaches them to a new GitHub Release for that
tag. Release files don't count against the repository's storage (each may be
up to 2 GB), and UIDM's update check reads the latest release. The build
files are kept as artifacts for one day only, to hand them from the
builders to the release. Starting the workflow by hand
(**Actions > Release > Run workflow**) builds the same files without a
release, as artifacts for a day.

To build the installer, install [Inno Setup 6](https://jrsoftware.org/isinfo.php)
(`winget install JRSoftware.InnoSetup`), then run:

```powershell
.\installer\build.ps1
```

It builds the release programs and writes `dist\UIDM-Setup-<version>.exe`,
with the version from `Cargo.toml`. Pass `-SkipBuild` when `target\release`
is already current.

The Rust tests also run on Linux under Wine, cross-compiled with
`cargo test --target x86_64-pc-windows-gnu` and
`CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine` (needs the `mingw-w64` and
`wine64` packages).

`cargo build --release` produces `uidm-host.exe`, `uidm-daemon.exe` and
`uidm-gui.exe` in `target\release`. The host and the window start the daemon
from their own folder, so keep the three together. The window renders through
OpenGL, which needs a working graphics driver.

To run one download without the browser, window or daemon:

```bash
cargo run --release --bin uidm-host -- download https://example.com/file.zip
```

### Adding a language

The window's strings are written in English in the code, and that English is
each string's ID: `tr("Save")`, or `trf!("{count} files", count = n)` for text
with values. A language is one JSON file in `crates/ui/src/lang/`, mapping each
English string to its translation, plus a line in `LANGUAGES` in
`crates/ui/src/i18n.rs`; a missing string shows in English. The tests check that
every key is still used and keeps its `{placeholders}`. The extension's text is
in `extension/_locales/<code>/messages.json`, as Chrome expects; its tests
check that every locale has the same messages as `en`. egui draws text without
shaping it, so scripts that need shaping, such as Bengali, Hindi or Arabic,
would not display correctly yet.

## Verified

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
  (without its tests) and the icon in `%LOCALAPPDATA%\Programs\UIDM`. It
  wrote a host manifest pointing at the installed `uidm-host.exe`, pointed
  the Chrome and Edge keys at it, and added the Start menu shortcut and the
  Installed apps entry. Started the way Chrome starts it (the manifest's
  path, the extension origin as argument), the installed host started the
  installed daemon, which downloaded a file intact. Uninstalling while that
  daemon ran closed it, then removed the folder, both keys, the shortcut, the
  Installed apps entry and the notification registration, leaving
  `%LOCALAPPDATA%\UIDM` alone. Installing over a running daemon closed it
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
  UIDM's settings.
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
  UIDM** adds with `start: false`. Form downloads: URL-encoded forms rebuild
  as the browser encodes them, multipart forms with a boundary of their own
  and without their file fields, and raw bodies byte for byte; forms that
  send a file and bodies the browser withheld stay in the browser; a
  download matches the latest POST to its URL within a minute; a form's
  download reaches UIDM as the same POST with its content type while a link
  next to it stays a GET; and a POST UIDM cannot start goes back to the
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
  Windows writes `"...\uidm-gui.exe" --minimized` to the Run key and removes
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
  download UIDM cannot start goes back to the browser; the context menu works
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

## Known gaps

- On macOS and Linux: the drop target and proxy configuration scripts
  (PAC and WPAD) are Windows-only; the system proxy comes from the
  `http_proxy`, `https_proxy`, `ftp_proxy`, `all_proxy` and `no_proxy`
  variables. Closing to the tray minimizes the window rather than hiding it.
  macOS notifications have no buttons. Passwords and saved requests are
  sealed with a key in UIDM's data folder that only your user can read,
  rather than the Keychain or the Secret Service. **Update now** opens the
  release page instead of installing. Microsoft Defender is Windows' own:
  choose a scanner program such as `clamscan`. These builds were tested on
  Linux; the macOS build is checked by the compiler and CI only.

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
- A form download that sends a file stays in the browser (the browser does
  not share the file), and a POST download cannot resume: a failure means
  sending the form again with Retry.
- Chrome does not let an extension cancel a download at its headers, so
  there it is still taken from the browser as it starts; Firefox cancels it
  at the headers.
- Streams: live streams, DASH manifests split into several periods (often
  ads), and HLS `SAMPLE-AES` without a key system are refused. Subtitles are
  not downloaded. The size shown for a quality is an estimate from its bit
  rate. Without ffmpeg, separate video and audio stay two files.
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
