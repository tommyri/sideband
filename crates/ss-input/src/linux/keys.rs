use crate::{Code, InputEvent, MouseButton};

#[allow(clippy::too_many_lines, reason = "physical Linux input code table")]
pub(super) fn evdev_key(code: u16) -> Option<Code> {
    Some(match code {
        0x01 => Code::Escape,
        0x02 => Code::Digit1,
        0x03 => Code::Digit2,
        0x04 => Code::Digit3,
        0x05 => Code::Digit4,
        0x06 => Code::Digit5,
        0x07 => Code::Digit6,
        0x08 => Code::Digit7,
        0x09 => Code::Digit8,
        0x0a => Code::Digit9,
        0x0b => Code::Digit0,
        0x0c => Code::Minus,
        0x0d => Code::Equal,
        0x0e => Code::Backspace,
        0x0f => Code::Tab,
        0x10 => Code::KeyQ,
        0x11 => Code::KeyW,
        0x12 => Code::KeyE,
        0x13 => Code::KeyR,
        0x14 => Code::KeyT,
        0x15 => Code::KeyY,
        0x16 => Code::KeyU,
        0x17 => Code::KeyI,
        0x18 => Code::KeyO,
        0x19 => Code::KeyP,
        0x1a => Code::BracketLeft,
        0x1b => Code::BracketRight,
        0x1c => Code::Enter,
        0x1d => Code::ControlLeft,
        0x1e => Code::KeyA,
        0x1f => Code::KeyS,
        0x20 => Code::KeyD,
        0x21 => Code::KeyF,
        0x22 => Code::KeyG,
        0x23 => Code::KeyH,
        0x24 => Code::KeyJ,
        0x25 => Code::KeyK,
        0x26 => Code::KeyL,
        0x27 => Code::Semicolon,
        0x28 => Code::Quote,
        0x29 => Code::Backquote,
        0x2a => Code::ShiftLeft,
        0x2b => Code::Backslash,
        0x2c => Code::KeyZ,
        0x2d => Code::KeyX,
        0x2e => Code::KeyC,
        0x2f => Code::KeyV,
        0x30 => Code::KeyB,
        0x31 => Code::KeyN,
        0x32 => Code::KeyM,
        0x33 => Code::Comma,
        0x34 => Code::Period,
        0x35 => Code::Slash,
        0x36 => Code::ShiftRight,
        0x37 => Code::NumpadMultiply,
        0x38 => Code::AltLeft,
        0x39 => Code::Space,
        0x3a => Code::CapsLock,
        0x3b => Code::F1,
        0x3c => Code::F2,
        0x3d => Code::F3,
        0x3e => Code::F4,
        0x3f => Code::F5,
        0x40 => Code::F6,
        0x41 => Code::F7,
        0x42 => Code::F8,
        0x43 => Code::F9,
        0x44 => Code::F10,
        0x45 => Code::NumLock,
        0x46 => Code::ScrollLock,
        0x47 => Code::Numpad7,
        0x48 => Code::Numpad8,
        0x49 => Code::Numpad9,
        0x4a => Code::NumpadSubtract,
        0x4b => Code::Numpad4,
        0x4c => Code::Numpad5,
        0x4d => Code::Numpad6,
        0x4e => Code::NumpadAdd,
        0x4f => Code::Numpad1,
        0x50 => Code::Numpad2,
        0x51 => Code::Numpad3,
        0x52 => Code::Numpad0,
        0x53 => Code::NumpadDecimal,
        0x56 => Code::IntlBackslash,
        0x57 => Code::F11,
        0x58 => Code::F12,
        96 => Code::NumpadEnter,
        97 => Code::ControlRight,
        98 => Code::NumpadDivide,
        99 => Code::PrintScreen,
        100 => Code::AltRight,
        102 => Code::Home,
        103 => Code::ArrowUp,
        104 => Code::PageUp,
        105 => Code::ArrowLeft,
        106 => Code::ArrowRight,
        107 => Code::End,
        108 => Code::ArrowDown,
        109 => Code::PageDown,
        110 => Code::Insert,
        111 => Code::Delete,
        119 => Code::Pause,
        125 => Code::MetaLeft,
        126 => Code::MetaRight,
        127 => Code::ContextMenu,
        183..=194 => return Code::from_hid_usage(0x68 + code - 183),
        _ => return None,
    })
}

pub(super) fn evdev_event(code: u16, pressed: bool) -> Option<InputEvent> {
    let mouse = match code {
        272 => Some(MouseButton::Left),
        273 => Some(MouseButton::Right),
        274 => Some(MouseButton::Middle),
        275 => Some(MouseButton::Mouse4),
        276 => Some(MouseButton::Mouse5),
        _ => None,
    };
    mouse
        .map(|button| InputEvent::mouse(button, pressed))
        .or_else(|| evdev_key(code).map(|code| InputEvent::key(code, pressed)))
}

pub(super) fn xkb_name(name: [u8; 4]) -> Option<Code> {
    const ROWS: &[(&str, &[u16])] = &[
        ("AE", &[2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]),
        ("AD", &[16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27]),
        ("AC", &[30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40]),
        ("AB", &[44, 45, 46, 47, 48, 49, 50, 51, 52, 53]),
    ];
    let name = std::str::from_utf8(&name).ok()?.trim_end_matches('\0');
    for (prefix, row) in ROWS {
        if let Some(index) = name
            .strip_prefix(prefix)
            .and_then(|s| s.parse::<usize>().ok())
        {
            return row
                .get(index.checked_sub(1)?)
                .and_then(|code| evdev_key(*code));
        }
    }
    if let Some(index) = name.strip_prefix("FK").and_then(|s| s.parse::<u16>().ok()) {
        return match index {
            1..=10 => evdev_key(58 + index),
            11..=12 => evdev_key(76 + index),
            13..=24 => evdev_key(170 + index),
            _ => None,
        };
    }
    let code = match name {
        "ESC" => 1,
        "TLDE" => 41,
        "BKSP" => 14,
        "TAB" => 15,
        "RTRN" => 28,
        "LCTL" => 29,
        "LFSH" => 42,
        "BKSL" => 43,
        "RTSH" => 54,
        "LALT" => 56,
        "SPCE" => 57,
        "CAPS" => 58,
        "NMLK" => 69,
        "SCLK" => 70,
        "LSGT" => 86,
        "KPEN" => 96,
        "RCTL" => 97,
        "KPDV" => 98,
        "PRSC" => 99,
        "RALT" => 100,
        "HOME" => 102,
        "UP" => 103,
        "PGUP" => 104,
        "LEFT" => 105,
        "RGHT" => 106,
        "END" => 107,
        "DOWN" => 108,
        "PGDN" => 109,
        "INS" => 110,
        "DELE" => 111,
        "PAUS" => 119,
        "LWIN" => 125,
        "RWIN" => 126,
        "MENU" => 127,
        "KPMU" => 55,
        "KPSU" => 74,
        "KPAD" => 78,
        "KPDL" => 83,
        "KP0" => 82,
        "KP1" => 79,
        "KP2" => 80,
        "KP3" => 81,
        "KP4" => 75,
        "KP5" => 76,
        "KP6" => 77,
        "KP7" => 71,
        "KP8" => 72,
        "KP9" => 73,
        _ => return None,
    };
    evdev_key(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_physical_keys_and_ignores_wheels() {
        assert_eq!(xkb_name(*b"AD01"), Some(Code::KeyQ));
        assert_eq!(xkb_name(*b"RCTL"), Some(Code::ControlRight));
        assert_eq!(xkb_name(*b"FK24"), Some(Code::F24));
        assert_eq!(
            evdev_event(276, false),
            Some(InputEvent::mouse_up(MouseButton::Mouse5))
        );
        assert_eq!(evdev_event(8_000, true), None);
    }
}
