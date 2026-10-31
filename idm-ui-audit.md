# IDM UI and defect audit, 2026-10-31

## How this was done

- **Static.** REA `inspect_pe_resources` on `C:\Program Files (x86)\Internet Download Manager\IDMan.exe` (IDM 6.43, SHA-256 `75ac33a7…2145285`). Evidence `ev_2a983598…11647`, 436 resources, complete coverage. A local script decoded these from the file ranges REA reported:
  - all 105 dialog templates, with every control's kind, id, text and rectangle;
  - the main menu (MENU 129) and the grabber menu (MENU 256);
  - the three toolbars (TOOLBAR 166, 260, 485);
  - the string tables.
- **Runtime.** Screenshots of the IDM main window and of `uidm-gui.exe`, both at 814x465.
- **Code.** Three reviews of UIDM's code, covering the main window, the dialogs, and defects. Every file:line below was read. The defects marked confirmed were checked again against the code.

Tags used below:
- **obs** means seen in the decoded resources or a screenshot.
- **inf** means it comes from general knowledge of IDM 6.x.
- **H** means visible on first launch. **M** and **L** are medium and low.

## Scope of "exact copy"

UIDM can match IDM's layout, menu tree, toolbar order, columns, dialog controls, wording and sizes.

UIDM will not copy IDM's icons, logo, bitmaps or product name. They are Tonec's artwork and trademarks. Draw new icons with the same meaning and size: 56x42 toolbar buttons, 16x16 file-type and tree icons.

## Main window

IDM [obs] has, from top to bottom:
- a native menu bar: Tasks, File, Downloads, View, Help, Registration;
- a toolbar of big icons with a label under each;
- a "Categories" tree pane with a close X;
- a details list with grid lines;
- no status bar.

UIDM, all references in `src/bin/uidm-gui.rs` unless another file is named:

| Area | IDM | UIDM now | Sev |
|---|---|---|---|
| Title | "Internet Download Manager 6.43" | "UIDM", no version (107) | H |
| Menu bar | MENU 129, with Alt mnemonics | none. Commands are spread over More (3591-3652), Help (`gui/help.rs:68`), About and Options buttons, row buttons, the status bar and the tray | H |
| Toolbar | Add URL, Resume, Stop, Stop All, Delete, Delete Completed, Options, Scheduler, Start Queue (dropdown), Stop Queue (dropdown), Grabber, Tell a Friend. Buttons are 56x42 and gray out based on the selection | URL field plus Download, Later, Login, More, Help, About, Options text buttons (3546-3666). No Resume, Stop, Stop All or Delete | H |
| Top row width | n/a | The field takes `available_width - 360` (3551), but the buttons need about 470 px. Options is never visible, and About is cut to "Abo" at 814 px | H |
| Categories | Tree: All Downloads (Compressed, Documents, Music, Programs, Video), Unfinished, Finished, Grabber projects, Queues, each with an icon. Has a "Categories" header with an X, and its splitter can be resized | flat list (3810-3831), shown only after settings load (3794), so a disconnected GUI has no pane. Fixed 170 px wide. No Unfinished, Finished or Grabber projects | H |
| Columns | File Name, Q, Size, Status, Time left, Transfer rate, Last Try Date, Description | File, Size, Progress, Speed, Time left, Status (`gui/columns.rs:49-56`), plus a tick column (4590) and an action-button column (4607-4682). No Q column | H |
| Cell text | Size is the total. Status reads "Complete" | Size is "done / total" (4776). Status reads "Done" (4804) | M |
| Rows | about 17 px, grid lines, file-type icon, Shift and Ctrl multi-select, right-click menu, double-click on the whole row | about 32 px striped `egui::Grid` (3910-3913), no lines, no icons, no selection (ticks only), a per-row More button instead of a context menu (4687-4733), double-click on the name only (4761) | H |
| Header | sort glyph at the right edge, 2 states [inf] | "▲/▼" appended to the text, 3 states (`gui/columns.rs:164-189`) | M |
| Empty list | headers still shown [inf] | centered text with no headers (3902-3907) | H |
| Status bar | none | connection state, counts, Clear finished, speed toggle (3680-3779). A long error pushes Reconnect off-screen (3691-3694) | H |
| Fonts | Tahoma or Segoe at 8 pt, about 11 px | Segoe UI at 13.5 px, 24 px interact height (`gui/theme.rs:96-107`) | M |
| Keys | Del, Ctrl+A, Enter, Alt mnemonics [inf/obs] | only Ctrl+F, F3, Shift+F3 and Esc (`gui/find.rs:101-116`) | M |
| View menu items | Arrange files, Toolbar large or small, tray icon style, Font choose or reset | header-click sorting only, no toolbar, a tray show/hide checkbox, a text-size zoom | M |
| Help menu | Tutorials, Home page, Tell a friend, Check for updates | missing, except update checking, which is inside About | L |

### Mapping MENU 129 to UIDM

**Tasks**
- Add new download is the inline field. IDM opens dialog 148 instead.
- Add batch download is More > "Add many...".
- Run site grabber is More > "Site grabber...".
- Show drop target is in Options and the tray only.
- Export and Import are flat More items. IDM uses submenus.
- Exit is in the tray only.

**File** (acts on the selection)
- Stop Download is the row "Cancel" button. The name and the meaning differ.
- Remove is the row Remove buttons. Running rows have no Remove.
- UIDM has no multi-row form of any of these.

**Downloads**
- Pause All is in the tray only. Stop All has no equivalent.
- Delete All Completed is "Clear finished".
- Start queue and Stop queue appear only inside a queue's view (`gui/queues.rs:71-74`).
- Speed Limiter on and off is the status-bar toggle.

## Dialogs

### Global
- **H.** Every UIDM dialog is an `egui::Window` drawn inside the main window. IDM's progress, Options and Scheduler are separate OS windows. egui 0.31 can open real windows with `show_viewport_deferred`.
- **H. Options is a property sheet in IDM**, with these pages: General 139, File types 294, Downloads 291, Save to 143, Connection 141, Proxy/Socks 140, Sites Logins 145, Sounds 195.
  - Dial Up/VPN 185 is also a page, but it is out of scope.
  - Virus checking 494 is not a page. It opens from Edit... on page 291.
  - UIDM's Options has 6 tabs with different names, no page header strip, and Save/Cancel buttons. Its General tab is about 30 rows, far taller than IDM's 268x258 DLU page.
- **M.** IDM opens these as modal dialogs: Keys 229, Edit context menu 334/335, Panel 340, Customize 367, Advanced proxy 369, Virus checking 494, Minimum size 488, Exceptions 370 and 303, Category 204, Server exceptions 226, Site login 146. UIDM places all of them inline in tabs.

### Per dialog (most visible first)

**130 Download File Info (H)**
- Title "Save download" vs "Download File Info".
- The URL is a read-only label in UIDM. IDM uses an editable field.
- IDM has one Save As combo with history and a "..." button. UIDM splits it into "File name" and "Save to".
- Row order differs.
- Size and Preview sit in the wrong place.
- The remember checkbox has no group box.
- UIDM puts extra things inline: duplicate radios (IDM uses 298), file-exists radios, a queue combo (IDM uses 345), and an "N more waiting" line.
- Buttons: IDM is `Download Later | Start Download | Cancel`, each 74 DLU wide. UIDM is `Start download | Download later | Cancel` (2042-2266).

**103 Download progress (H)** (`gui/progress.rs:106-172`)
- The tabs match apart from names: "Speed limit" vs "Speed Limiter", "When done" vs "Options on completion".
- The button gap differs.
- The segment strip label is missing.
- The URL row has an extra "Address" caption.
- 365: the "Remember Speed Limiter settings..." checkbox is missing, and UIDM adds an extra Apply.
- 364: the Save To line and the Power options heading are missing, and a combo replaces the three checkboxes.
- 367 is inline, and "Show Hide tab buttons" is missing.

**148 Enter new address (H)**
- UIDM has no dialog, only the inline top bar.
- Missing: the address history combo, and the "Use authorization" group with fields disabled until it is ticked.

**308 Scheduler, 306 Schedule, 307 Files in the queue (H)** (`gui/queues.rs:243-448`)
- The title is "Queues", with no 306/307 tabs.
- Start now, Stop and Help are missing.
- Missing from 306: the One-time vs Periodic radios and the time pickers. UIDM uses "23:00" text.
- 307 has no file list with move up and down.
- 309, per-queue limits, is missing.

**280 Batch download (H)** (`gui/tools.rs:145-341`)
- UIDM uses a `[001-120]` pattern. IDM uses an asterisk with a "Replace asterisk to" group (Numbers or Letters, From/To, Wildcard size), and shows First, Second and Last file previews.
- Buttons are "Add N"/Cancel vs OK/Cancel.

**Site Grabber 239-284 (H)** (`gui/tools.rs:876-1427`)
- IDM is a wizard with a picture panel and `< Back | Next > | Close`. Its steps are project 257, explore 264, files 240, save 259, then results 241 as a tree plus a list. UIDM is one long form.
- Missing: the template combo on step 1, manual login page, "Don't open the logout page", "Add checked files... on closing", size unit combos, and One-time vs Periodic.

**140 Proxy/Socks and 369 Advanced (H)**
- IDM's manual mode is one row of Address, Port, User and Password, plus http/https/ftp protocol checkboxes, with an Advanced/Socks... button that opens 369. 369 has per-protocol groups, Socks 4/5 radios and Proxy DNS.
- UIDM has 4 rows with a combined `host:port`, no Port field and no per-protocol enable.
- FTP polarity is inverted. IDM's option is "PASV mode", UIDM's is "Active mode".

**337 Speed Limiter (H, meaning)**
- IDM limits **one file** and has "Always turn on Speed Limiter on IDM startup".
- UIDM's limit is a **total** limit, with no startup checkbox.

**Medium**
- **135 Properties.**
  - Title and labels differ: Link/Address, Referrer/Referer, Folder plus File name vs Save To.
  - IDM's icon and header line are missing.
  - The parent-page link is missing.
  - Login fields are hidden behind a checkbox.
  - Buttons are `Save | Close | Open` vs `Open | OK`.
- **139, 291, 143, 141, 145, 195, 494 Options pages.**
  - The browser list has no "Add browser...".
  - "Only add to the queue" sits above "Show start download dialog" in UIDM, the reverse of IDM's nesting.
  - Save to is a 6-column grid, not one category per combo.
  - The connection count is a number, not a combo.
  - "Use TLS 1.3" is missing.
  - Site logins have no protocol combo.
  - Sounds is 3 fixed rows.
  - The virus scanner has no separate parameters field.
- **229 Special keys.**
  - Master checkboxes, Del and the two notes are missing.
  - Key order is Ctrl, Alt, Shift vs IDM's Alt, Shift, Ctrl.
- **334/335 Context menu.** IDM has two per-browser sets of 4 items. UIDM has one set of 6.
- **340 Panel.**
  - "Don't capture downloads from web-players automatically" is missing.
  - Order differs, and there are no view preview images.
- **228 All links.**
  - Title differs.
  - Hide filters sit at the top in UIDM, at the bottom right in IDM.
  - "Edit..." is per row in UIDM. IDM has one button.
  - Buttons are "Add N" vs OK.
- **286 Complete.**
  - IDM has selectable Address and full-path fields. UIDM shows a truncated label and only the folder.
  - "Show in folder" vs "Open folder".
  - The checkbox is above the buttons in UIDM, below them in IDM.
- **298 Duplicate.** Not a dialog of its own. The URL field is missing, and the wording differs.
- **150 Shut down.** The progress bar is missing, and the title differs. 292 reboot has no counterpart.
- **209 Columns.**
  - UIDM uses a grid, where IDM has a checklist with Move Up, Move Down, Show, Hide, Reset and a width-in-pixels field.
  - Edits apply live, with no Cancel.
- **204 Category.** Missing: "files from these sites only", "Remember last save path", and IDM's OK/Cancel at the top right.
- **338/339 Delete.** IDM has two variants with Yes/No. UIDM has one, with Remove/Cancel.
- **100 About.** Missing: the Close button, the credits box, and the Home Site and Support Team captions.

**Low**
- **487/506 Find.** UIDM has a bar only, which is closer to 505.
- **105 Tip of the Day.** The heading and picture are missing.
- **296/297 Browser detection.** No counterpart.

**Excluded by design:** dial-up/VPN 184-186, IE integration, registration 138/300/144, problem reports 232/336, component download 190.

## Defects

Build: `cargo clippy --all-targets` reports 0 errors and 11 style warnings. `cargo test` passes 299 tests, with 2 ignored. The extension tests pass.

| # | Defect | Where | Status |
|---|---|---|---|
| 1 | GUI shows "could not prove it is the UIDM daemon". The cause is two copies of `daemon.token`: a daemon started from an MSIX-packaged app (here, the Claude desktop app) wrote it into the package's redirected `LocalCache`. The same can happen with Store Firefox launching the native host. | `src/ipc.rs` | confirmed (handshake replayed with both tokens) |
| 2 | Each failed connect truncates the running daemon's log with `File::create`. The daemon keeps writing at its old offset, so the log fills with NULs and the startup lines are lost. Use append mode. | `src/ipc.rs:263` | fixed in step 8 |
| 3 | A proof failure (`PermissionDenied`) is handled like "nothing listening". UIDM spawns a daemon that cannot bind, then retries for about 15 s. | `src/ipc.rs:102-125` | fixed in step 8 |
| 4 | The disconnect error does not wrap, so it pushes Reconnect off-screen. There is no "restart daemon" action. | `src/bin/uidm-gui.rs:3691-3694` | fixed in #123 |
| 5 | Audio-only YouTube: `choose_output` checks only the final `.mp3`. An unrelated existing `Song.m4a` (or `.webm`) is reused by yt-dlp, converted, then deleted. | `src/engine/page.rs:376-411` | fixed in step 8 |
| 6 | Incognito YouTube pages are recorded into the normal profile's "last videos" menus. `videoMenu` doesn't check `sender.tab.incognito`. | `extension/background.js:141` | fixed in step 8 |
| 7 | A 206 response is accepted without checking that `Content-Range` starts at the requested offset. A misbehaving proxy can silently corrupt the file. | `src/engine/http.rs:233` | fixed in step 8 |
| 8 | yt-dlp `.part` and `.ytdl` leftovers are not in `files_of`. "Delete with files" leaves them behind, and a restart resumes from stale parts. | `src/engine/mod.rs:903` | plausible |
| 9 | `yt-dlp -J` probes aren't deduplicated and have no timeout or kill. Reopening the video menu stacks about 100 MB processes. | `uidm-daemon.rs:1533`, `background.js` `probeStream` | plausible |
| 10 | Page downloads drop the cookies, referrer and login the extension sends, and they bypass the global speed limit and the per-host connection limits. | `src/media/ytdlp.rs`, `src/engine/page.rs` | confirmed (code) |
| 11 | `sanitize_filename` lets Windows device names through (`CON`, `NUL`, `AUX.mp4`), and also a trailing space left after trimming dots. | `src/engine/mod.rs:1631` | plausible, low |
| 12 | The installer kills only processes under `{app}`. A daemon from another install location keeps the port, and newer clients then talk to an old daemon. | `installer/uidm.iss:149` | plausible |

## Recommended order of work

1. **Native menu bar from MENU 129.** `muda` is already in Cargo.lock through tray-icon. Attach it with `Menu::init_for_hwnd` and remove the top row. This fixes the clipped Options button as well.
2. **Toolbar from TOOLBAR 166, with a real selection model.** Use 56x42 buttons with labels and new icons, gray out buttons by selection, add queue dropdowns, and add a large/small toggle.
3. **List.**
   - Use `egui_extras::TableBuilder` with IDM's 8 columns, 17 px virtualized rows, grid lines, multi-select and a row context menu.
   - Remove the tick and action columns.
   - Show "Complete" in Status and the total in Size.
4. **Categories tree.** Use IDM's nodes, icons, header and X. Show it even when disconnected, and make it resizable.
5. **Status bar.** Remove it, and move connection state to a banner that wraps and carries Retry and Restart daemon.
6. **Fonts and spacing.** Use 11 px, and tighten spacing and padding.
7. **Dialogs.** Rebuild them in the order of the H list. Use real OS windows (`show_viewport_deferred`), DLU sizes from `idm_ui.json`, and IDM's labels and button order.
8. **Defects.** Fix 2-7 alongside this work. 5 and 7 can lose or corrupt data.

Pixel-exact native look (a real SysListView32 and TreeView, the Explorer dark theme) needs Win32 controls. Embedding them in the eframe GL window brings focus, z-order and DPI trouble. If that level is required, rewriting the main window in Win32 is cleaner than mixing the two. Steps 1-7 reach a close match without it.
