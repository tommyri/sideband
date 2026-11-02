//! Windows scan-code and button mapping.

use crate::{Code, InputEvent, MouseButton};

const RI_KEY_BREAK: u16 = 0x01;
const RI_KEY_E0: u16 = 0x02;

/// Decode one `RAWKEYBOARD` record and the virtual key used by the stuck-key
/// guard. Scan codes preserve physical positions across keyboard layouts.
pub(super) fn keyboard(scan: u16, flags: u16, virtual_key: u16) -> Option<(InputEvent, i32)> {
    let code = key(scan, flags & RI_KEY_E0 != 0, virtual_key)?;
    let pressed = flags & RI_KEY_BREAK == 0;
    Some((InputEvent::key(code, pressed), guard_key(code, virtual_key)))
}

pub(super) fn mouse(flags: u16) -> impl Iterator<Item = (InputEvent, i32)> {
    const EDGES: &[(u16, MouseButton, bool, i32)] = &[
        (0x0001, MouseButton::Left, true, 0x01),
        (0x0002, MouseButton::Left, false, 0x01),
        (0x0004, MouseButton::Right, true, 0x02),
        (0x0008, MouseButton::Right, false, 0x02),
        (0x0010, MouseButton::Middle, true, 0x04),
        (0x0020, MouseButton::Middle, false, 0x04),
        (0x0040, MouseButton::Mouse4, true, 0x05),
        (0x0080, MouseButton::Mouse4, false, 0x05),
        (0x0100, MouseButton::Mouse5, true, 0x06),
        (0x0200, MouseButton::Mouse5, false, 0x06),
    ];
    EDGES
        .iter()
        .filter_map(move |&(mask, button, pressed, vk)| {
            (flags & mask != 0).then_some((InputEvent::mouse(button, pressed), vk))
        })
}

#[allow(
    clippy::too_many_lines,
    reason = "the Set 1 scan-code table is clearest as one exhaustive mapping"
)]
fn key(scan: u16, extended: bool, virtual_key: u16) -> Option<Code> {
    if virtual_key == 0x13 {
        return Some(Code::Pause);
    }
    if virtual_key == 0x2c {
        return Some(Code::PrintScreen);
    }
    if (0x7c..=0x87).contains(&virtual_key) {
        return Code::from_hid_usage(0x68 + virtual_key - 0x7c);
    }
    if extended {
        return Some(match scan {
            0x1c => Code::NumpadEnter,
            0x1d => Code::ControlRight,
            0x35 => Code::NumpadDivide,
            0x38 => Code::AltRight,
            0x47 => Code::Home,
            0x48 => Code::ArrowUp,
            0x49 => Code::PageUp,
            0x4b => Code::ArrowLeft,
            0x4d => Code::ArrowRight,
            0x4f => Code::End,
            0x50 => Code::ArrowDown,
            0x51 => Code::PageDown,
            0x52 => Code::Insert,
            0x53 => Code::Delete,
            0x5b => Code::MetaLeft,
            0x5c => Code::MetaRight,
            0x5d => Code::ContextMenu,
            _ => return None,
        });
    }
    Some(match scan {
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
        _ => return None,
    })
}

fn guard_key(code: Code, virtual_key: u16) -> i32 {
    match code {
        Code::ShiftLeft => 0xa0,
        Code::ShiftRight => 0xa1,
        Code::ControlLeft => 0xa2,
        Code::ControlRight => 0xa3,
        Code::AltLeft => 0xa4,
        Code::AltRight => 0xa5,
        Code::MetaLeft => 0x5b,
        Code::MetaRight => 0x5c,
        _ => i32::from(virtual_key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_layout_extended_keys_and_mouse_map() {
        assert_eq!(
            keyboard(0x10, 0, 0x51).unwrap().0,
            InputEvent::key_down(Code::KeyQ)
        );
        assert_eq!(
            keyboard(0x1d, RI_KEY_E0, 0x11).unwrap().0,
            InputEvent::key_down(Code::ControlRight)
        );
        assert_eq!(
            keyboard(0x53, RI_KEY_E0 | RI_KEY_BREAK, 0x2e).unwrap().0,
            InputEvent::key_up(Code::Delete)
        );
        assert_eq!(
            keyboard(0, 0, 0x7c).unwrap().0,
            InputEvent::key_down(Code::F13)
        );
        assert_eq!(
            mouse(0x0040 | 0x0200)
                .map(|(event, _)| event)
                .collect::<Vec<_>>(),
            [
                InputEvent::mouse_down(MouseButton::Mouse4),
                InputEvent::mouse_up(MouseButton::Mouse5),
            ]
        );
    }
}
