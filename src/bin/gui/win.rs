//! Win32 calls the window needs outside egui: showing and hiding it while egui
//! is idle (a hidden window gets no frames, so viewport commands would wait),
//! reading the clipboard for the tray menu and the clipboard offer, the
//! Windows `Open with` picker, and the display languages.

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};

/// The main window's HWND, once eframe has created it.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

const SW_HIDE: i32 = 0;
const SW_SHOW: i32 = 5;
const SW_RESTORE: i32 = 9;
const CF_UNICODETEXT: u32 = 13;

const GWLP_HWNDPARENT: i32 = -8;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
extern "system" {
    fn EnumThreadWindows(thread: u32, callback: extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
    fn GetWindowTextW(hwnd: isize, text: *mut u16, len: i32) -> i32;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> u32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn IsIconic(hwnd: isize) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn OpenClipboard(owner: isize) -> i32;
    fn CloseClipboard() -> i32;
    fn GetClipboardData(format: u32) -> *mut c_void;
    fn GetClipboardSequenceNumber() -> u32;
}

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
    fn GetUserPreferredUILanguages(flags: u32, count: *mut u32, buffer: *mut u16, size: *mut u32) -> i32;
}

/// Languages by name (`de-DE`) rather than by number.
const MUI_LANGUAGE_NAME: u32 = 0x8;

pub fn set_window(hwnd: isize) {
    WINDOW.store(hwnd, Ordering::Relaxed);
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
}

/// The window titled `title` that this thread made, if eframe has made it.
/// Dialogs are made on the UI thread, which calls this.
fn thread_window(title: &str) -> Option<isize> {
    struct Search {
        title: Vec<u16>,
        found: isize,
    }
    extern "system" fn visit(hwnd: isize, lparam: isize) -> i32 {
        // SAFETY: `lparam` is the `Search` below, alive for the whole call.
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut text = [0u16; 256];
        // SAFETY: the buffer's length is passed with it.
        let len = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
        if text[..len.max(0) as usize] == search.title[..] && hwnd != WINDOW.load(Ordering::Relaxed) {
            search.found = hwnd;
            return 0;
        }
        1
    }
    let mut search = Search { title: title.encode_utf16().collect(), found: 0 };
    // SAFETY: `visit` only reads `search` through the pointer while
    // EnumThreadWindows runs.
    unsafe { EnumThreadWindows(GetCurrentThreadId(), visit, &mut search as *mut Search as isize) };
    (search.found != 0).then_some(search.found)
}

/// Make the main window the owner of the dialog titled `title`, so it stays
/// in front of the main window. False until the dialog's window exists.
pub fn own_dialog(title: &str) -> bool {
    let owner = WINDOW.load(Ordering::Relaxed);
    let Some(dialog) = thread_window(title).filter(|_| owner != 0) else {
        return false;
    };
    // SAFETY: both are this process's windows; GWLP_HWNDPARENT on a
    // top-level window sets its owner.
    unsafe { SetWindowLongPtrW(dialog, GWLP_HWNDPARENT, owner) };
    true
}

/// Center the dialog titled `title` on the main window. False until the
/// dialog's window exists.
pub fn center_dialog(title: &str) -> bool {
    let owner = WINDOW.load(Ordering::Relaxed);
    let Some(dialog) = thread_window(title).filter(|_| owner != 0) else {
        return false;
    };
    let (mut main, mut own) = (Rect::default(), Rect::default());
    // SAFETY: both are this process's windows, and the rects are ours.
    unsafe {
        if GetWindowRect(owner, &mut main) == 0 || GetWindowRect(dialog, &mut own) == 0 {
            return false;
        }
        let x = (main.left + main.right) / 2 - (own.right - own.left) / 2;
        let y = (main.top + main.bottom) / 2 - (own.bottom - own.top) / 2;
        SetWindowPos(dialog, 0, x, y.max(main.top), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
    true
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
        ShowWindow(hwnd, SW_SHOW);
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

/// Hide the window; it lives on in the tray.
pub fn hide() {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    if hwnd != 0 {
        // SAFETY: plain call on our own window handle.
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
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
                let mut len = 0;
                while *ptr.add(len) != 0 {
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
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, std::ptr::null_mut(), &mut size) == 0 || size == 0 {
            return Vec::new();
        }
        let mut buffer = vec![0u16; size as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, buffer.as_mut_ptr(), &mut size) == 0 {
            return Vec::new();
        }
        buffer
            .split(|&c| c == 0)
            .filter(|tag| !tag.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }
}
