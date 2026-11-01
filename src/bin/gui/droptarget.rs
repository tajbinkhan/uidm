//! The drop target: a small window that stays on top of the others and
//! takes links dragged onto it from a browser or another program. eframe
//! only receives dropped files, so this is a plain Win32 window on its own
//! thread with its own OLE drop target, which reads the dragged URL (the
//! browser's `UniformResourceLocatorW` format, else plain text).
//!
//! Drag the window to move it; its place is remembered. Double-click it to
//! open UIDM; right-click it to close it.

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::i18n::tr;

type Hwnd = isize;

const WS_POPUP: u32 = 0x8000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_EX_TOPMOST: u32 = 0x8;
const WS_EX_TOOLWINDOW: u32 = 0x80;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_PAINT: u32 = 0x000F;
const WM_NCHITTEST: u32 = 0x0084;
const WM_NCLBUTTONDBLCLK: u32 = 0x00A3;
const WM_NCRBUTTONUP: u32 = 0x00A5;
const WM_EXITSIZEMOVE: u32 = 0x0232;
const HTCAPTION: isize = 2;
const DT_CENTER: u32 = 0x1;
const DT_VCENTER: u32 = 0x4;
const DT_WORDBREAK: u32 = 0x10;
const TRANSPARENT: i32 = 1;
const CF_UNICODETEXT: u16 = 13;
const TYMED_HGLOBAL: u32 = 1;
const DVASPECT_CONTENT: u32 = 1;
const DROPEFFECT_NONE: u32 = 0;
const DROPEFFECT_COPY: u32 = 1;
const S_OK: i32 = 0;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
const IDC_ARROW: usize = 32512;

const WIDTH: i32 = 132;
const HEIGHT: i32 = 64;

#[repr(C)]
struct WndClass {
    style: u32,
    proc_: extern "system" fn(Hwnd, u32, usize, isize) -> isize,
    cls_extra: i32,
    wnd_extra: i32,
    instance: *mut c_void,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: *const u16,
    class_name: *const u16,
}

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct Msg {
    hwnd: Hwnd,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: [i32; 2],
    private: u32,
}

#[repr(C)]
struct PaintStruct {
    hdc: *mut c_void,
    erase: i32,
    paint: Rect,
    restore: i32,
    inc_update: i32,
    reserved: [u8; 32],
}

#[repr(C)]
struct FormatEtc {
    format: u16,
    target_device: *mut c_void,
    aspect: u32,
    index: i32,
    tymed: u32,
}

#[repr(C)]
struct StgMedium {
    tymed: u32,
    data: *mut c_void,
    release: *mut c_void,
}

/// `POINTL`, passed by value.
#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    _x: i32,
    _y: i32,
}

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const IID_IUNKNOWN: Guid = Guid { data1: 0, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
const IID_IDROPTARGET: Guid = Guid { data1: 0x0000_0122, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };

fn same_guid(a: &Guid, b: &Guid) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WndClass) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Hwnd,
        menu: *mut c_void,
        instance: *mut c_void,
        param: *mut c_void,
    ) -> Hwnd;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostQuitMessage(code: i32);
    fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn DestroyWindow(hwnd: Hwnd) -> i32;
    fn BeginPaint(hwnd: Hwnd, ps: *mut PaintStruct) -> *mut c_void;
    fn EndPaint(hwnd: Hwnd, ps: *const PaintStruct) -> i32;
    fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
    fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
    fn DrawTextW(hdc: *mut c_void, text: *const u16, len: i32, rect: *mut Rect, format: u32) -> i32;
    fn FillRect(hdc: *mut c_void, rect: *const Rect, brush: *mut c_void) -> i32;
    fn LoadCursorW(instance: *mut c_void, name: usize) -> *mut c_void;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
    fn GetSystemMetrics(index: i32) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateSolidBrush(color: u32) -> *mut c_void;
    fn DeleteObject(object: *mut c_void) -> i32;
    fn SetBkMode(hdc: *mut c_void, mode: i32) -> i32;
    fn SetTextColor(hdc: *mut c_void, color: u32) -> u32;
}

#[link(name = "ole32")]
extern "system" {
    fn OleInitialize(reserved: *mut c_void) -> i32;
    fn OleUninitialize();
    fn RegisterDragDrop(hwnd: Hwnd, target: *mut c_void) -> i32;
    fn RevokeDragDrop(hwnd: Hwnd) -> i32;
    fn ReleaseStgMedium(medium: *mut StgMedium);
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(mem: *mut c_void) -> i32;
    fn GlobalSize(mem: *mut c_void) -> usize;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The window, while it is open.
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// What a drop does with each link.
type OnDrop = Arc<dyn Fn(String) + Send + Sync>;
static ON_DROP: Mutex<Option<OnDrop>> = Mutex::new(None);
/// What a double-click does.
static ON_OPEN: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

/// `%LOCALAPPDATA%\UIDM\droptarget.txt`: `x y` of the window.
fn place_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|b| PathBuf::from(b).join("UIDM").join("droptarget.txt"))
}

fn saved_place() -> Option<(i32, i32)> {
    let text = std::fs::read_to_string(place_file()?).ok()?;
    let mut parts = text.split_whitespace().map(|p| p.parse::<i32>().ok());
    Some((parts.next()??, parts.next()??))
}

/// Open, or opening: the window's thread sets the handle once it exists.
pub fn is_open() -> bool {
    WINDOW.load(Ordering::Relaxed) != 0
}

/// Set while the window's thread is creating it.
const OPENING: isize = -1;

/// Open the drop target if it is not open. `on_drop` gets each dropped
/// link; `on_open` runs on a double-click.
pub fn open(on_drop: OnDrop, on_open: Arc<dyn Fn() + Send + Sync>) {
    if is_open() {
        return;
    }
    *ON_DROP.lock().unwrap() = Some(on_drop);
    *ON_OPEN.lock().unwrap() = Some(on_open);
    WINDOW.store(OPENING, Ordering::Relaxed);
    std::thread::spawn(run);
}

/// Close it, if it is open.
pub fn close() {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    if hwnd > 0 {
        // SAFETY: posting to a window handle is safe even if it is gone.
        unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
    }
}

/// The links in dropped text: each line or word that is a download address.
pub fn links_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        let word = word.trim_matches(['"', '\'', '<', '>']);
        if uidm::batch::is_download_url(word) && !out.iter().any(|w: &String| w == word) {
            out.push(word.to_string());
        }
    }
    out
}

fn run() {
    // SAFETY: plain Win32 and OLE calls on this thread's own window; every
    // pointer passed lives through its call.
    unsafe {
        OleInitialize(ptr::null_mut());
        let instance = GetModuleHandleW(ptr::null());
        let class = wide("UidmDropTarget");
        let wc = WndClass {
            style: 0,
            proc_: window_proc,
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: ptr::null_mut(),
            cursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            background: ptr::null_mut(),
            menu_name: ptr::null(),
            class_name: class.as_ptr(),
        };
        RegisterClassW(&wc);
        // Bottom right of the screen the first time.
        let (x, y) = saved_place().unwrap_or_else(|| (GetSystemMetrics(0) - WIDTH - 40, GetSystemMetrics(1) - HEIGHT - 80));
        let title = wide(tr("UIDM drop target"));
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_VISIBLE | WS_BORDER,
            x,
            y,
            WIDTH,
            HEIGHT,
            0,
            ptr::null_mut(),
            instance,
            ptr::null_mut(),
        );
        if hwnd == 0 {
            WINDOW.store(0, Ordering::Relaxed);
            OleUninitialize();
            return;
        }
        WINDOW.store(hwnd, Ordering::Relaxed);
        let target = DropTarget::new();
        RegisterDragDrop(hwnd, target as *mut c_void);
        let mut msg = Msg {
            hwnd: 0,
            message: 0,
            wparam: 0,
            lparam: 0,
            time: 0,
            pt: [0, 0],
            private: 0,
        };
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        RevokeDragDrop(hwnd);
        DropTarget::release(target as *mut c_void);
        WINDOW.store(0, Ordering::Relaxed);
        OleUninitialize();
    }
}

extern "system" fn window_proc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: called by Windows for this thread's window with valid
    // arguments.
    unsafe {
        match msg {
            // The whole window is a handle to drag it by.
            WM_NCHITTEST => HTCAPTION,
            WM_NCLBUTTONDBLCLK => {
                if let Some(f) = ON_OPEN.lock().unwrap().clone() {
                    f();
                }
                0
            }
            WM_NCRBUTTONUP => {
                DestroyWindow(hwnd);
                0
            }
            WM_EXITSIZEMOVE => {
                let mut r = Rect::default();
                GetWindowRect(hwnd, &mut r);
                if let Some(path) = place_file() {
                    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
                    let _ = std::fs::write(path, format!("{} {}", r.left, r.top));
                }
                0
            }
            WM_PAINT => {
                let mut ps = PaintStruct {
                    hdc: ptr::null_mut(),
                    erase: 0,
                    paint: Rect::default(),
                    restore: 0,
                    inc_update: 0,
                    reserved: [0; 32],
                };
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut r = Rect::default();
                GetClientRect(hwnd, &mut r);
                // Colors are 0x00BBGGRR.
                let brush = CreateSolidBrush(0x00EB_6325);
                FillRect(hdc, &r, brush);
                DeleteObject(brush);
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, 0x00FF_FFFF);
                let text = wide(tr("Drop links here\nfor UIDM"));
                DrawTextW(hdc, text.as_ptr(), -1, &mut r, DT_CENTER | DT_VCENTER | DT_WORDBREAK);
                EndPaint(hwnd, &ps);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// `IDropTarget`, by hand: a vtable pointer and a reference count.
#[repr(C)]
struct DropTarget {
    vtbl: *const DropTargetVtbl,
    refs: AtomicU32,
}

#[repr(C)]
struct DropTargetVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    drag_enter: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, Point, *mut u32) -> i32,
    drag_over: unsafe extern "system" fn(*mut c_void, u32, Point, *mut u32) -> i32,
    drag_leave: unsafe extern "system" fn(*mut c_void) -> i32,
    drop: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, Point, *mut u32) -> i32,
}

static VTBL: DropTargetVtbl = DropTargetVtbl {
    query_interface: DropTarget::query_interface,
    add_ref: DropTarget::add_ref,
    release: DropTarget::release,
    drag_enter: DropTarget::drag_enter,
    drag_over: DropTarget::drag_over,
    drag_leave: DropTarget::drag_leave,
    drop: DropTarget::drop,
};

/// `IDataObject::GetData`, the fourth entry of its vtable.
type GetData = unsafe extern "system" fn(*mut c_void, *const FormatEtc, *mut StgMedium) -> i32;

impl DropTarget {
    fn new() -> *mut DropTarget {
        Box::into_raw(Box::new(DropTarget {
            vtbl: &VTBL,
            refs: AtomicU32::new(1),
        }))
    }

    unsafe extern "system" fn query_interface(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> i32 {
        if same_guid(&*iid, &IID_IUNKNOWN) || same_guid(&*iid, &IID_IDROPTARGET) {
            Self::add_ref(this);
            *out = this;
            S_OK
        } else {
            *out = ptr::null_mut();
            E_NOINTERFACE
        }
    }

    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        (*(this as *mut DropTarget)).refs.fetch_add(1, Ordering::Relaxed) + 1
    }

    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        let left = (*(this as *mut DropTarget)).refs.fetch_sub(1, Ordering::Release) - 1;
        if left == 0 {
            drop(Box::from_raw(this as *mut DropTarget));
        }
        left
    }

    unsafe extern "system" fn drag_enter(_: *mut c_void, data: *mut c_void, _: u32, _: Point, effect: *mut u32) -> i32 {
        *effect = if read_text(data).is_some_and(|t| !links_in(&t).is_empty()) {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        };
        S_OK
    }

    unsafe extern "system" fn drag_over(_: *mut c_void, _: u32, _: Point, effect: *mut u32) -> i32 {
        // Keeps what DragEnter decided.
        if *effect & DROPEFFECT_COPY != 0 {
            *effect = DROPEFFECT_COPY;
        }
        S_OK
    }

    unsafe extern "system" fn drag_leave(_: *mut c_void) -> i32 {
        S_OK
    }

    unsafe extern "system" fn drop(_: *mut c_void, data: *mut c_void, _: u32, _: Point, effect: *mut u32) -> i32 {
        let links = read_text(data).map(|t| links_in(&t)).unwrap_or_default();
        *effect = if links.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY };
        if let Some(f) = ON_DROP.lock().unwrap().clone() {
            for link in links {
                f(link);
            }
        }
        S_OK
    }
}

/// The dragged text: the URL a browser offers, else plain text.
///
/// SAFETY: `data` is the `IDataObject` OLE passed to the drop target.
unsafe fn read_text(data: *mut c_void) -> Option<String> {
    if data.is_null() {
        return None;
    }
    let vtbl = *(data as *const *const usize);
    let get_data: GetData = std::mem::transmute(*vtbl.add(3));
    let url_format = RegisterClipboardFormatW(wide("UniformResourceLocatorW").as_ptr()) as u16;
    for format in [url_format, CF_UNICODETEXT] {
        let fmt = FormatEtc {
            format,
            target_device: ptr::null_mut(),
            aspect: DVASPECT_CONTENT,
            index: -1,
            tymed: TYMED_HGLOBAL,
        };
        let mut medium = StgMedium {
            tymed: 0,
            data: ptr::null_mut(),
            release: ptr::null_mut(),
        };
        if get_data(data, &fmt, &mut medium) != S_OK {
            continue;
        }
        let mut text = None;
        if medium.tymed == TYMED_HGLOBAL && !medium.data.is_null() {
            let p = GlobalLock(medium.data) as *const u16;
            if !p.is_null() {
                // Up to the NUL, but never past the end of the block: the
                // program that dragged it may have left the NUL out.
                let max = GlobalSize(medium.data) / 2;
                let mut len = 0;
                while len < max && *p.add(len) != 0 {
                    len += 1;
                }
                text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
                GlobalUnlock(medium.data);
            }
        }
        ReleaseStgMedium(&mut medium);
        if text.is_some() {
            return text;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_picked_out_of_dropped_text() {
        assert_eq!(links_in("https://x/a.zip"), ["https://x/a.zip"]);
        assert_eq!(
            links_in("see <https://x/a.zip> and\r\nftp://y/b.iso https://x/a.zip"),
            ["https://x/a.zip", "ftp://y/b.iso"]
        );
        assert!(links_in("just words").is_empty());
    }

    /// Opens a real window on the desktop, so it is left out of the usual
    /// run: `cargo test --bin uidm-gui -- --ignored`.
    #[test]
    #[ignore]
    fn the_window_opens_and_closes() {
        open(Arc::new(|_| {}), Arc::new(|| {}));
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while WINDOW.load(Ordering::Relaxed) <= 0 {
            assert!(std::time::Instant::now() < until, "the window never opened");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        close();
        while is_open() {
            assert!(std::time::Instant::now() < until, "the window never closed");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}