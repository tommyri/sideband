//! Carbon virtual-key positions to USB HID codes. No layout/text translation.
use crate::{Code, InputEvent, MouseButton};

// Carbon Events.h, kVK_* constants. Unmapped media/Fn/JIS-only keys are ignored.
#[allow(
    clippy::too_many_lines,
    reason = "one auditable Carbon-to-HID mapping table"
)]
pub(super) fn key(code: u16) -> Option<Code> {
    use Code::{self as C};
    Some(match code {
        0 => C::KeyA,
        1 => C::KeyS,
        2 => C::KeyD,
        3 => C::KeyF,
        4 => C::KeyH,
        5 => C::KeyG,
        6 => C::KeyZ,
        7 => C::KeyX,
        8 => C::KeyC,
        9 => C::KeyV,
        10 => C::IntlBackslash,
        11 => C::KeyB,
        12 => C::KeyQ,
        13 => C::KeyW,
        14 => C::KeyE,
        15 => C::KeyR,
        16 => C::KeyY,
        17 => C::KeyT,
        18 => C::Digit1,
        19 => C::Digit2,
        20 => C::Digit3,
        21 => C::Digit4,
        22 => C::Digit6,
        23 => C::Digit5,
        24 => C::Equal,
        25 => C::Digit9,
        26 => C::Digit7,
        27 => C::Minus,
        28 => C::Digit8,
        29 => C::Digit0,
        30 => C::BracketRight,
        31 => C::KeyO,
        32 => C::KeyU,
        33 => C::BracketLeft,
        34 => C::KeyI,
        35 => C::KeyP,
        36 => C::Enter,
        37 => C::KeyL,
        38 => C::KeyJ,
        39 => C::Quote,
        40 => C::KeyK,
        41 => C::Semicolon,
        42 => C::Backslash,
        43 => C::Comma,
        44 => C::Slash,
        45 => C::KeyN,
        46 => C::KeyM,
        47 => C::Period,
        48 => C::Tab,
        49 => C::Space,
        50 => C::Backquote,
        51 => C::Backspace,
        53 => C::Escape,
        54 => C::MetaRight,
        55 => C::MetaLeft,
        56 => C::ShiftLeft,
        57 => C::CapsLock,
        58 => C::AltLeft,
        59 => C::ControlLeft,
        60 => C::ShiftRight,
        61 => C::AltRight,
        62 => C::ControlRight,
        64 => C::F17,
        65 => C::NumpadDecimal,
        67 => C::NumpadMultiply,
        69 => C::NumpadAdd,
        71 => C::NumLock,
        75 => C::NumpadDivide,
        76 => C::NumpadEnter,
        78 => C::NumpadSubtract,
        79 => C::F18,
        80 => C::F19,
        81 => C::NumpadEqual,
        82 => C::Numpad0,
        83 => C::Numpad1,
        84 => C::Numpad2,
        85 => C::Numpad3,
        86 => C::Numpad4,
        87 => C::Numpad5,
        88 => C::Numpad6,
        89 => C::Numpad7,
        91 => C::Numpad8,
        92 => C::Numpad9,
        96 => C::F5,
        97 => C::F6,
        98 => C::F7,
        99 => C::F3,
        100 => C::F8,
        101 => C::F9,
        103 => C::F11,
        105 => C::F13,
        106 => C::F16,
        107 => C::F14,
        109 => C::F10,
        111 => C::F12,
        113 => C::F15,
        114 => C::Insert,
        115 => C::Home,
        116 => C::PageUp,
        117 => C::Delete,
        118 => C::F4,
        119 => C::End,
        120 => C::F2,
        121 => C::PageDown,
        122 => C::F1,
        123 => C::ArrowLeft,
        124 => C::ArrowRight,
        125 => C::ArrowDown,
        126 => C::ArrowUp,
        90 => C::F20,
        _ => return None,
    })
}
pub(super) fn mouse(number: i64) -> Option<MouseButton> {
    Some(match number {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        3 => MouseButton::Mouse4,
        4 => MouseButton::Mouse5,
        _ => return None,
    })
}

// NX_DEVICE* masks from IOKit/hidsystem/IOLLEvent.h, paired with CG aggregate flags.
const MODIFIERS: [(u16, u64, u64); 8] = [
    (59, 0x0001, 1 << 18),
    (62, 0x2000, 1 << 18),
    (56, 0x0002, 1 << 17),
    (60, 0x0004, 1 << 17),
    (58, 0x0020, 1 << 19),
    (61, 0x0040, 1 << 19),
    (55, 0x0008, 1 << 20),
    (54, 0x0010, 1 << 20),
];

pub(super) struct Decoder {
    keys: [bool; 128],
    buttons: [bool; 5],
}
impl Default for Decoder {
    fn default() -> Self {
        Self {
            keys: [false; 128],
            buttons: [false; 5],
        }
    }
}
impl Decoder {
    pub fn key(&mut self, code: u16, down: bool) -> Option<InputEvent> {
        let mapped = key(code)?;
        let old = self.keys.get_mut(usize::from(code))?;
        if *old == down {
            return None;
        }
        *old = down;
        Some(InputEvent::key(mapped, down))
    }
    pub fn mouse(&mut self, number: i64, down: bool) -> Option<InputEvent> {
        let mapped = mouse(number)?;
        let old = self.buttons.get_mut(usize::try_from(number).ok()?)?;
        if *old == down {
            return None;
        }
        *old = down;
        Some(InputEvent::mouse(mapped, down))
    }
    /// Snapshot side-specific modifiers on every event, including a listener
    /// started while a modifier was already held. Never toggle aggregate flags:
    /// releasing left Shift while right Shift is held must release only left.
    pub fn modifiers(&mut self, flags: u64, mut emit: impl FnMut(InputEvent)) {
        for (code, side, aggregate) in MODIFIERS {
            if let Some(event) = self.key(code, flags & side != 0 && flags & aggregate != 0) {
                emit(event);
            }
        }
    }
    /// Repair releases missed while the tap was disabled. Held inputs retain
    /// their suppression until an up; do not invent a fresh down on recovery.
    pub fn reconcile(
        &mut self,
        mut key_down: impl FnMut(u16) -> bool,
        mut mouse_down: impl FnMut(u8) -> bool,
        mut emit: impl FnMut(InputEvent),
    ) {
        for code in 0_u16..128 {
            if self.keys[usize::from(code)]
                && !key_down(code)
                && let Some(event) = self.key(code, false)
            {
                emit(event);
            }
        }
        for button in 0_u8..5 {
            if self.buttons[usize::from(button)]
                && !mouse_down(button)
                && let Some(event) = self.mouse(i64::from(button), false)
            {
                emit(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn carbon_keys_are_physical_and_mouse_buttons_are_zero_based() {
        assert_eq!(key(105), Some(Code::F13));
        assert_eq!(key(9), Some(Code::KeyV));
        assert_eq!(key(51), Some(Code::Backspace));
        assert_eq!(key(117), Some(Code::Delete));
        assert_eq!(key(10), Some(Code::IntlBackslash));
        assert_eq!(key(63), None); // Fn has no HID keyboard Code.
        assert_eq!(key(u16::MAX), None);
        assert_eq!(mouse(3), Some(MouseButton::Mouse4));
        assert_eq!(mouse(4), Some(MouseButton::Mouse5));
        assert_eq!(mouse(-1), None);
        assert_eq!(mouse(5), None);
        let mut unique = std::collections::HashSet::new();
        for carbon in 0..128 {
            if let Some(mapped) = key(carbon) {
                assert!(unique.insert(mapped));
            }
        }
    }
    #[test]
    fn both_sides_of_a_modifier_are_independent_and_duplicates_are_ignored() {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        let shift = 1 << 17;
        decoder.modifiers(shift | 2, |e| events.push(e));
        decoder.modifiers(shift | 6, |e| events.push(e));
        decoder.modifiers(shift | 4, |e| events.push(e));
        decoder.modifiers(shift | 4, |e| events.push(e));
        decoder.modifiers(0, |e| events.push(e));
        assert_eq!(
            events,
            [
                InputEvent::key_down(Code::ShiftLeft),
                InputEvent::key_down(Code::ShiftRight),
                InputEvent::key_up(Code::ShiftLeft),
                InputEvent::key_up(Code::ShiftRight)
            ]
        );
    }
    #[test]
    fn recovery_synthesizes_only_missing_ups() {
        let mut decoder = Decoder::default();
        decoder.key(105, true);
        decoder.key(9, true);
        decoder.mouse(3, true);
        let mut events = Vec::new();
        decoder.reconcile(|code| code == 9, |_| false, |e| events.push(e));
        assert_eq!(
            events,
            [
                InputEvent::key_up(Code::F13),
                InputEvent::mouse_up(MouseButton::Mouse4)
            ]
        );
        assert_eq!(decoder.key(9, true), None);
        assert_eq!(decoder.key(9, false), Some(InputEvent::key_up(Code::KeyV)));
    }
}
