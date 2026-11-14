# Privacy policy of the UDM browser extension

This is the privacy policy of UDM's browser extension, the one for Chrome,
Edge and other Chromium browsers and its copy for Firefox. It covers what
the extension reads in the browser, where that goes, and why it asks for
each permission. It was last changed on 14 November 2026.

## In short

- The extension collects nothing for UDM's makers or anyone else. It has no
  analytics, no telemetry, no advertising, no accounts and no remote server
  of its own.
- What it reads goes to one place only: the UDM program on the same
  computer, through the browser's native messaging (the host
  `com.udm.host`, which relays to UDM's background service). Nothing goes
  over the internet from the extension itself.
- It sends UDM a download only when UDM is running and capturing downloads,
  or when you ask it for something (a page's links, a video, a site's
  cookies for a site grab).
- Downloads from private (incognito) windows stay in the browser unless you
  turn on **Capture downloads in private windows too** in UDM.

UDM itself is a download manager that runs on your computer. It downloads
what the extension hands it from the sites it came from, and keeps its
download list in its data folder on your computer. Its source, this policy
included, is public at <https://github.com/tajbinkhan/UDM>.

## What goes to UDM, and when

| What | When | Why |
| --- | --- | --- |
| A download's address, its file name and size, the page and frame it came from, and the request the browser made for it (method, headers including cookies, a form's fields, redirects and the server's response headers) | When the browser starts a download that UDM's capture rules take, or when you choose **Download with UDM** | So UDM can download the same file the browser would have, as the same signed-in request |
| The cookies the browser holds for a download's or a page's address | With a download, and when UDM's site grabber asks for a site you are signed in to | So the server gives UDM what it gives the browser |
| The addresses of videos and streams a tab plays, and the page they play on | When you use the video button or a video item of the right-click menu | So UDM can offer the video's qualities and download it |
| A page's links, or the links in what you selected | When you choose **Download all links with UDM** or **Download the selected links with UDM** | For UDM's link picker |
| A page's address | When you choose **Download gallery with UDM** | So UDM can read the page's pictures |
| A page's HTML, opened in a background tab and closed again | When UDM's site grabber asks for a page that needs the browser to draw it | So the grabber sees the page as the browser shows it |
| The browser's proxy setting | With a download | For UDM's option to use the browser's proxy |
| The browser's user-agent string | With a download or a list of links | So servers answer UDM as they answer the browser |

UDM keeps a download's request, cookies included, while the download can
still be resumed or retried, encrypted so that only your user account can
read it. Its download list itself never holds cookies or request headers.

## What stays in the browser

- In the browser's own extension storage: whether capture is on, whether to
  show the notice when UDM cannot take a download, the keys that skip or
  force capture, and the tabs where you turned capture off for the session.
- In memory only, for a short while: the requests of pages and players for
  about a minute, and the videos a tab played until the tab closes or moves
  to another page, to match a download or a video to the request behind it.
  These include the bodies of the last 50 requests that sent one (forms,
  and a page's own `XMLHttpRequest` and `fetch` calls) for about a minute,
  so a download a form started can be sent again as the same POST. A body
  can hold what was typed into the form.
- In memory only, while the browser runs: UDM's download list and settings,
  as UDM sends them, for the toolbar popup and the capture rules.

Nothing else is kept, and none of it leaves the computer. A request body is
sent to UDM only with the download it started.

## Permissions

Each permission the extension asks for, and what it uses it for. The
Chromium extension and the Firefox copy ask for the same ones, except where
noted.

| Permission | What the extension does with it |
| --- | --- |
| `nativeMessaging` | Talks to UDM on the same computer through the native-messaging host `com.udm.host`. This is the only place anything the extension reads goes to. |
| `downloads` | Sees each download the browser starts, cancels and removes the browser's copy of one UDM takes, hands a download back to the browser when UDM cannot take it, and lets the toolbar popup hand UDM a download the browser already started. |
| `downloads.ui` (Chromium only) | Hides the browser's download bubble while UDM takes a download, so the browser does not show a download it is not doing. Firefox has no such permission. |
| `cookies` | Reads the cookies for a download's address (and for a site UDM's grabber signs in to), from a private window's own cookie store only when private capture is on, to send with the download to UDM. It never sets or changes a cookie. |
| `contextMenus` | Adds **Download with UDM**, **Download later with UDM**, the links, gallery and video items to the right-click menu. |
| `storage` | Keeps the extension's own few settings listed above. |
| `webRequest` | Reads the requests of pages and frames, and the requests of video players, to rebuild a form's POST download, to send UDM the browser's whole request, and to notice the videos and streams a page loads. It does not change any request. |
| `webRequestBlocking` (Firefox only) | Lets the extension cancel a download UDM takes when its response headers arrive, so Firefox never starts it. It is used for nothing else. |
| `webNavigation` | Finds the frame a link in an `about:blank` frame belongs to, for the right referrer, and notices when a video site moves to another video without loading a page. |
| `proxy` | Reads the browser's proxy setting for UDM's option to use the browser's proxy. It never changes the setting. |
| `scripting` | Runs the extension's own scripts in a page when you ask for its links or selection, adds the video button to tabs already open when the extension starts, and reads a page's HTML in a background tab for UDM's site grabber. |
| `notifications` | Says when UDM could not be reached or could not take a download, with a button to stop the notice. |
| Host permission for all sites (`<all_urls>`) | Downloads, videos and links can come from any site, so the permissions above need every site: `webRequest`, `cookies` and `scripting` work only on the sites an extension may access. A small content script on every page shows the video button over a playing video and a **Download with UDM** button over a selection that holds links, notes the skip and force keys for a link you click, and, when a form with file fields is sent, tells the extension which fields held files (never the files), so a download the form starts can be rebuilt. It sends nothing until one of these happens. |

The extension loads no code from the internet. Its only link to a web page
is the video button's **?**, which opens UDM's help page on GitHub when you
click it.

## Changes

A change to this policy is made in the public repository, where its history
is kept: <https://github.com/tajbinkhan/UDM/commits/main/docs/privacy.md>.

## Contact

Ask questions or report a problem at
<https://github.com/tajbinkhan/UDM/issues>.

---

[← Back to the help pages](README.md)
