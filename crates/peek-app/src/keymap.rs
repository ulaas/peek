//! egui keys to physical [`KeyCode`]s.
//!
//! egui reports the *physical* key position (US names) in `physical_key`,
//! which is what RDP scancodes need. Keys egui does not model at all
//! (Windows key, Caps Lock, Print Screen, the modifiers themselves) are
//! handled by the modifier tracking in `view.rs` and, later, a low-level
//! keyboard hook.

use egui::Key as K;
use peek_core::KeyCode as C;

pub fn map(key: K) -> Option<C> {
    Some(match key {
        K::ArrowDown => C::ArrowDown,
        K::ArrowLeft => C::ArrowLeft,
        K::ArrowRight => C::ArrowRight,
        K::ArrowUp => C::ArrowUp,
        K::Escape => C::Escape,
        K::Tab => C::Tab,
        K::Backspace => C::Backspace,
        K::Enter => C::Enter,
        K::Space => C::Space,
        K::Insert => C::Insert,
        K::Delete => C::Delete,
        K::Home => C::Home,
        K::End => C::End,
        K::PageUp => C::PageUp,
        K::PageDown => C::PageDown,
        K::Comma => C::Comma,
        K::Backslash | K::Pipe => C::Backslash,
        K::Slash | K::Questionmark => C::Slash,
        K::OpenBracket | K::OpenCurlyBracket => C::BracketLeft,
        K::CloseBracket | K::CloseCurlyBracket => C::BracketRight,
        K::Backtick => C::Backquote,
        K::Minus => C::Minus,
        K::Period => C::Period,
        K::Plus | K::Equals => C::Equal,
        K::Semicolon | K::Colon => C::Semicolon,
        K::Quote => C::Quote,
        K::Exclamationmark => C::Digit1,
        K::Num0 => C::Digit0,
        K::Num1 => C::Digit1,
        K::Num2 => C::Digit2,
        K::Num3 => C::Digit3,
        K::Num4 => C::Digit4,
        K::Num5 => C::Digit5,
        K::Num6 => C::Digit6,
        K::Num7 => C::Digit7,
        K::Num8 => C::Digit8,
        K::Num9 => C::Digit9,
        K::A => C::KeyA,
        K::B => C::KeyB,
        K::C => C::KeyC,
        K::D => C::KeyD,
        K::E => C::KeyE,
        K::F => C::KeyF,
        K::G => C::KeyG,
        K::H => C::KeyH,
        K::I => C::KeyI,
        K::J => C::KeyJ,
        K::K => C::KeyK,
        K::L => C::KeyL,
        K::M => C::KeyM,
        K::N => C::KeyN,
        K::O => C::KeyO,
        K::P => C::KeyP,
        K::Q => C::KeyQ,
        K::R => C::KeyR,
        K::S => C::KeyS,
        K::T => C::KeyT,
        K::U => C::KeyU,
        K::V => C::KeyV,
        K::W => C::KeyW,
        K::X => C::KeyX,
        K::Y => C::KeyY,
        K::Z => C::KeyZ,
        K::F1 => C::F1,
        K::F2 => C::F2,
        K::F3 => C::F3,
        K::F4 => C::F4,
        K::F5 => C::F5,
        K::F6 => C::F6,
        K::F7 => C::F7,
        K::F8 => C::F8,
        K::F9 => C::F9,
        K::F10 => C::F10,
        K::F11 => C::F11,
        K::F12 => C::F12,
        _ => return None,
    })
}

/// The keyboard layout to announce to RDP servers (Windows KLID).
pub fn local_keyboard_layout() -> u32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
        // SAFETY: GetKeyboardLayout(0) has no preconditions; it returns the
        // calling thread's active layout handle.
        let hkl = unsafe { GetKeyboardLayout(0) } as usize;
        let lang = (hkl & 0xFFFF) as u32;
        if lang != 0 {
            return lang;
        }
    }
    0x0409
}
