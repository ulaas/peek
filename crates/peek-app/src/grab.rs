//! Keyboard grab: while a session has focus, keys the OS would normally
//! handle itself (Windows key, Alt+Tab, Ctrl+Esc, Print Screen) go to the
//! remote computer instead.
//!
//! On Windows this is a low-level keyboard hook (`WH_KEYBOARD_LL`), installed
//! only while needed. In [`GrabMode::All`] every key is captured as a real
//! scancode, which gives RDP exact fidelity (left/right modifiers, Caps Lock,
//! Num Lock). In [`GrabMode::System`] only the system shortcuts are captured
//! and ordinary typing keeps flowing through egui, which VNC needs for
//! layout-aware text.
//!
//! The session view also forwards whatever keys still reach egui, in every
//! mode: the hook skips injected input (remote-control tools, on-screen
//! keyboards) and Windows can drop a slow hook without notice, so egui is
//! the fallback that keeps typing working.

use peek_core::KeyCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrabMode {
    Off,
    System,
    All,
}

/// A captured key: physical key and whether it went down.
pub type GrabbedKey = (KeyCode, bool);

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP,
        SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    };

    static HOOK: AtomicIsize = AtomicIsize::new(0);
    static MODE: AtomicU8 = AtomicU8::new(0);
    static SINK: Mutex<Option<(Sender<GrabbedKey>, egui::Context)>> = Mutex::new(None);

    const VK_TAB: u32 = 0x09;
    const VK_ESCAPE: u32 = 0x1B;
    const VK_SNAPSHOT: u32 = 0x2C;
    const VK_LWIN: u32 = 0x5B;
    const VK_RWIN: u32 = 0x5C;
    const VK_APPS: u32 = 0x5D;
    const VK_F4: u32 = 0x73;
    const VK_CONTROL_HELD: u32 = 0x11;

    fn ctrl_down() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        // SAFETY: plain query of key state, no pointers involved.
        unsafe { GetAsyncKeyState(VK_CONTROL_HELD as i32) < 0 }
    }

    fn to_keycode(scan: u32, extended: bool) -> Option<KeyCode> {
        let scan = scan as u8;
        // Print Screen arrives as scan 0x37 extended, Right Shift as 0x36 not extended.
        KeyCode::ALL
            .iter()
            .copied()
            .find(|k| k.scancode() == (scan, extended))
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
            let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let mode = MODE.load(Ordering::Relaxed);
            if k.flags & LLKHF_INJECTED == 0 && mode != 0 {
                let alt = k.flags & LLKHF_ALTDOWN != 0;
                let system_combo = match k.vkCode {
                    VK_LWIN | VK_RWIN | VK_APPS | VK_SNAPSHOT => true,
                    VK_TAB | VK_F4 => alt,
                    VK_ESCAPE => alt || ctrl_down(),
                    _ => false,
                };
                let take = mode == 2 || system_combo;
                if take {
                    let extended = k.flags & LLKHF_EXTENDED != 0;
                    if let Some(key) = to_keycode(k.scanCode, extended) {
                        if let Ok(g) = SINK.lock()
                            && let Some((tx, ctx)) = g.as_ref()
                        {
                            let _ = tx.send((key, k.flags & LLKHF_UP == 0));
                            ctx.request_repaint();
                        }
                        return 1; // swallow: the local OS never sees it
                    }
                }
            }
        }
        // SAFETY: forwarding to the next hook with the arguments we received.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    pub struct Grab {
        rx: Receiver<GrabbedKey>,
        mode: GrabMode,
    }

    impl Grab {
        pub fn new(ctx: &egui::Context) -> Self {
            let (tx, rx) = channel();
            if let Ok(mut g) = SINK.lock() {
                *g = Some((tx, ctx.clone()));
            }
            Self {
                rx,
                mode: GrabMode::Off,
            }
        }

        pub fn set_mode(&mut self, mode: GrabMode) {
            if mode == self.mode {
                return;
            }
            self.mode = mode;
            MODE.store(
                match mode {
                    GrabMode::Off => 0,
                    GrabMode::System => 1,
                    GrabMode::All => 2,
                },
                Ordering::Relaxed,
            );
            let installed = HOOK.load(Ordering::Relaxed);
            match (mode, installed) {
                (GrabMode::Off, h) if h != 0 => {
                    // SAFETY: h is the handle returned by SetWindowsHookExW below.
                    unsafe { UnhookWindowsHookEx(h as _) };
                    HOOK.store(0, Ordering::Relaxed);
                }
                (GrabMode::System | GrabMode::All, 0) => {
                    // SAFETY: installs a hook procedure from this module; it is
                    // removed again in set_mode(Off) / Drop.
                    let h = unsafe {
                        SetWindowsHookExW(
                            WH_KEYBOARD_LL,
                            Some(hook_proc),
                            GetModuleHandleW(std::ptr::null()),
                            0,
                        )
                    };
                    if h.is_null() {
                        tracing::warn!("keyboard grab unavailable");
                    } else {
                        HOOK.store(h as isize, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
        }

        pub fn drain(&self) -> Vec<GrabbedKey> {
            self.rx.try_iter().collect()
        }
    }

    impl Drop for Grab {
        fn drop(&mut self) {
            self.set_mode(GrabMode::Off);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    /// Keyboard grab is Windows-only for now; elsewhere this is inert.
    pub struct Grab;

    impl Grab {
        pub fn new(_: &egui::Context) -> Self {
            Self
        }
        pub fn set_mode(&mut self, _: GrabMode) {}
        pub fn drain(&self) -> Vec<GrabbedKey> {
            Vec::new()
        }
    }
}

pub use imp::Grab;
