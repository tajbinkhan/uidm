# IDM parity audit, 2026-10-28

Source: REA `inspect_pe_resources` on `C:\Program Files (x86)\Internet Download Manager\IDMan.exe` (IDM 6.43, SHA-256 `75ac33a7…2145285`, Evidence `ev_2a983598…11647`). REA listed 436 resources with complete coverage: 105 dialogs (RT_DIALOG) and 2 menus (RT_MENU). The dialog and menu payloads were decoded from the file ranges REA reported. Each IDM item was then checked against UIDM's source.

Left out by design, as in roadmap #66: dial-up/VPN (184-186), IE-based browser monitoring and IE cache, toolbar skins and button sizes, registration and ordering, reports to Tonec (232, 336), component download (190), Chrome/Edge help screens (381, 489), the admin warning (342).

Correction: this audit read a checkout that was behind origin. PR #67, already merged there, had finished #46, #63 and #64, so the first table below is wrong. #68 to #70, filed from it, were closed as already done.

On 2026-10-28 the remaining issues, #71 to #107, were re-checked against origin/main (8a00f78). PR #67 added only the theme, text size and language options and the Firefox copy of the extension, so none of them was done. Their file:line references were moved to the current code. The line numbers below are still the old ones from cc2fc2d.

The gaps below are filed as #68 to #107 (milestones 13 to 19) under roadmap #108. TLS 1.3 was not filed because it was never checked, and toolbar customization went under "Not planned" with toolbar skins.

## Closed issues that are not done

| Issue | What is missing | Evidence |
|---|---|---|
| #46 Firefox | Setup registers the host with Chrome and Edge only. Only `install-host.ps1` writes the Mozilla key and the Firefox manifest. | `installer/uidm.iss:61-62`, `install-host.ps1:33` |
| #63 Theme and font size | The window follows the system light/dark setting. There is no theme choice and no font or size setting. | `src/bin/gui/theme.rs:88-110` |
| #64 Translation | No string table, no language setting. The window is English only. | no `language`/`translat` match in `src/` |

## Missing

Main window and menus (MENU 129)
- Hide categories: the sidebar is always drawn (`uidm-gui.rs:2815`).
- Tray icon: no "Don't show" option.
- Help contents, tutorials and Tip of the Day (105, 236).
- Choose font / reset font.
- Column widths can't be set or saved (209, `columns.rs:80-87`).

Adding downloads
- Save-as dialog (130) has no category picker, no "+" for a new category and no description field (`Action::Confirm`, `uidm-gui.rs:395-406`).
- No per-type list of extensions that start without the dialog (193).
- No setting to only add to the queue, and no option to start while the dialog is open (291).
- No "Yes to all / No to all" prompt (147).
- No login prompt on a 401: it becomes "needs login" (279, `engine/http.rs:157`).
- No "Ignore file modification time changes when resuming" (291): a changed ETag or Last-Modified always blocks the resume (`engine/resume.rs:40-63`).
- Links picker (228): no "all files to one category", no editing a link, no rename by pattern.

Download progress and completion
- No per-download status window with resume capability (363), and nothing to customize (367).
- No per-download completion options (364): when-done is per queue only.
- Shutdown countdown has no "Shut down now" button (150, 292).
- Shutdown never forces processes to close: `shutdown /s /t 0` without `/f` (`src/power.rs:32`).

Queues and scheduler
- No "start again every N hours/min" for queues (306) or grabber projects (284) (`src/queue.rs` has days or a date only).
- Export can't be limited to one queue (191).
- No "don't ask me again" for the queue choice (345).

Browser capture
- Prevent/force keys (229): one key each, no combinations, no Insert, no left-click option, no skipping pages/pictures/scripts when forcing.
- Context menu items can't be turned off one by one (334, 335, `background.js:641-661`).
- Web player panel (340): no full/mini view, file types are fixed (`media.js:30-44`), minimum size is fixed at 128 KB (`media.js:18`), no toggle for the protected-content note.
- No "Get System" button to copy the system proxy into the manual fields (140).
- No notice when UIDM can't take over a download (305): it goes back to the browser silently.

Site grabber (MENU 256, dialogs 239-284)
- No Grabber Settings dialog (262): no "files to explore at the same time" (the crawl reads one page at a time), no link text as description.
- No named filters (244, 245) and no field for file types to skip (240).
- "Search files on this site only" and "start downloading all matched files at once" (240) are missing.
- "All files to a category's folder" and "all files to one flat folder" (259) are missing.
- Ignore popup windows (264) is missing.
- Templates (257, 268): one unnamed template, no names, no built-in templates.
- Scheduled runs (284): "download checked files" and when-done actions are missing.
- No Browse button for the virus scanner (494).

## Partial

- Import/export uses UIDM's own JSON and text, not IDM's `.ef2` format.
- "Add batch download from clipboard": the tray adds one URL; the batch list takes pasted text only.
- Clipboard watching asks Download/Ignore instead of starting the download.
- Sort covers every column except "Parent web page".
- Connections window (103) has no Pause/Cancel; those are on the row.
- Properties (135) has no file type field and no Open button.
- "Remember this path" works only for named categories, not General, and has no checkbox in the dialog (130, 143).
- "Use the browser's proxy" is always on when set, not only after errors (140).
- The quota warning before stopping always fires and can't be turned off (141, 309).
- Update notice links to the release page. "Update now" doesn't download or install (144).
- Site logins match by host only, so a path in the pattern is ignored (146).
- No login fields when adding a URL (148) or a batch (280).
- Sounds are two on/off system beeps, with no per-event file (195).
- Browsers to capture are fixed at install. There is no list and no "Add browser" (139, 296, 297).
- One login is shared by all proxies, and SOCKS covers every protocol instead of only the unticked ones (369).
- "Don't show again" (286) and "remember my selection" (298) are only in Options, not in the dialogs.
- File types to capture have no Default button (294).
- The schedule reminder that UIDM must be running is in the grabber only (332).
- Grabber results are a flat checklist, not a tree (241). Stats have no explored/matched/downloaded split (271).
- Grabber login uses the global site logins, not a per-project login (257).
- "Explore the whole site" stops at depth 10 and page/MB budgets (264). Other sites share the same depth.
- A scheduled "explore and download" adds new files to the queue but never starts it (284, `uidm-daemon.rs:1529-1537`).
- Last FLV / last 10 FLV videos are in the video button's list, not the context menu (334, 335).
- TLS 1.3 uses SChannel through native-tls, with no toggle. Whether it negotiates TLS 1.3 was not checked (141).
