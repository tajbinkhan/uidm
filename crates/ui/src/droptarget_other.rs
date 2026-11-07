//! The drop target on macOS and Linux: not there yet. The Windows one is a
//! Win32 window with an OLE drop target; eframe itself only receives dropped
//! files, so links dragged from a browser have no way in here. The setting
//! and tray item do nothing.

use std::sync::Arc;

/// Whether the drop target is open: never.
pub fn is_open() -> bool {
    false
}

/// Would open the drop target.
pub fn open(_on_drop: Arc<dyn Fn(String) + Send + Sync>, _on_open: Arc<dyn Fn() + Send + Sync>) {}

/// Would close it.
pub fn close() {}
