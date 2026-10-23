//! Protocol-neutral input events.
//!
//! Keys are *physical* keys named after the US layout (like the W3C
//! `KeyboardEvent.code` values). RDP wants PC scancodes, VNC wants X11
//! keysyms; both mappings live here so every backend agrees.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Key {
        code: KeyCode,
        down: bool,
    },
    /// Text produced by the local keyboard layout / IME. Backends that send
    /// physical keys (RDP) normally ignore it; VNC uses it for printable keys.
    Text(String),
    /// Absolute pointer position in remote framebuffer pixels.
    PointerMove {
        x: u32,
        y: u32,
    },
    Button {
        button: MouseButton,
        down: bool,
        x: u32,
        y: u32,
    },
    /// Wheel movement in Windows WHEEL_DELTA units (120 = one notch).
    Wheel {
        dx: i32,
        dy: i32,
        x: u32,
        y: u32,
    },
    /// Release every key and button the backend believes is held.
    ReleaseAll,
}

macro_rules! keycodes {
    ($( $name:ident = $sc:expr, $ext:expr, $sym:expr; )*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum KeyCode { $($name),* }

        impl KeyCode {
            pub const ALL: &'static [KeyCode] = &[$(KeyCode::$name),*];

            /// PC/AT scancode set 1 and whether it needs the 0xE0 prefix.
            pub const fn scancode(self) -> (u8, bool) {
                match self { $(KeyCode::$name => ($sc, $ext)),* }
            }

            /// X11 keysym for the unshifted key on a US layout.
            pub const fn keysym(self) -> u32 {
                match self { $(KeyCode::$name => $sym),* }
            }
        }
    };
}

keycodes! {
    Escape = 0x01, false, 0xff1b;
    Digit1 = 0x02, false, 0x31; Digit2 = 0x03, false, 0x32; Digit3 = 0x04, false, 0x33;
    Digit4 = 0x05, false, 0x34; Digit5 = 0x06, false, 0x35; Digit6 = 0x07, false, 0x36;
    Digit7 = 0x08, false, 0x37; Digit8 = 0x09, false, 0x38; Digit9 = 0x0A, false, 0x39;
    Digit0 = 0x0B, false, 0x30;
    Minus = 0x0C, false, 0x2d; Equal = 0x0D, false, 0x3d;
    Backspace = 0x0E, false, 0xff08; Tab = 0x0F, false, 0xff09;
    KeyQ = 0x10, false, 0x71; KeyW = 0x11, false, 0x77; KeyE = 0x12, false, 0x65;
    KeyR = 0x13, false, 0x72; KeyT = 0x14, false, 0x74; KeyY = 0x15, false, 0x79;
    KeyU = 0x16, false, 0x75; KeyI = 0x17, false, 0x69; KeyO = 0x18, false, 0x6f;
    KeyP = 0x19, false, 0x70;
    BracketLeft = 0x1A, false, 0x5b; BracketRight = 0x1B, false, 0x5d;
    Enter = 0x1C, false, 0xff0d; ControlLeft = 0x1D, false, 0xffe3;
    KeyA = 0x1E, false, 0x61; KeyS = 0x1F, false, 0x73; KeyD = 0x20, false, 0x64;
    KeyF = 0x21, false, 0x66; KeyG = 0x22, false, 0x67; KeyH = 0x23, false, 0x68;
    KeyJ = 0x24, false, 0x6a; KeyK = 0x25, false, 0x6b; KeyL = 0x26, false, 0x6c;
    Semicolon = 0x27, false, 0x3b; Quote = 0x28, false, 0x27; Backquote = 0x29, false, 0x60;
    ShiftLeft = 0x2A, false, 0xffe1; Backslash = 0x2B, false, 0x5c;
    KeyZ = 0x2C, false, 0x7a; KeyX = 0x2D, false, 0x78; KeyC = 0x2E, false, 0x63;
    KeyV = 0x2F, false, 0x76; KeyB = 0x30, false, 0x62; KeyN = 0x31, false, 0x6e;
    KeyM = 0x32, false, 0x6d;
    Comma = 0x33, false, 0x2c; Period = 0x34, false, 0x2e; Slash = 0x35, false, 0x2f;
    ShiftRight = 0x36, false, 0xffe2; NumpadMultiply = 0x37, false, 0xffaa;
    AltLeft = 0x38, false, 0xffe9; Space = 0x39, false, 0x20; CapsLock = 0x3A, false, 0xffe5;
    F1 = 0x3B, false, 0xffbe; F2 = 0x3C, false, 0xffbf; F3 = 0x3D, false, 0xffc0;
    F4 = 0x3E, false, 0xffc1; F5 = 0x3F, false, 0xffc2; F6 = 0x40, false, 0xffc3;
    F7 = 0x41, false, 0xffc4; F8 = 0x42, false, 0xffc5; F9 = 0x43, false, 0xffc6;
    F10 = 0x44, false, 0xffc7; F11 = 0x57, false, 0xffc8; F12 = 0x58, false, 0xffc9;
    NumLock = 0x45, false, 0xff7f; ScrollLock = 0x46, false, 0xff14;
    Numpad7 = 0x47, false, 0xffb7; Numpad8 = 0x48, false, 0xffb8; Numpad9 = 0x49, false, 0xffb9;
    NumpadSubtract = 0x4A, false, 0xffad;
    Numpad4 = 0x4B, false, 0xffb4; Numpad5 = 0x4C, false, 0xffb5; Numpad6 = 0x4D, false, 0xffb6;
    NumpadAdd = 0x4E, false, 0xffab;
    Numpad1 = 0x4F, false, 0xffb1; Numpad2 = 0x50, false, 0xffb2; Numpad3 = 0x51, false, 0xffb3;
    Numpad0 = 0x52, false, 0xffb0; NumpadDecimal = 0x53, false, 0xffae;
    IntlBackslash = 0x56, false, 0x3c;
    NumpadEnter = 0x1C, true, 0xff8d; ControlRight = 0x1D, true, 0xffe4;
    NumpadDivide = 0x35, true, 0xffaf; PrintScreen = 0x37, true, 0xff61;
    AltRight = 0x38, true, 0xffea;
    Home = 0x47, true, 0xff50; ArrowUp = 0x48, true, 0xff52; PageUp = 0x49, true, 0xff55;
    ArrowLeft = 0x4B, true, 0xff51; ArrowRight = 0x4D, true, 0xff53;
    End = 0x4F, true, 0xff57; ArrowDown = 0x50, true, 0xff54; PageDown = 0x51, true, 0xff56;
    Insert = 0x52, true, 0xff63; Delete = 0x53, true, 0xffff;
    MetaLeft = 0x5B, true, 0xffeb; MetaRight = 0x5C, true, 0xffec; ContextMenu = 0x5D, true, 0xff67;
}

impl KeyCode {
    /// Keys whose effect is to type a character. When no Ctrl/Alt/Meta is
    /// held these arrive as [`InputEvent::Text`] too.
    pub fn is_printable(self) -> bool {
        use KeyCode::*;
        let ks = self.keysym();
        (0x20..0x7f).contains(&ks)
            || matches!(
                self,
                Numpad0
                    | Numpad1
                    | Numpad2
                    | Numpad3
                    | Numpad4
                    | Numpad5
                    | Numpad6
                    | Numpad7
                    | Numpad8
                    | Numpad9
                    | NumpadDecimal
                    | NumpadAdd
                    | NumpadSubtract
                    | NumpadMultiply
                    | NumpadDivide
            )
    }

    pub fn is_modifier(self) -> bool {
        use KeyCode::*;
        matches!(
            self,
            ShiftLeft
                | ShiftRight
                | ControlLeft
                | ControlRight
                | AltLeft
                | AltRight
                | MetaLeft
                | MetaRight
        )
    }
}

/// X11 keysym for a Unicode character.
pub fn char_to_keysym(c: char) -> u32 {
    let cp = c as u32;
    match c {
        ' '..='~' | '\u{a0}'..='\u{ff}' => cp,
        '\n' | '\r' => 0xff0d,
        '\t' => 0xff09,
        '\u{8}' => 0xff08,
        _ => 0x0100_0000 | cp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancodes_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for k in KeyCode::ALL {
            assert!(seen.insert(k.scancode()), "duplicate scancode for {k:?}");
        }
    }

    #[test]
    fn keysym_for_chars() {
        assert_eq!(char_to_keysym('a'), 0x61);
        assert_eq!(char_to_keysym('é'), 0xe9);
        assert_eq!(char_to_keysym('ş'), 0x0100_015f);
    }
}
