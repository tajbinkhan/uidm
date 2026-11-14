# What the extension does and does not take

A `magnet:` link you click goes to UDM, which downloads its torrent, while UDM is connected and
**Capture downloads** is on (and the tab captures, and it is not a private window unless UDM lets
those in). The skip keys, or a middle click, leave it to the
browser, which opens it with the program it gives magnet links to. This is so in Safari too.

It takes a download from the browser only when all of these hold:

- UDM is connected and **Capture downloads** is on;
- the URL is `http` or `https` (`blob:` and `data:` downloads only exist inside
  the browser);
- it is not from a private window, so private downloads never enter UDM's
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
    the size (often it does not yet, and then only the type and site count).
    A `.torrent` file (by its name or its type, `application/x-bittorrent`)
    skips the type list and the minimum size while **Take .torrent files as
    torrents** is on: UDM fetches it and downloads its torrent
    ([Torrents](torrents.md));
  - neither its URL nor the page it came from is on an excluded site.
    `example.com` and `*.example.com` both cover the site and every subdomain;
  - its URL matches none of the excluded addresses, where `*` matches any
    run of characters and an address without `http://` or `https://`
    matches both. UDM starts with a short list of site sounds and browser
    and Windows update files that sites fetch on their own (taken from IDM's
    own list). Remove any you want captured; **Restore defaults** adds them
    back and keeps your own;
- **Skip next download** in the popup was not pressed; it lets exactly one
  download stay in the browser;
- the skip keys were not held while clicking its link (Alt by default), nor
  while its tab started it some other way (a form sent, a page's script).

Holding the force keys (Ctrl by default) while clicking a link sends it to
UDM whatever the rules say, instead of opening it. Each is any mix of Ctrl,
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
(`webRequestBlocking`), so the browser never starts it and UDM gets it at
once. Chrome lets extensions like this one only look, not cancel, so there
the download still appears in the browser and is taken from it at once
(`downloads.onCreated`), with the size, type and name the headers gave
(the download item often has none yet); the download bubble is hidden from
the moment the headers say UDM takes it (`downloads.setUiOptions`) until
the browser's copy is canceled and erased, and comes back after ten seconds
whatever happens. A tab another tab opened a moment before only for the
download (it still shows nothing or the download's address, and never
showed a page) is closed once UDM takes the download.

**The whole request.** The extension follows the requests of pages and
frames: the headers the browser sent (`onSendHeaders`, with Chrome's
`extraHeaders` so nothing is hidden), the redirects it followed, and what
the response said. A download goes to UDM with them: `request_headers`
(without `Cookie`, which goes as `cookies`), `redirects`, `size` and
`content_type`. The daemon repeats the headers that are safe to: not the
ones about the connection (`Connection`, `Keep-Alive`, `TE`, `Upgrade`,
...), not `Host`, `Content-Length`, `Range`, the conditional `If-*` ones or
`Accept-Encoding` (the engine asks for ranges and decompresses what it asks
for itself), not `Authorization` (logins are UDM's), and not `Cookie`,
`Referer` and `User-Agent`, which come in their own fields. It keeps at most
64 headers, 20 redirects and values of 8 KB. The size and type are shown
until the server is asked, and are all a form download has before it
starts. The referrer is the document that started the download; a frame
made from nothing (`about:blank`, `about:srcdoc`) is skipped for the
nearest frame above it with an address (`webNavigation.getAllFrames`), else
the page.

A download a form started, with a POST, is sent to UDM as the same POST.
The extension records the method and body of recent form submissions in
frames and of scripts' requests (`webRequest`, `requestBody`) and matches a
new download to the request it came from, else the latest form to the same
URL in the last minute (a script's XMLHttpRequest or fetch is matched only
as itself, as when a web player's file is taken). It rebuilds URL-encoded
forms, multipart forms (with a boundary of its own) and raw bodies such as
a script's JSON. The browser does not share a multipart form's files, so
the page tells the extension which of its fields are files as the form is
sent: they are left out, and a form that sends a chosen file stays in the
browser, as does a body the browser did not share. UDM sends the POST once
and saves the response as one stream; it is not resumable, and it is not
repeated on a failure, since a form may not be safe to send twice.
Downloads without a recorded POST are fetched with GET as before.

If UDM takes a download but cannot even start it, for example because the
server refuses the request, the extension removes it from UDM's list and
hands it back to the browser, so it is never silently lost. A POST goes back
as a POST with its form, when the form is text.

Either way the user hears of it: a browser notice, "UDM could not take over
the download", names the file and why (UDM's error, or that UDM is not
running or not connected, for a download the rules would have taken). Its
**Don't show again** button turns these notices off for good; Firefox's
notices have no buttons, so there a click on the notice does it.

---

[← Back to the help pages](README.md)
