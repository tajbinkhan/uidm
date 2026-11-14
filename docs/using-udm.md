# Using UDM

- **Open the window** from the Start menu (**UDM**), or run
  `target\release\udm-gui.exe`. It lists every download with its size,
  progress, speed and time left. **Cancel** stops one and keeps the partial
  file; **Retry** resumes it; **Open** and **Show in folder** work on finished
  files; **Remove** takes a row off the list without deleting the file. Paste
  a URL at the top to add one. Everything works from the keyboard as well,
  and screen readers read the window; [Keyboard shortcuts](keyboard-shortcuts.md)
  lists the keys.
- **Download in the browser as usual.** While UDM is connected, the extension
  takes each download away from the browser and hands it to UDM, cookies and
  referrer included, so sign-in-protected files work. Right-click a link,
  image, video or audio and choose **Download with UDM** to send it directly.
- **The toolbar popup** shows whether UDM is connected, lists recent
  downloads, takes a pasted URL, and has a **Capture downloads** switch. With
  capture off, or while UDM is not running, the browser keeps its own
  downloads. At its top, **Capture in this tab** turns capture off for the
  tab it was opened over only, as IDM's toolbar button does: that tab's
  downloads, forced links and web players' files stay with the browser, and
  its toolbar icon turns grey with an "X" badge until it is turned on again
  or the tab closes. (IDM's button toggles on a click; UDM's button keeps
  opening the popup, which holds the rest of these features, so the switch
  is its first row.) Under **Downloading in the browser** it lists the browser's own
  downloads in progress (ones it kept, or started before UDM ran): **Send to
  UDM** cancels and erases the browser's copy and starts it in UDM, with its
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
  **Start** button) or **Cancel**. If the window is closed, UDM opens it; if
  no window shows up within 20 seconds, the download starts as proposed.
  Options can turn the dialog off, or skip it for some file types
  (**Start these types without asking**, such as `exe, msi`; the type is
  checked again once the server names the file). **Start downloading while
  the save-as dialog is open** fetches the file while you decide, into a
  folder of its own (`UDM early` in the Windows temporary folder), and the
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
  UDM** alike, until **Download later puts files in** in Options changes
  it. **Start the queue now** starts that queue as soon as **Download
  later** has put the download in it.
- **When a file name is taken**, UDM saves the new file as `name (1).ext`
  by default. Options (**If the file exists**) can make it overwrite the old
  file instead, or ask. With **Ask me**, the save-as dialog says the name is
  taken and offers **Save with a new name** or **Overwrite it**. This holds
  even with the dialog turned off: the download stops before writing anything
  and opens the dialog. If no window answers within 20 seconds, it gets a new
  name.
- **When UDM may not write to the folder** (one under `C:\Program Files`,
  say), the save-as dialog opens even when it is turned off and says so.
  Pick another folder with **Browse...**, cancel, or tick **Save there with
  administrator rights** (Windows only). With that ticked, the download
  still runs as you, in a staging folder (`UDM\elevated-<id>` in the
  temporary folder), and when it is done Windows asks for administrator
  rights and a helper (`udm-daemon --move-elevated`) moves the file into
  place. If you say No, the download fails with a message naming the
  finished file, which stays in the staging folder; starting it again asks
  again without downloading it again.
- **When the address is already in the list**, UDM asks by default: the
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
  with UDM** menu item add a download to a queue without opening a
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
  run. If UDM was not running at the start time, the queue starts as soon
  as it is, while the time window is still open. The Queues dialog says so
  next to a schedule until **Don't show this again** hides the note.
  **Wake the computer to start** (on by default, for queues and grabber
  projects) sets a Windows wake timer, so a sleeping or hibernating PC
  wakes 30 seconds before the start time, as IDM does. UDM must be
  running, and the power plan must allow wake timers; the schedule shows a
  warning with the setting's location when it does not.
- **When a queue is done**, UDM can do nothing, exit, or put the computer to
  sleep, hibernate or shut it down, per queue or for the current run only.
  Sleeping, hibernating and shutting down wait for a 60-second countdown in
  the window, which **Cancel** stops and **Shut down now** (or **Sleep
  now**, **Hibernate now**) skips. **Force apps to close** shuts down even
  when a program asks to save its work first. Failed downloads count as
  finished, unless the queue retries them.
- **The tray icon** keeps UDM at hand. Closing the window hides it there;
  click the icon to bring it back. Its tooltip shows how many downloads run
  and how fast, and its menu has **Open UDM**, **Add URL from clipboard**,
  **Add batch download from clipboard**, **Pause all**, **Resume all**, **Speed limit** and **Exit**. Exit closes the
  window; downloads keep going and the daemon stops on its own once idle.
  **Pause all** stops every queued and running download, keeping its partial
  file; **Resume all** starts them again, along with any interrupted ones.
  **View > Tray icon** picks the **Colored icon** or a **White icon** (a
  white silhouette of it, for a dark taskbar), or **Don't show**; the style
  changes at once and is kept with the window's layout.
- **Start with Windows** (in Options) starts UDM hidden in the tray when you
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
  completion dialog whatever Options says, then exit UDM, sleep, hibernate
  or shut down (after the same 60-second countdown a queue uses). It runs
  once, after the virus scan, and not for a download that fails.
  It opens whenever a download starts (**Options > General** can turn that
  off), behind the program you are in, and opens even while UDM is in the
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
- **When a download ends**, UDM shows a Windows notification: **Open** and
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
  web page; the extension hands that download to UDM as the new link instead
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
  for a login (HTTP Basic, Digest or NTLM), UDM sends the matching one to that
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
- **Debrid account** (experimental; **Options > Debrid**): links to file
  hosts such as Rapidgator and 1fichier go through a Real-Debrid, AllDebrid
  or Premiumize account and download from the fast direct link it gives.
  See [Debrid accounts](debrid.md).
- **FTP and FTPS**: `ftp://` and `ftps://` addresses download like web ones,
  over several connections and resuming where they stopped. `ftps://` is
  FTP over TLS from the first byte (port 990 unless the address names
  another). An `ftp://` server that will not take a plain login and asks
  for TLS gets `AUTH TLS` on a new connection (explicit TLS), and so does
  every later connection to it; one that does not ask stays plain. Over TLS
  the data connections are TLS too, the server's certificate is checked as
  for https, and a saved login goes even without its **Plain connections**
  box. UDM does not resume the control connection's TLS session on the
  data connections, so a server that insists on it (vsftpd's
  `require_ssl_reuse`) may refuse the transfers.
  **Cancel** leaves it stopped, with a **Log in...** button in its row. A
  login can also go with an address from the start: **Login** next to the
  URL bar shows user name and password fields for the addresses added
  there, and **Add many** has **Use a login** for every address in it.
- **Download this video**: a bar in IDM's style appears over a video playing
  in the browser, with **?** for help and **x** to close it for that video
  until the page loads again. It also appears over `<audio>` players and `<object>`/`<embed>` media players (an
  audio player gets it just above). It lists what that player loaded:
  direct video and audio files with their sizes, and streams (HLS `.m3u8`,
  DASH `.mpd`, HDS `.f4m`) with each quality UDM read from the playlist and its
  estimated size; on a feed with many players each button lists its own
  player's streams, and the page's whole list when it cannot tell. The
  button hides while its player is covered by something else on the page,
  while the tab is in the background or its window minimized, and in full
  screen. Pick one and UDM downloads it, named after the page (its
  `og:title`, else the tab's title, also for a player in a frame) without the
  site's name after it ("Cute cat - YouTube" becomes "Cute cat.mp4", on any
  site). A stream's segments are fetched in parallel
  and joined into one video (see [Streams](streams-batches-and-grabber.md)).
  Videos protected by DRM say so and offer nothing: UDM does not get around
  DRM. The popup turns the button off everywhere or on the current site;
  **Options > Videos and menus** has the same switch and the site list, the
  video and audio types the button lists (with **Check all**, **Clear all**
  and **Add**; M3U8 and MPD stand for streams), the smallest file it lists
  (128 KB by default), a full or mini look for it and for the button by
  selected links, and whether it shows on protected videos to say so.
  **Web players** (off by default) hands a web player's media of the listed
  types (MP4, M4V, WebM, FLV, MP3 and M4A at first) to UDM as soon as the
  page's `<video>` or `<audio>` loads it, without the button, as IDM's
  "automatically start downloading from web players" does: only whole
  files, not HLS or DASH streams, and each address once per tab.
- **Context menu**: the extension adds **Download with UDM**, **Download
  later with UDM**, **Download page with UDM** (on a page or a frame: that
  page's own address, or the frame's), **Download all links**, **Download
  selected links**, **Download the last video with UDM** (the video or audio
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
  reach UDM** (with the browser's reason when it gave one; a link then
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
  order), or export all, as a UDM list (`.json`, with names, folders,
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
  - sign in on a login page: **Open** opens it in your browser (UDM has no
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
  from a saved template or one that comes with UDM (pictures, audio,
  video, documents, or archives and programs from a site). A scheduled
  project explores only, explores and downloads the new files (starting the
  queue), or downloads the ticked files without exploring. Like a queue, it
  is one-time downloading or periodic synchronization, which also checks the
  files earlier runs found (or the ticked ones) and downloads again those
  changed on the server. It then exits UDM,
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
  **Add many** with every http, https, ftp and ftps link in the copied text, of any
  file type, to check and add.
- **Download all links with UDM** (the browser's menu on a page or a text
  selection, and the button over a selection) lists the links in the window
  to tick. As IDM does, it collects more than a page's links: its frames'
  addresses, addresses in its inline scripts (absolute ones, and quoted
  relative paths ending in a captured file type; the first 1 MB of script
  per page), the pictures images' onclick handlers open, its images (with
  their largest srcset picture), and the links in frames made from nothing
  (about:blank or srcdoc, up to three deep). **Download selected links**
  takes the same from what the selection touches, plus the addresses in its
  text, or in the text selected in a text box. Only http, https, ftp and ftps
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
- **Packages**: **Keep them together as a package** in the link picker
  keeps the ticked links as one package under a name (the archive's name is
  offered), saved in a folder of that name, inside the folder they would go
  to (the download folder when each would go to its category's). Ticked or
  not, the parts of each split archive among them (`show.part1.rar`,
  `show.part2.rar`; `show.rar` with `show.r00`; `backup.7z.001`; `site.zip`
  with `site.z01`) are kept as a package of their own. The sidebar lists
  packages under **Packages**, with how many of their files are in and how
  unpacking went; picking one lists its files. Its menu has **Unpack** (once
  every file is in), **Unpack with password...**, **Open unpacked folder** and
  **Stop keeping as a package** (the downloads stay). UDM unpacks zip
  (also with ZipCrypto or AES passwords), tar, `.tar.gz`, `.tar.xz`, `.gz`
  and `.xz` itself, and joins pieces such as `.zip.001`; 7z, RAR, bzip2 and
  spanned zips need 7-Zip (`7z`, `7zz` or `7za` on PATH, in Program Files,
  or where Options says). Encrypted archives ask for their password, which
  goes to 7-Zip on its standard input, never on its command line, and is not
  kept. Unpacking never replaces a file already there (the new one gets a
  numbered name); it refuses an archive with a file that would land outside
  the folder (`..`, a full path or a drive) or with a link, one of more than
  100,000 files, and one that would leave less than 64 MB free on the disk.
  Each archive is unpacked into a hidden folder first and moved into place
  once all of it came out, so one that fails leaves nothing behind.
- **Options** (top right of the window) has five tabs. **General**: the
  download folder, connections per file (1 to 16), downloads at once (1 to
  10), a total speed limit (with, while it is on, a limit for each file
  that has no limit of its own, **Always turn the speed limit on when
  UDM starts**, and **Only while I'm using the computer**, which lifts
  the limit after so many minutes (5 by default) without keyboard or mouse
  input and applies it again at the next one; the tray tooltip and a strip
  under the toolbar say while it is lifted, and downloads with a limit of
  their own keep it), a download quota (no more than so many MB every
  so many hours, with a warning at 90% that can be turned off), what to do when a file name is taken or an
  address is already in the list, the save-as
  dialog, whether to resume unfinished downloads when UDM starts, Start with
  Windows, whether UDM shows a tray icon (without one, closing the window
  quits and the Start menu opens UDM again), whether closing the window
  keeps UDM in the tray, whether minimizing the window hides it in the
  tray (on by default, as in IDM), where ffmpeg
  is, **Archives** (unpack a package as soon as all its files are in, off
  by default; unpack into a folder named after the package beside the
  archives, into the archives' own folder, or into a folder named after the
  package in a chosen folder; then send the archives to the Recycle Bin)
  and where 7-Zip is, the **Language**, the **Theme** (follow Windows, light, dark, high
  contrast, or a theme file of your own; see [Themes](themes.md)) and the
  **Interface size** (80% to 200%, scaling the whole window). Language, theme
  and interface size change as soon as you save. **More > Hide categories** hides
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
  manifest it names (`%LOCALAPPDATA%\UDM\com.udm.host.json`, or
  `com.udm.host.firefox.json` for Firefox), pointing at the `udm-host.exe`
  next to the window; unticking removes the key, and the manifest once no
  browser names it. Setup registers Chrome, Edge and Firefox. Vivaldi and
  Opera read Chrome's registration, so the list says so when it finds them
  instead of listing them. Restart a browser after a change: its
  **Restart** button asks first, then closes it the polite way (as closing
  its window does, so it keeps its tabs), ends what is left of it after 8
  seconds (Chromium keeps helper processes, and with "Continue running
  background apps" the browser itself), and starts it again. **Add
  browser...** adds any other browser by its program file: UDM tells its
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
  layout and font are kept in `%LOCALAPPDATA%\UDM\window.json`.
- **Help** (next to **More**): **Help contents** opens these help pages on
  GitHub, two more items open the pages on queues and the scheduler and
  on the site grabber, **Tutorials** opens this page, **Tip of the day** shows a tip under a "Do you know..." header
  with **Next tip**, and **Contact support** opens the issue tracker. A tip shows when the window
  opens unless its **Show tips on startup** box (also in **Options >
  General**) is cleared; the next start shows the next tip. **About** sits
  beside **Help**, and **More > Manage queues...** opens the queues even with
  the categories panel hidden.
- **Command line and other programs**: `udm-gui` takes IDM's switches (`/d URL`, `/p`,
  `/f`, `/a`, `/n`, `/q`, `/s`), `udm-cli` downloads a file with its progress on the terminal
  and lists, pauses, resumes and removes downloads, `udm-host send` takes links as IDM's
  `SendLinkToIDM2` does, and programs of your own can use the automation API.
  [Command line and automation](command-line.md) covers all four.
- **Languages**: the window comes in English, Bahasa Indonesia, German,
  Spanish, French, Italian, Dutch, Polish, Brazilian Portuguese, Vietnamese,
  Turkish, Russian, Ukrainian, Japanese, Korean and Chinese (Simplified and
  Traditional). It starts in the first system display language it has, else
  English; **View > Language** or **Options > General > Language** picks
  another. Numbers and dates follow the language (`2,5 MB` and `07.03.2026` in
  German). Chinese, Japanese, Korean and Vietnamese draw with a font of the
  system's (Microsoft YaHei, JhengHei, Yu Gothic, Malgun Gothic or Segoe UI on
  Windows, fontconfig's choice on Linux), read when the language is chosen;
  until then Options shows their names in English. Most tables were made by
  machine and wait for a native speaker's review
  ([Help translate](translating.md)). The extension follows the browser's
  language, with the same set. Messages from the daemon (errors,
  notifications) are still in English. Right-to-left languages and scripts
  that need shaping are not offered yet ([Known gaps](known-gaps.md)).

Files are saved to your Downloads folder, sorted by category, unless Options
says otherwise.
Closing the browser or the window does not stop a download. The list survives
restarts: downloads that were running when UDM stopped show as
**Interrupted**, and **Retry** resumes them from their partial file.

---

[← Back to the help pages](README.md)
