//! Win32 calls the window needs outside egui: showing and hiding it while egui
//! is idle (viewport commands wait for a frame),
//! reading the clipboard for the tray menu and the clipboard offer, the
//! Windows `Open with` picker, dragging a file out of the window, and the
//! display languages.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

/// The main window's HWND, once eframe has created it.
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// The window is cloaked in the tray (see `hide`).
static HIDDEN: AtomicBool = AtomicBool::new(false);

const SW_SHOW: i32 = 5;
const SW_MINIMIZE: i32 = 6;
const SW_SHOWMINNOACTIVE: i32 = 7;
const SW_RESTORE: i32 = 9;
const CF_UNICODETEXT: u32 = 13;
const WM_LBUTTONUP: u32 = 0x0202;

const HWND_TOPMOST: isize = -1;
const HWND_NOTOPMOST: isize = -2;
const SWP_NOSIZE: u32 = 0x1;
const SWP_NOMOVE: u32 = 0x2;
const SWP_SHOWWINDOW: u32 = 0x40;

const DWMWA_CLOAK: u32 = 13;
/// `SetWindowLongPtrW` index of a top-level window's owner.
const GWLP_HWNDPARENT: i32 = -8;

#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const c_void, size: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
    fn GetCurrentProcessId() -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn IsIconic(hwnd: isize) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn GetForegroundWindow() -> isize;
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    fn AttachThreadInput(attach: u32, to: u32, on: i32) -> i32;
    fn BringWindowToTop(hwnd: isize) -> i32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    fn EnumWindows(callback: extern "system" fn(isize, isize) -> i32, param: isize) -> i32;
    fn GetWindowTextW(hwnd: isize, text: *mut u16, max: i32) -> i32;
    fn IsWindowVisible(hwnd: isize) -> i32;
    fn OpenClipboard(owner: isize) -> i32;
    fn CloseClipboard() -> i32;
    fn GetClipboardData(format: u32) -> *mut c_void;
    fn GetClipboardSequenceNumber() -> u32;
    fn GetCapture() -> isize;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn MonitorFromPoint(point: Point, flags: u32) -> isize;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
}

/// `POINT`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

/// `RECT`.
#[repr(C)]
#[derive(Default)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// `MONITORINFO`.
#[repr(C)]
#[derive(Default)]
struct MonitorInfo {
    size: u32,
    monitor: WinRect,
    work: WinRect,
    flags: u32,
}

const MONITOR_DEFAULTTOPRIMARY: u32 = 1;

/// `OPENASINFO`.
#[repr(C)]
struct OpenAsInfo {
    file: *const u16,
    class: *const u16,
    flags: u32,
}

const OAIF_ALLOW_REGISTRATION: u32 = 0x1;
const OAIF_REGISTER_EXT: u32 = 0x2;
const OAIF_EXEC: u32 = 0x4;

/// `SHFILEINFOW`.
#[repr(C)]
struct ShFileInfo {
    icon: isize,
    icon_index: i32,
    attributes: u32,
    display_name: [u16; 260],
    type_name: [u16; 80],
}

const SHGFI_USEFILEATTRIBUTES: u32 = 0x10;
const SHGFI_TYPENAME: u32 = 0x400;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

#[link(name = "shell32")]
extern "system" {
    fn SHOpenWithDialog(parent: isize, info: *const OpenAsInfo) -> i32;
    fn SHGetFileInfoW(path: *const u16, attributes: u32, info: *mut ShFileInfo, size: u32, flags: u32) -> usize;
}

/// What Windows calls files with extension `ext` (no dot), such as "PDF
/// Document", from the file types registered on this computer.
pub fn file_type_name(ext: &str) -> Option<String> {
    if ext.is_empty() || ext.contains(['\\', '/', '\0']) {
        return None;
    }
    let name: Vec<u16> = format!("file.{ext}").encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: an all-zero SHFILEINFOW is valid, and the call writes within
    // the size it is given.
    let mut info: ShFileInfo = unsafe { std::mem::zeroed() };
    let found = unsafe {
        SHGetFileInfoW(
            name.as_ptr(),
            FILE_ATTRIBUTE_NORMAL,
            &mut info,
            std::mem::size_of::<ShFileInfo>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        )
    };
    let len = info.type_name.iter().position(|c| *c == 0).unwrap_or(info.type_name.len());
    (found != 0 && len > 0).then(|| String::from_utf16_lossy(&info.type_name[..len]))
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(mem: *mut c_void) -> i32;
    fn GlobalSize(mem: *mut c_void) -> usize;
    fn GetUserPreferredUILanguages(flags: u32, count: *mut u32, buffer: *mut u16, size: *mut u32) -> i32;
}

/// Languages by name (`de-DE`) rather than by number.
const MUI_LANGUAGE_NAME: u32 = 0x8;

pub fn set_window(hwnd: isize) {
    WINDOW.store(hwnd, Ordering::Relaxed);
}

/// Show the window, restore it if minimized, and bring it to the front.
/// Safe to call from any thread.
pub fn show() {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    // SAFETY: plain calls on our own window handle.
    unsafe {
        if HIDDEN.swap(false, Ordering::Relaxed) {
            cloak(hwnd, false);
            taskbar_button(hwnd, true);
        }
        ShowWindow(hwnd, SW_SHOW);
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
    }
    raise(hwnd);
}

/// Whether one of UIDM's own windows is in front, holding the focus.
pub fn in_front() -> bool {
    // SAFETY: plain calls; a null window has no process.
    unsafe {
        let front = GetForegroundWindow();
        let mut pid = 0;
        front != 0 && GetWindowThreadProcessId(front, &mut pid) != 0 && pid == GetCurrentProcessId()
    }
}

/// The middle of the work area (the screen less the taskbar) of the monitor
/// under the mouse, in pixels, while the main window is in the tray or
/// minimized. A dialog then opens there, as IDM's do, instead of centered
/// on a window the user cannot see. None while the main window shows.
pub fn screen_center() -> Option<(f32, f32)> {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    // SAFETY: plain calls; the structs are sized for them.
    unsafe {
        if hwnd != 0 && !HIDDEN.load(Ordering::Relaxed) && IsIconic(hwnd) == 0 {
            return None;
        }
        let mut cursor = Point::default();
        GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, ..Default::default() };
        if monitor == 0 || GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let w = &info.work;
        Some(((w.left + w.right) as f32 / 2.0, (w.top + w.bottom) as f32 / 2.0))
    }
}

/// A dialog that opens on its own (save-as, download complete) needs the
/// window's frames. Here a hidden window still gets them (see `hide`), so
/// the dialog shows without the main window, as IDM's do.
pub fn wake() {}

/// Bring `hwnd` in front of the other programs' windows and give it the
/// focus. Windows lets only the program the user is in take the foreground,
/// so a download caught from the browser left the window, and its save-as
/// dialog, behind the browser. Joining the foreground window's input for
/// the call is allowed; should the focus still be refused, a pass through
/// "topmost" at least puts the window on top, where the user sees it.
fn raise(hwnd: isize) {
    // SAFETY: plain calls on window and thread handles Windows gave us; the
    // input is detached again before returning.
    unsafe {
        let front = GetForegroundWindow();
        if front == hwnd {
            return;
        }
        let me = GetCurrentThreadId();
        let theirs = if front != 0 { GetWindowThreadProcessId(front, std::ptr::null_mut()) } else { 0 };
        let joined = theirs != 0 && theirs != me && AttachThreadInput(me, theirs, 1) != 0;
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if joined {
            AttachThreadInput(me, theirs, 0);
        }
        if GetForegroundWindow() != hwnd {
            let keep = SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW;
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, keep);
            SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, keep);
        }
    }
}

/// Bring this program's visible window titled `title` to the front, as
/// `raise` does: a dialog that opens on its own (a save-as dialog for a
/// caught download, a completion dialog). False when there is no such
/// window yet.
pub fn raise_window(title: &str) -> bool {
    let found = find_window(title);
    if found == 0 {
        return false;
    }
    raise(found);
    true
}

/// Make the main window the owner of this program's dialog titled `title`,
/// as a Win32 dialog's parent is. Windows then keeps the dialog in front of
/// the main window, so a click on the main window no longer covers it.
///
/// Waits while the main window sits in the tray: Windows hides owned
/// windows with their minimized owner, and a save-as dialog for a caught
/// download must show there. False until the dialog is owned, so the
/// caller tries again on a later frame.
pub fn own_window(title: &str) -> bool {
    let main = WINDOW.load(Ordering::Relaxed);
    // SAFETY: a plain call on our own window handle.
    if main == 0 || HIDDEN.load(Ordering::Relaxed) || unsafe { IsIconic(main) } != 0 {
        return false;
    }
    let found = find_window(title);
    if found == 0 || found == main {
        return false;
    }
    // SAFETY: both handles are this program's own top-level windows.
    unsafe {
        SetWindowLongPtrW(found, GWLP_HWNDPARENT, main);
    }
    true
}

/// This program's visible window titled `title`, or 0.
fn find_window(title: &str) -> isize {
    struct Search {
        pid: u32,
        title: Vec<u16>,
        found: isize,
    }
    extern "system" fn visit(hwnd: isize, param: isize) -> i32 {
        // SAFETY: `param` is the `Search` below, alive for the whole
        // EnumWindows call; the text buffer is sized for the call.
        unsafe {
            let search = &mut *(param as *mut Search);
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != search.pid || IsWindowVisible(hwnd) == 0 {
                return 1;
            }
            let mut text = [0u16; 512];
            let n = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32).max(0) as usize;
            if text[..n] == search.title[..] {
                search.found = hwnd;
                return 0;
            }
            1
        }
    }
    let mut search = Search {
        // SAFETY: no arguments, no state.
        pid: unsafe { GetCurrentProcessId() },
        title: title.encode_utf16().collect(),
        found: 0,
    };
    // SAFETY: `visit` reads `search` only during this call.
    unsafe {
        EnumWindows(visit, &mut search as *mut Search as isize);
    }
    search.found
}

/// Hide the window; it lives on in the tray.
///
/// A window hidden with `SW_HIDE` gets no frames, and every dialog is drawn
/// in the main window's frame, so a save-as or download window could not
/// open while UIDM sat in the tray. Instead the window is cloaked: DWM stops
/// drawing it and it takes no clicks, but Windows still counts it visible
/// and keeps painting it. Minimizing it hands the keyboard to the next
/// window, and its taskbar button goes.
pub fn hide() {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    // SAFETY: plain calls on our own window handle.
    unsafe {
        cloak(hwnd, true);
        HIDDEN.store(true, Ordering::Relaxed);
        if IsIconic(hwnd) == 0 {
            let front = GetForegroundWindow() == hwnd;
            ShowWindow(hwnd, if front { SW_MINIMIZE } else { SW_SHOWMINNOACTIVE });
        }
        taskbar_button(hwnd, false);
    }
}

/// Cloak or uncloak `hwnd` (`DWMWA_CLOAK`).
unsafe fn cloak(hwnd: isize, on: bool) {
    let value: i32 = on.into();
    DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, (&value as *const i32).cast(), 4);
}

/// Add or take away `hwnd`'s taskbar button, which a cloaked window may
/// keep.
fn taskbar_button(hwnd: isize, on: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
    // SAFETY: COM calls on an object made here; a thread already in COM
    // keeps its own apartment.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(list) = CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER) else {
            return;
        };
        if list.HrInit().is_err() {
            return;
        }
        let hwnd = HWND(hwnd as *mut c_void);
        let _ = if on { list.AddTab(hwnd) } else { list.DeleteTab(hwnd) };
    }
}

/// Show Windows' "Open with" picker for `path` on its own thread (it waits
/// for the user), and open the file with the program chosen.
pub fn open_with(path: &str) {
    let file: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let parent = WINDOW.load(Ordering::Relaxed);
    std::thread::spawn(move || {
        let info = OpenAsInfo {
            file: file.as_ptr(),
            class: std::ptr::null(),
            flags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_EXEC,
        };
        // SAFETY: `info` and the path it points to live until the call
        // returns.
        unsafe { SHOpenWithDialog(parent, &info) };
    });
}

/// Drag the file at `path` out of the window, as Explorer drags one. A drop
/// in a folder copies it, and a program takes it as the file itself. Call
/// it while the left button is down; it returns once the file is dropped or
/// the drag is given up. Moving is not offered, so the download keeps its
/// file.
pub fn drag_file(path: &str) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Ole::{IDropSource, OleInitialize, OleUninitialize, DROPEFFECT_COPY, DROPEFFECT_LINK};
    use windows::Win32::UI::Shell::SHDoDragDrop;
    // SAFETY: COM calls on objects made here, on the window's thread, which
    // OLE needs for a drag. The shell supplies both the file's data object
    // and the drop source, and runs the drag's loop until it ends.
    unsafe {
        // eframe captures the mouse for the window the button went down in,
        // which may be a dialog rather than the main window.
        let source = match GetCapture() {
            0 => WINDOW.load(Ordering::Relaxed),
            hwnd => hwnd,
        };
        let ole = OleInitialize(None).is_ok();
        if let Some(data) = file_data(path) {
            let _ = SHDoDragDrop(
                Some(HWND(source as *mut c_void)),
                &data,
                None::<&IDropSource>,
                DROPEFFECT_COPY | DROPEFFECT_LINK,
            );
        }
        if ole {
            OleUninitialize();
        }
        // The drag's loop took the button's release, so the window never
        // saw it, and egui would go on dragging until the next click.
        if source != 0 {
            PostMessageW(source, WM_LBUTTONUP, 0, 0);
        }
    }
}

/// The shell's data object for the file at `path`, the one Explorer drags,
/// which carries it as a file list (`CF_HDROP`) among other formats.
fn file_data(path: &str) -> Option<windows::Win32::System::Com::IDataObject> {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::{IBindCtx, IDataObject};
    use windows::Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName};
    // SAFETY: the path lives until the call returns; the item and its data
    // object are COM objects that release themselves when dropped.
    unsafe {
        let item = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path), None::<&IBindCtx>).ok()?;
        item.BindToHandler::<_, IDataObject>(None::<&IBindCtx>, &BHID_DataObject).ok()
    }
}

/// A number that changes whenever the clipboard does.
pub fn clipboard_sequence() -> u32 {
    // SAFETY: takes no arguments and only reads a counter.
    unsafe { GetClipboardSequenceNumber() }
}

/// The clipboard's text, if it holds any.
pub fn clipboard_text() -> Option<String> {
    // SAFETY: the clipboard is opened and closed around the read; the
    // locked memory is a NUL-terminated UTF-16 string by CF_UNICODETEXT's
    // contract, read only while locked.
    unsafe {
        if OpenClipboard(0) == 0 {
            return None;
        }
        let mut text = None;
        let handle = GetClipboardData(CF_UNICODETEXT);
        if !handle.is_null() {
            let ptr = GlobalLock(handle) as *const u16;
            if !ptr.is_null() {
                // Up to the NUL, but never past the end of the block: the
                // program that put it there may have left the NUL out.
                let max = GlobalSize(handle) / 2;
                let mut len = 0;
                while len < max && *ptr.add(len) != 0 {
                    len += 1;
                }
                text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)));
                GlobalUnlock(handle);
            }
        }
        CloseClipboard();
        text
    }
}

/// Windows' display languages, most preferred first, as tags such as
/// `de-DE`.
pub fn display_languages() -> Vec<String> {
    let (mut count, mut size) = (0u32, 0u32);
    // SAFETY: the first call only reports the size; the second fills a
    // buffer of that size with NUL-separated strings, ending in two NULs.
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, std::ptr::null_mut(), &mut size) == 0 || size == 0
        {
            return Vec::new();
        }
        let mut buffer = vec![0u16; size as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, buffer.as_mut_ptr(), &mut size) == 0 {
            return Vec::new();
        }
        buffer.split(|&c| c == 0).filter(|tag| !tag.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_drags_as_a_file_list() {
        use windows::Win32::System::Com::{
            CoInitializeEx, COINIT_APARTMENTTHREADED, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL,
        };
        const CF_HDROP: u16 = 15;
        let path = std::env::temp_dir().join(format!("uidm-drag-test-{}.txt", std::process::id()));
        std::fs::write(&path, b"drag me").unwrap();
        // SAFETY: COM is started on this thread before the shell is asked
        // for the data object; the format is a plain struct read by the call.
        let found = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let data = file_data(path.to_str().unwrap()).expect("the shell has a data object for a file");
            let format = FORMATETC {
                cfFormat: CF_HDROP,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            data.QueryGetData(&format).is_ok()
        };
        let _ = std::fs::remove_file(&path);
        assert!(found, "Explorer and other programs take a dropped file as CF_HDROP");
        assert!(file_data(r"C:\no such folder\no such file.txt").is_none());
    }
}
