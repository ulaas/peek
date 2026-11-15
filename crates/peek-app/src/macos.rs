//! macOS integration: files opened from Finder.
//!
//! Finder does not pass a double-clicked `.peekrec` or `.rdp` file on the
//! command line; it sends an Apple Event, which AppKit hands to the
//! application delegate's `application:openURLs:`. winit's delegate does not
//! implement that method, so it is added to winit's delegate class here,
//! after the event loop exists and before it runs (the files a cold launch
//! opens arrive before the first frame). The paths wait in a queue that the
//! app drains every frame.

use std::sync::{Mutex, OnceLock};

use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{ffi, sel};
use objc2_foundation::{NSArray, NSURL};

static OPENED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

extern "C-unwind" fn open_urls(_this: &AnyObject, _sel: Sel, _app: &AnyObject, urls: &NSArray<NSURL>) {
    let paths: Vec<String> =
        urls.iter().filter(|u| u.isFileURL()).filter_map(|u| u.path()).map(|p| p.to_string()).collect();
    if paths.is_empty() {
        return;
    }
    tracing::info!("opened from Finder: {paths:?}");
    OPENED.lock().unwrap_or_else(|e| e.into_inner()).extend(paths);
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

/// Teach winit's application delegate `application:openURLs:`. Call once the
/// event loop has been created (that registers the class) and before it runs.
pub fn install_open_handler() {
    let Some(class) = AnyClass::get(c"WinitApplicationDelegate") else {
        tracing::warn!("no WinitApplicationDelegate class; files opened from Finder are ignored");
        return;
    };
    let method: extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, &NSArray<NSURL>) = open_urls;
    // SAFETY: the class is registered; the implementation's signature matches the type
    // encoding (void return, self, _cmd, NSApplication *, NSArray<NSURL *> *), and adding a
    // method winit does not define changes nothing else about the class.
    let added = unsafe {
        let imp: Imp = std::mem::transmute(method);
        ffi::class_addMethod((class as *const AnyClass).cast_mut(), sel!(application:openURLs:), imp, c"v@:@@".as_ptr())
    };
    if !added.as_bool() {
        tracing::warn!("could not add application:openURLs:; files opened from Finder are ignored");
    }
}

/// Lets a file opened while Peek is idle wake it up.
pub fn set_context(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// Files opened from Finder since the last call.
pub fn take_opened() -> Vec<String> {
    std::mem::take(&mut *OPENED.lock().unwrap_or_else(|e| e.into_inner()))
}
