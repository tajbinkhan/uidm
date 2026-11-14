# Install

Every release on the
[Releases page](https://github.com/tajbinkhan/UDM/releases) has a file for
each system:

| System | File | |
|---|---|---|
| Windows 10 and 11 | `UDM-Setup-<version>.exe` | the installer, below |
| | `UDM-<version>-windows-portable.zip` | a [portable copy](portable.md), everything in its own folder |
| macOS 11 or later | `UDM-<version>-macos.dmg` | Apple Silicon and Intel |
| Ubuntu, Debian, Mint | `udm_<version>_amd64.deb` | `sudo apt install ./udm_*.deb` |
| Fedora, openSUSE, RHEL | `udm-<version>-1.x86_64.rpm` | `sudo dnf install ./udm-*.rpm` |
| Any Linux | `UDM-<version>-x86_64.AppImage` | make it executable and run it |
| | `udm-<version>-linux-x86_64.tar.gz` | unpack and run `./install.sh` |

The Linux files need glibc 2.35 and OpenSSL 3 (Ubuntu 22.04, Debian 12,
Fedora 36 or newer) and GTK 3. The tray icon needs
`libayatana-appindicator3` (and on GNOME the AppIndicator extension);
notifications use `notify-send`. [Build](building.md#build) says how the files are made.

## Coming from Pullbay or UIDM

UDM (Unified Download Manager) was called Pullbay in version 0.4 and UIDM
up to 0.3.3. Install UDM over either one. The first time a UDM program
starts, it moves the old settings, download list and logs into UDM's
folders. It also replaces the old browser registrations and the
start-at-sign-in entry with UDM's. Half-finished downloads resume, and
site passwords saved in Pullbay still work.

- **Windows**: Setup closes Pullbay or UIDM, installs into a `UDM` folder
  beside the old one, and removes the old folder and shortcuts.
- **Debian, Ubuntu, Fedora**: the `udm` package replaces the `pullbay`
  and `uidm` ones. `install.sh` removes their copies in `~/.local`.
- **macOS and AppImage**: delete `Pullbay.app`, `UIDM.app` or the old
  AppImage yourself.

The browser extension has to be loaded again, because the old one went with
its folder. Remove the Pullbay or UIDM extension, then load UDM's as
described below. It keeps the same ID. In Firefox it has a new ID,
`udm@udm.local`.

## Windows

Run `UDM-Setup-<version>.exe`. It installs for your user only, without administrator rights, into
`%LOCALAPPDATA%\Programs\UDM`. It registers UDM with Chrome, Edge and
Firefox, adds
**UDM** to the Start menu (and, if you tick it, the desktop), and lists UDM
under **Installed apps**. Setup is not code-signed, so Windows SmartScreen
may warn before it runs.

To install from a source checkout instead, with the build toolchain described
under [Build](building.md#build), run this from this folder:

```powershell
.\install-host.ps1
```

That builds the programs if needed, registers UDM with Chrome, Edge and
Firefox, and adds **UDM** to the Start menu.

Either way, add the extension to the browser once. It is not in the Chrome
Web Store or on Firefox Add-ons yet. Once it is listed there, this page links
to it and installing it from the store takes the place of the steps below;
UDM then lets in both the store's copy and the one loaded by hand. Until then,
load it by hand:

1. Open `chrome://extensions` (or `edge://extensions`) and turn on
   **Developer mode**.
2. Click **Load unpacked** and choose the `extension` folder inside the
   install folder (Setup's last page shows it and can open it), or
   `extension/chromium` in this checkout.
3. Check that its ID reads `epmjakacaejhdhldkpbjnakhfmjjbhfm`. The manifest
   carries a fixed public key, so the ID is the same on every machine.

After updating, reload the extension on the extensions page. What it does
with each permission, and that nothing it reads leaves your computer, is in
its [privacy policy](privacy.md). It asks for
`webRequest` (to send downloads started by a form as the same POST, to send
UDM the browser's whole request, and to notice the videos a page loads),
`webNavigation` (to find the frame a link in an `about:blank` frame belongs
to), `downloads.ui` (to keep the download bubble hidden for a download UDM
takes) and `proxy` (to read the browser's proxy setting, which it never
changes), in addition to native messaging, downloads, cookies, context
menus, storage, scripting and notifications. A small
content script runs on every page for the video button; it does nothing
until a video plays.

For streamed videos to end up as one `.mp4`, install
[ffmpeg](https://ffmpeg.org/): **Install ffmpeg** in **Options > Tools**
downloads the official build the yt-dlp project makes and checks it against
its published checksum (on Windows and Linux), or put `ffmpeg.exe` on `PATH`,
or choose it in **Options > Tools**. Without it UDM still downloads
streams, as `.ts` files or as separate video and audio files.

Videos on YouTube, Facebook, Instagram, Vimeo and the other sites
[yt-dlp](https://github.com/yt-dlp/yt-dlp) reads need yt-dlp as well:
**Install yt-dlp** in **Options > Tools** downloads yt-dlp's own release
for your system and checks it the same way, and **Keep yt-dlp up to date**
updates it once a day. `winget install yt-dlp.yt-dlp` installs it with
ffmpeg and Deno too, which yt-dlp uses to read YouTube's player; Setup offers
to run that command for you when winget is there and yt-dlp is not. UDM
finds yt-dlp in its own tools folder, on `PATH`, next to UDM's programs
or next to ffmpeg. Restart UDM after installing it yourself, so it sees
the new `PATH`. What the buttons download, from where, and how it is checked
is in [Streams, batches and the grabber](streams-batches-and-grabber.md).

To remove UDM, uninstall it from **Installed apps**. That closes UDM if it
is running, removes the program, its browser registration, shortcuts, the
**Start with Windows** entry and the notification registration, and asks
whether to remove your settings and download list too. Installing a newer
setup over an older one closes UDM first and keeps your settings. With the
script, `.\install-host.ps1 -Uninstall` removes what it added. Downloaded
files are left alone either way.

**Updates.** UDM looks for a newer release on GitHub once a week (Options >
General can turn that off), and **About** (which also shows the version,
the copyright, and a support link to the issue tracker) checks on demand. When a newer
release has a Setup attached, **Update now** (in About, and next to the
notice in the status bar) has the daemon download `UDM-Setup-<version>.exe`
through your proxy settings, only from this repository's release downloads.
It checks the file's size, that it is a Windows program, and its SHA-256
against the release's signed checksums (below), saves it to the `update` folder in UDM's data folder and runs it with
`/fromudm=yes` (Setup also takes Pullbay's `/frompullbay=yes`). UDM then stops its downloads, saves its list and quits;
Setup closes whatever is left without asking again and replaces the files.
Stopped downloads resume from the list as usual. The notice's **×** hides it
for that version (remembered in `window.json`); a newer release shows it
again. **Update now** works the same way on macOS and Linux (see
[Updates on macOS and Linux](#updates-on-macos-and-linux)).

**Signed releases.** Each release carries `SHA256SUMS`, the SHA-256 of every
file in it, and `SHA256SUMS.minisig`, a [minisign](https://jedisct1.github.io/minisign/)
signature of that list by UDM's release key. UDM has the public key built in
(`crates/infrastructure/src/release-key.pub`) and installs nothing that the
signed list does not name with the same checksum: a release without the two
files, with a signature by another key, or with a file changed after signing
is refused, and About says why. To check a download by hand:

```sh
minisign -Vm SHA256SUMS -p release-key.pub
sha256sum --check --ignore-missing SHA256SUMS
```

### Updates on macOS and Linux

**Update now** downloads the file that matches how UDM was installed, checks
it as above, and puts it in place of the running copy. UDM then stops its
downloads, saves its list and quits, and the new copy starts a few seconds
later.

| Installed from | Update now |
| --- | --- |
| The macOS disk image (`UDM.app`) | Downloads `UDM-<version>-macos.dmg`, opens it, and puts its `UDM.app` where the old one was. You need permission to write there (in `/Applications`, an administrator's account). |
| The AppImage | Downloads `UDM-<version>-<arch>.AppImage` and replaces the AppImage file UDM runs from. |
| The `.tar.gz`, with `install.sh` | Downloads `udm-<version>-linux-<arch>.tar.gz` and runs its `install.sh`, which replaces `~/.local/lib/udm`. |
| The `.deb` or `.rpm` | Nothing: About says to update through your package manager (`sudo apt install ./udm_<version>_amd64.deb`, `sudo dnf install ./udm-<version>-1.x86_64.rpm`, with the files from the release page). |
| Anything else, such as a copy built from source | Nothing: the notice links to the release page. |

## macOS

Open the disk image and drag **UDM** to **Applications**. UDM is not
signed with an Apple Developer ID, so the first time, right-click it in
Applications, choose **Open**, then **Open** again (or allow it under
**System Settings > Privacy & Security**). The extension folders are inside
the app: in Finder, right-click UDM, **Show Package Contents**, then
`Contents/Resources/extension` (Chrome, Edge, Brave) or
`Contents/Resources/firefox`. In Chrome's **Load unpacked** dialog,
Cmd+Shift+G and `/Applications/UDM.app/Contents/Resources/extension` gets
there directly. Safari's extension comes inside the app; [Safari](#safari)
says how to turn it on.

## Linux

Install the `.deb` or `.rpm` with your package manager, or run the
AppImage, or unpack the `.tar.gz` and run `./install.sh` (it installs into
`~/.local` for your user; `./install.sh --remove` takes it away). UDM is
then among your applications, and `udm` starts it from a terminal. The
extension folders are `/usr/lib/udm/extension` and `/usr/lib/udm/firefox`
for the packages, `~/.local/lib/udm/...` for the archive, and inside the
AppImage (`./UDM-*.AppImage --appimage-extract` unpacks them).

## Browsers on macOS and Linux

There is no Setup to register UDM with the browsers, so the first time UDM
runs it registers itself with every browser it finds: Chrome, Edge,
Firefox, Brave, Chromium and Vivaldi (Opera reads Chrome's). Each browser
reads `com.udm.host.json` from its own `NativeMessagingHosts` folder, such
as `~/.config/google-chrome/NativeMessagingHosts` or
`~/Library/Application Support/Google/Chrome/NativeMessagingHosts`.
**Options > Browser capture** lists them to add or remove one later. When an
AppImage moves, UDM points the browsers at its new place the next time it
runs.

Then load the extension as described for Windows above, from the folder for
your system.

## Firefox

Firefox 128 or later loads the `firefox` folder, a copy of the extension with
its own manifest and the ID `udm@udm.local`. Firefox runs background scripts
where Chrome runs a service worker, and Chrome warns about Firefox's keys, so
the two cannot share one `manifest.json`. Setup and the macOS and Linux
packages install the `firefox` folder next to `extension`. In a checkout it
is `extension/firefox`, which `node installer/make-firefox.js` makes from
`extension/chromium` and `extension/firefox.json`. Git keeps it, so after
changing the Chromium extension run that command and commit both folders.
`node installer/make-firefox.js --check` (or
`.\installer\windows\make-firefox.ps1 -Check extension\firefox`) lists each
file that differs, is missing or is extra, and fails if there is any; the
extension's tests, CI and the package builds run it. Setup and `install-host.ps1` register UDM with
Firefox (under
`HKCU\Software\Mozilla\NativeMessagingHosts`, with a host manifest that names
that ID). Once the extension is listed on Firefox Add-ons, install it from
there: Mozilla signs that copy, so it stays installed when Firefox closes, and
it keeps the same ID. Until then, load it for each Firefox session:

1. Open `about:debugging`, choose **This Firefox**, then **Load Temporary
   Add-on...**, and pick `manifest.json` in the `firefox` folder.
2. In `about:addons`, open UDM's **Permissions** and allow **Access your
   data for all websites**. Firefox asks for this separately; without it the
   video button, the selection button, form downloads and **Download all
   links** do not work, though plain downloads are still caught.

Known gaps in Firefox:

- A temporary add-on is removed when Firefox closes, until the signed copy
  is listed on Firefox Add-ons.
- Firefox has no way to hide its download panel, as Chrome's `downloads.ui`
  does. It needs it less: it lets the extension cancel a download at its
  headers (`webRequestBlocking`, which only the Firefox copy asks for), so
  a download UDM takes there never starts in Firefox. One it only takes
  later (a link with a `download` attribute, say) may show the panel.

These were checked against a stand-in for the browser API in the extension's
tests, not in a running Firefox.

## Safari

Safari 16.4 or later on macOS gets the extension from UDM.app itself: the
disk image's app holds it as an app extension (`Contents/PlugIns/UDM
Extension.appex`), made by Xcode's converter from `extension/safari`, a copy
of the extension with its own manifest (`node installer/make-safari.js` makes
it from `extension/chromium` and `extension/safari.json`, and
`--check` tells whether it is current, as for Firefox). To turn it on:

1. Open UDM once from **Applications**, so macOS finds the extension in it.
2. UDM is signed ad hoc, not by a registered developer, so Safari hides the
   extension until you allow unsigned extensions: turn on **Show features
   for web developers** in **Safari > Settings > Advanced**, then choose
   **Develop > Allow Unsigned Extensions** (in Safari 17 and later it is also
   under **Safari > Settings > Developer**). Safari asks for your password,
   and turns it off again each time it quits.
3. In **Safari > Settings > Extensions**, tick **UDM**, then allow it on
   every website (**Edit Websites...**, or **Always Allow on Every Website**
   when Safari asks).

Safari has no downloads API, so the extension cannot take a download the
browser started. Instead it takes a link to a file as it is clicked, when
the file's type is in UDM's captured types; other links open as usual. The
context menu, **Download all links** and the popup work as in Chrome, and
the video button as far as Safari lets the extension see a page's requests.
Safari's native messages go to the app extension, not to a program Safari
starts, and the app extension cannot send the extension anything of its
own, so the extension keeps a request out that the app extension answers
when UDM has news. The app extension runs `udm-host` inside Safari's
sandbox, where it cannot start the daemon: UDM must already be running.
[Known gaps](known-gaps.md) lists the rest.

---

[← Back to the help pages](README.md)
