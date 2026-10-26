//! Physical keys, mouse buttons, and the chords a user can bind.
//!
//! Names follow the USB HID Keyboard/Keypad usage page (0x07). They are key
//! positions, not the glyph the current layout would type. `KeyV` is the key
//! in the US QWERTY V position on every layout.
//!
//! HID usage `0x2A` is Backspace (the spec calls it Delete). Forward delete
//! is `0x4C`. Power (`0x66`) is intentionally absent.

use std::{collections::HashSet, fmt};

use serde::{
    Deserialize, Serialize,
    de::{self, Deserializer, MapAccess, Visitor},
};

macro_rules! define_codes {
    ($(
        $(#[$meta:meta])*
        $name:ident = $usage:literal => $label:literal
    ),* $(,)?) => {
        /// A physical keyboard key.
        ///
        /// Discriminants are USB HID Keyboard/Keypad usage IDs.
        /// [`Code::hid_usage`] and [`Code::from_hid_usage`] convert both ways
        /// so an operating-system backend can map scan codes, virtual key
        /// codes, or XKB keycodes without a second table.
        ///
        /// Declaration order follows the HID usage table. `0x2A` is
        /// [`Code::Backspace`], not forward delete.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[non_exhaustive]
        #[repr(u16)]
        pub enum Code {
            $(
                $(#[$meta])*
                $name = $usage,
            )*
        }

        struct HidRow {
            usage: u16,
            code: Code,
            label: &'static str,
        }

        /// Sorted by usage. [`Code::from_hid_usage`] binary-searches this.
        const HID_ROWS: &[HidRow] = &[
            $(
                HidRow {
                    usage: $usage,
                    code: Code::$name,
                    label: $label,
                },
            )*
        ];

        #[cfg(test)]
        const ALL_CODES: &[Code] = &[$(Code::$name),*];
    };
}

define_codes! {
    /// US-layout A position.
    KeyA = 0x04 => "A",
    /// US-layout B position.
    KeyB = 0x05 => "B",
    /// US-layout C position.
    KeyC = 0x06 => "C",
    /// US-layout D position.
    KeyD = 0x07 => "D",
    /// US-layout E position.
    KeyE = 0x08 => "E",
    /// US-layout F position.
    KeyF = 0x09 => "F",
    /// US-layout G position.
    KeyG = 0x0A => "G",
    /// US-layout H position.
    KeyH = 0x0B => "H",
    /// US-layout I position.
    KeyI = 0x0C => "I",
    /// US-layout J position.
    KeyJ = 0x0D => "J",
    /// US-layout K position.
    KeyK = 0x0E => "K",
    /// US-layout L position.
    KeyL = 0x0F => "L",
    /// US-layout M position.
    KeyM = 0x10 => "M",
    /// US-layout N position.
    KeyN = 0x11 => "N",
    /// US-layout O position.
    KeyO = 0x12 => "O",
    /// US-layout P position.
    KeyP = 0x13 => "P",
    /// US-layout Q position.
    KeyQ = 0x14 => "Q",
    /// US-layout R position.
    KeyR = 0x15 => "R",
    /// US-layout S position.
    KeyS = 0x16 => "S",
    /// US-layout T position.
    KeyT = 0x17 => "T",
    /// US-layout U position.
    KeyU = 0x18 => "U",
    /// US-layout V position.
    KeyV = 0x19 => "V",
    /// US-layout W position.
    KeyW = 0x1A => "W",
    /// US-layout X position.
    KeyX = 0x1B => "X",
    /// US-layout Y position.
    KeyY = 0x1C => "Y",
    /// US-layout Z position.
    KeyZ = 0x1D => "Z",
    /// Digit row `1`.
    Digit1 = 0x1E => "1",
    /// Digit row `2`.
    Digit2 = 0x1F => "2",
    /// Digit row `3`.
    Digit3 = 0x20 => "3",
    /// Digit row `4`.
    Digit4 = 0x21 => "4",
    /// Digit row `5`.
    Digit5 = 0x22 => "5",
    /// Digit row `6`.
    Digit6 = 0x23 => "6",
    /// Digit row `7`.
    Digit7 = 0x24 => "7",
    /// Digit row `8`.
    Digit8 = 0x25 => "8",
    /// Digit row `9`.
    Digit9 = 0x26 => "9",
    /// Digit row `0`.
    Digit0 = 0x27 => "0",
    /// Enter. HID names this usage Return.
    Enter = 0x28 => "Enter",
    /// Escape.
    Escape = 0x29 => "Escape",
    /// Backspace. HID usage `0x2A` is this key, not forward delete.
    Backspace = 0x2A => "Backspace",
    /// Tab.
    Tab = 0x2B => "Tab",
    /// Space bar.
    Space = 0x2C => "Space",
    /// `-` / `_` on a US layout.
    Minus = 0x2D => "Minus",
    /// `=` / `+` on a US layout.
    Equal = 0x2E => "Equals",
    /// `[` / `{` on a US layout.
    BracketLeft = 0x2F => "Left Bracket",
    /// `]` / `}` on a US layout.
    BracketRight = 0x30 => "Right Bracket",
    /// `\` / `|` on a US layout.
    Backslash = 0x31 => "Backslash",
    /// Non-US `#` / `~` (HID usage `0x32`).
    IntlHash = 0x32 => "Intl Hash",
    /// `;` / `:` on a US layout.
    Semicolon = 0x33 => "Semicolon",
    /// `'` / `"` on a US layout.
    Quote = 0x34 => "Quote",
    /// Grave accent / tilde on a US layout.
    Backquote = 0x35 => "Backquote",
    /// `,` / `<` on a US layout.
    Comma = 0x36 => "Comma",
    /// `.` / `>` on a US layout.
    Period = 0x37 => "Period",
    /// `/` / `?` on a US layout.
    Slash = 0x38 => "Slash",
    /// Caps Lock.
    CapsLock = 0x39 => "Caps Lock",
    /// Function key F1.
    F1 = 0x3A => "F1",
    /// Function key F2.
    F2 = 0x3B => "F2",
    /// Function key F3.
    F3 = 0x3C => "F3",
    /// Function key F4.
    F4 = 0x3D => "F4",
    /// Function key F5.
    F5 = 0x3E => "F5",
    /// Function key F6.
    F6 = 0x3F => "F6",
    /// Function key F7.
    F7 = 0x40 => "F7",
    /// Function key F8.
    F8 = 0x41 => "F8",
    /// Function key F9.
    F9 = 0x42 => "F9",
    /// Function key F10.
    F10 = 0x43 => "F10",
    /// Function key F11.
    F11 = 0x44 => "F11",
    /// Function key F12.
    F12 = 0x45 => "F12",
    /// Print Screen.
    PrintScreen = 0x46 => "Print Screen",
    /// Scroll Lock.
    ScrollLock = 0x47 => "Scroll Lock",
    /// Pause / Break.
    Pause = 0x48 => "Pause",
    /// Insert.
    Insert = 0x49 => "Insert",
    /// Home.
    Home = 0x4A => "Home",
    /// Page Up.
    PageUp = 0x4B => "Page Up",
    /// Forward delete. HID usage `0x4C`.
    Delete = 0x4C => "Delete",
    /// End.
    End = 0x4D => "End",
    /// Page Down.
    PageDown = 0x4E => "Page Down",
    /// Right arrow.
    ArrowRight = 0x4F => "Right Arrow",
    /// Left arrow.
    ArrowLeft = 0x50 => "Left Arrow",
    /// Down arrow.
    ArrowDown = 0x51 => "Down Arrow",
    /// Up arrow.
    ArrowUp = 0x52 => "Up Arrow",
    /// Num Lock.
    NumLock = 0x53 => "Num Lock",
    /// Numpad divide.
    NumpadDivide = 0x54 => "Numpad Divide",
    /// Numpad multiply.
    NumpadMultiply = 0x55 => "Numpad Multiply",
    /// Numpad subtract.
    NumpadSubtract = 0x56 => "Numpad Minus",
    /// Numpad add.
    NumpadAdd = 0x57 => "Numpad Plus",
    /// Numpad Enter.
    NumpadEnter = 0x58 => "Numpad Enter",
    /// Numpad 1.
    Numpad1 = 0x59 => "Numpad 1",
    /// Numpad 2.
    Numpad2 = 0x5A => "Numpad 2",
    /// Numpad 3.
    Numpad3 = 0x5B => "Numpad 3",
    /// Numpad 4.
    Numpad4 = 0x5C => "Numpad 4",
    /// Numpad 5.
    Numpad5 = 0x5D => "Numpad 5",
    /// Numpad 6.
    Numpad6 = 0x5E => "Numpad 6",
    /// Numpad 7.
    Numpad7 = 0x5F => "Numpad 7",
    /// Numpad 8.
    Numpad8 = 0x60 => "Numpad 8",
    /// Numpad 9.
    Numpad9 = 0x61 => "Numpad 9",
    /// Numpad 0.
    Numpad0 = 0x62 => "Numpad 0",
    /// Numpad decimal.
    NumpadDecimal = 0x63 => "Numpad Decimal",
    /// Non-US `\` / `|` (HID usage `0x64`).
    IntlBackslash = 0x64 => "Intl Backslash",
    /// Application / context menu key.
    ContextMenu = 0x65 => "Menu",
    /// Numpad equals.
    NumpadEqual = 0x67 => "Numpad Equals",
    /// Function key F13. A common push-to-talk bind.
    F13 = 0x68 => "F13",
    /// Function key F14.
    F14 = 0x69 => "F14",
    /// Function key F15.
    F15 = 0x6A => "F15",
    /// Function key F16.
    F16 = 0x6B => "F16",
    /// Function key F17.
    F17 = 0x6C => "F17",
    /// Function key F18.
    F18 = 0x6D => "F18",
    /// Function key F19.
    F19 = 0x6E => "F19",
    /// Function key F20.
    F20 = 0x6F => "F20",
    /// Function key F21.
    F21 = 0x70 => "F21",
    /// Function key F22.
    F22 = 0x71 => "F22",
    /// Function key F23.
    F23 = 0x72 => "F23",
    /// Function key F24.
    F24 = 0x73 => "F24",
    /// Left Control.
    ControlLeft = 0xE0 => "Left Ctrl",
    /// Left Shift.
    ShiftLeft = 0xE1 => "Left Shift",
    /// Left Alt. Labeled Option on macOS.
    AltLeft = 0xE2 => "Left Alt",
    /// Left GUI. Win, Command, or Super depending on the OS.
    MetaLeft = 0xE3 => "Left Super",
    /// Right Control.
    ControlRight = 0xE4 => "Right Ctrl",
    /// Right Shift.
    ShiftRight = 0xE5 => "Right Shift",
    /// Right Alt. Labeled Option on macOS.
    AltRight = 0xE6 => "Right Alt",
    /// Right GUI. Win, Command, or Super depending on the OS.
    MetaRight = 0xE7 => "Right Super",
}

impl Code {
    /// USB HID usage ID for this key.
    #[must_use]
    pub const fn hid_usage(self) -> u16 {
        self as u16
    }

    /// Key for a USB HID usage ID, or `None` when this crate does not bind it.
    ///
    /// Power (`0x66`) is one of the gaps.
    #[must_use]
    pub fn from_hid_usage(usage: u16) -> Option<Self> {
        let index = HID_ROWS
            .binary_search_by_key(&usage, |row| row.usage)
            .ok()?;
        Some(HID_ROWS[index].code)
    }

    /// `true` for Control, Shift, Alt, and Meta on either side.
    #[must_use]
    pub const fn is_modifier(self) -> bool {
        matches!(
            self,
            Self::ControlLeft
                | Self::ControlRight
                | Self::ShiftLeft
                | Self::ShiftRight
                | Self::AltLeft
                | Self::AltRight
                | Self::MetaLeft
                | Self::MetaRight
        )
    }

    /// Short name of this key on `platform`.
    ///
    /// Letters, digits, and function keys are the same everywhere. Alt, Meta,
    /// and Control use the names of `platform`.
    #[must_use]
    pub fn label(self, platform: HostPlatform) -> &'static str {
        match (self, platform) {
            (Self::ControlLeft, HostPlatform::Macos) => "Left Control",
            (Self::ControlRight, HostPlatform::Macos) => "Right Control",
            (Self::AltLeft, HostPlatform::Macos) => "Left Option",
            (Self::AltRight, HostPlatform::Macos) => "Right Option",
            (Self::MetaLeft, HostPlatform::Windows) => "Left Win",
            (Self::MetaLeft, HostPlatform::Macos) => "Left Command",
            (Self::MetaRight, HostPlatform::Windows) => "Right Win",
            (Self::MetaRight, HostPlatform::Macos) => "Right Command",
            _ => self.fixed_label(),
        }
    }

    fn fixed_label(self) -> &'static str {
        for row in HID_ROWS {
            if row.code == self {
                return row.label;
            }
        }
        // `define_codes!` inserts every variant into `HID_ROWS`.
        "Unknown"
    }
}

impl fmt::Display for Code {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label(HostPlatform::current()))
    }
}

/// A mouse button that can be bound for push-to-talk.
///
/// Values are the USB HID Button page usages: 1 is left, 2 is right, 3 is
/// middle, 4 and 5 are the side buttons games call Mouse 4 and Mouse 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[repr(u16)]
pub enum MouseButton {
    /// Primary button.
    Left = 1,
    /// Secondary button.
    Right = 2,
    /// Wheel button.
    Middle = 3,
    /// First side button, often Back.
    Mouse4 = 4,
    /// Second side button, often Forward.
    Mouse5 = 5,
}

impl MouseButton {
    /// USB HID Button-page usage.
    #[must_use]
    pub const fn hid_button(self) -> u16 {
        self as u16
    }

    /// Button for a HID button usage, or `None` above Mouse 5.
    #[must_use]
    pub const fn from_hid_button(button: u16) -> Option<Self> {
        Some(match button {
            1 => Self::Left,
            2 => Self::Right,
            3 => Self::Middle,
            4 => Self::Mouse4,
            5 => Self::Mouse5,
            _ => return None,
        })
    }

    /// Settings label. Mouse names do not change per operating system.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left Mouse",
            Self::Right => "Right Mouse",
            Self::Middle => "Middle Mouse",
            Self::Mouse4 => "Mouse 4",
            Self::Mouse5 => "Mouse 5",
        }
    }
}

impl fmt::Display for MouseButton {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Operating system used to pick human names for modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HostPlatform {
    /// Windows. Meta is Win.
    Windows,
    /// macOS. Alt is Option and Meta is Command.
    Macos,
    /// Linux. Meta is Super.
    Linux,
    /// Any other target. Names match Linux.
    Other,
}

impl HostPlatform {
    /// Platform this binary was built for.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }

    /// Chord prefix for Control.
    #[must_use]
    pub const fn ctrl_name(self) -> &'static str {
        match self {
            Self::Macos => "Control",
            Self::Windows | Self::Linux | Self::Other => "Ctrl",
        }
    }

    /// Chord prefix for Shift.
    #[must_use]
    pub const fn shift_name(self) -> &'static str {
        match self {
            Self::Windows | Self::Macos | Self::Linux | Self::Other => "Shift",
        }
    }

    /// Chord prefix for Alt.
    #[must_use]
    pub const fn alt_name(self) -> &'static str {
        match self {
            Self::Macos => "Option",
            Self::Windows | Self::Linux | Self::Other => "Alt",
        }
    }

    /// Chord prefix for the GUI key.
    #[must_use]
    pub const fn meta_name(self) -> &'static str {
        match self {
            Self::Windows => "Win",
            Self::Macos => "Command",
            Self::Linux | Self::Other => "Super",
        }
    }
}

/// Modifiers that must be held along with a trigger.
///
/// Each flag accepts either physical side. Flags that are false do not
/// reject extra modifiers: a `V` binding still matches while Shift is down.
///
/// Serialized as a JSON object of the flags that are true. Missing flags
/// deserialize as false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per modifier keeps the settings JSON readable"
)]
pub struct ModifierSet {
    /// Either Control key.
    pub ctrl: bool,
    /// Either Shift key.
    pub shift: bool,
    /// Either Alt key.
    pub alt: bool,
    /// Either GUI key (Win, Command, or Super).
    pub meta: bool,
}

impl ModifierSet {
    /// No modifiers required.
    pub const EMPTY: Self = Self {
        ctrl: false,
        shift: false,
        alt: false,
        meta: false,
    };

    /// Control, and nothing else.
    pub const CTRL: Self = Self::EMPTY.with_ctrl();

    /// Shift, and nothing else.
    pub const SHIFT: Self = Self::EMPTY.with_shift();

    /// Alt, and nothing else.
    pub const ALT: Self = Self::EMPTY.with_alt();

    /// Meta, and nothing else.
    pub const META: Self = Self::EMPTY.with_meta();

    /// Require Control.
    #[must_use]
    pub const fn with_ctrl(self) -> Self {
        Self { ctrl: true, ..self }
    }

    /// Require Shift.
    #[must_use]
    pub const fn with_shift(self) -> Self {
        Self {
            shift: true,
            ..self
        }
    }

    /// Require Alt.
    #[must_use]
    pub const fn with_alt(self) -> Self {
        Self { alt: true, ..self }
    }

    /// Require Meta.
    #[must_use]
    pub const fn with_meta(self) -> Self {
        Self { meta: true, ..self }
    }

    /// `true` when every flag is false.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.ctrl && !self.shift && !self.alt && !self.meta
    }

    /// Flags for whichever modifier keys are currently in `keys`.
    #[must_use]
    pub fn from_key_set(keys: &HashSet<Code>) -> Self {
        Self {
            ctrl: keys.contains(&Code::ControlLeft) || keys.contains(&Code::ControlRight),
            shift: keys.contains(&Code::ShiftLeft) || keys.contains(&Code::ShiftRight),
            alt: keys.contains(&Code::AltLeft) || keys.contains(&Code::AltRight),
            meta: keys.contains(&Code::MetaLeft) || keys.contains(&Code::MetaRight),
        }
    }

    /// Whether every required modifier is down in `keys`.
    #[must_use]
    pub fn satisfied_by(self, keys: &HashSet<Code>) -> bool {
        (!self.ctrl || Self::either(keys, Code::ControlLeft, Code::ControlRight))
            && (!self.shift || Self::either(keys, Code::ShiftLeft, Code::ShiftRight))
            && (!self.alt || Self::either(keys, Code::AltLeft, Code::AltRight))
            && (!self.meta || Self::either(keys, Code::MetaLeft, Code::MetaRight))
    }

    fn either(keys: &HashSet<Code>, left: Code, right: Code) -> bool {
        keys.contains(&left) || keys.contains(&right)
    }
}

impl Serialize for ModifierSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        if self.ctrl {
            map.serialize_entry("ctrl", &true)?;
        }
        if self.shift {
            map.serialize_entry("shift", &true)?;
        }
        if self.alt {
            map.serialize_entry("alt", &true)?;
        }
        if self.meta {
            map.serialize_entry("meta", &true)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ModifierSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ModifierVisitor;

        impl<'de> Visitor<'de> for ModifierVisitor {
            type Value = ModifierSet;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a modifier object with ctrl, shift, alt, and meta flags")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut set = ModifierSet::EMPTY;
                while let Some(key) = map.next_key::<String>()? {
                    let value: bool = map.next_value()?;
                    match key.as_str() {
                        "ctrl" => set.ctrl = value,
                        "shift" => set.shift = value,
                        "alt" => set.alt = value,
                        "meta" => set.meta = value,
                        _ => {
                            return Err(de::Error::unknown_field(
                                &key,
                                &["ctrl", "shift", "alt", "meta"],
                            ));
                        }
                    }
                }
                Ok(set)
            }
        }

        deserializer.deserialize_map(ModifierVisitor)
    }
}

/// What the user presses to complete a [`Binding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Trigger {
    /// A keyboard key.
    Key(Code),
    /// A mouse button.
    Mouse(MouseButton),
}

impl Trigger {
    /// Label of the key or button on `platform`.
    #[must_use]
    pub fn label(self, platform: HostPlatform) -> &'static str {
        match self {
            Self::Key(code) => code.label(platform),
            Self::Mouse(button) => button.label(),
        }
    }
}

/// A push-to-talk chord: one trigger plus the modifiers that must be held.
///
/// JSON uses an externally tagged trigger and a modifier object of only the
/// flags that are true:
///
/// ```json
/// {"trigger":{"key":"KeyV"},"modifiers":{"ctrl":true}}
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Binding {
    /// Key or mouse button that completes the chord.
    pub trigger: Trigger,
    /// Modifiers that must also be down. Empty means the trigger alone.
    #[serde(default)]
    pub modifiers: ModifierSet,
}

impl Binding {
    /// Bind `code` with no modifiers.
    #[must_use]
    pub const fn key(code: Code) -> Self {
        Self {
            trigger: Trigger::Key(code),
            modifiers: ModifierSet::EMPTY,
        }
    }

    /// Bind `button` with no modifiers.
    #[must_use]
    pub const fn mouse(button: MouseButton) -> Self {
        Self {
            trigger: Trigger::Mouse(button),
            modifiers: ModifierSet::EMPTY,
        }
    }

    /// Require `modifiers` in addition to the trigger.
    #[must_use]
    pub const fn with_modifiers(mut self, modifiers: ModifierSet) -> Self {
        self.modifiers = modifiers;
        self
    }

    /// Human label on the platform this binary was built for.
    #[must_use]
    pub fn label(&self) -> String {
        self.label_for(HostPlatform::current())
    }

    /// Human label using `platform`'s names for Control, Alt, and Meta.
    ///
    /// Modifiers are ordered Control, Shift, Alt, Meta, then the trigger.
    /// A bare key is just its own name (`V`, `F13`, `Mouse 4`).
    #[must_use]
    pub fn label_for(&self, platform: HostPlatform) -> String {
        let mut parts = Vec::new();
        if self.modifiers.ctrl {
            parts.push(platform.ctrl_name());
        }
        if self.modifiers.shift {
            parts.push(platform.shift_name());
        }
        if self.modifiers.alt {
            parts.push(platform.alt_name());
        }
        if self.modifiers.meta {
            parts.push(platform.meta_name());
        }
        parts.push(self.trigger.label(platform));
        parts.join(" + ")
    }

    /// Whether `keys` and `buttons` currently satisfy this chord.
    ///
    /// A required modifier accepts either physical side. Extra modifiers still
    /// count as held.
    #[must_use]
    pub fn is_held(&self, keys: &HashSet<Code>, buttons: &HashSet<MouseButton>) -> bool {
        let trigger_down = match self.trigger {
            Trigger::Key(code) => keys.contains(&code),
            Trigger::Mouse(button) => buttons.contains(&button),
        };
        trigger_down && self.modifiers.satisfied_by(keys)
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label())
    }
}

/// One edge from a backend.
///
/// `pressed` is true on down and false on up. The state machine and
/// [`CaptureSession`] both consume this. Backends do not interpret chords.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InputEvent {
    /// Keyboard edge.
    Key {
        /// Physical key.
        code: Code,
        /// `true` on down, `false` on up.
        pressed: bool,
    },
    /// Mouse-button edge.
    Mouse {
        /// Button that changed.
        button: MouseButton,
        /// `true` on down, `false` on up.
        pressed: bool,
    },
}

impl InputEvent {
    /// Keyboard edge.
    #[must_use]
    pub const fn key(code: Code, pressed: bool) -> Self {
        Self::Key { code, pressed }
    }

    /// Key down.
    #[must_use]
    pub const fn key_down(code: Code) -> Self {
        Self::key(code, true)
    }

    /// Key up.
    #[must_use]
    pub const fn key_up(code: Code) -> Self {
        Self::key(code, false)
    }

    /// Mouse-button edge.
    #[must_use]
    pub const fn mouse(button: MouseButton, pressed: bool) -> Self {
        Self::Mouse { button, pressed }
    }

    /// Button down.
    #[must_use]
    pub const fn mouse_down(button: MouseButton) -> Self {
        Self::mouse(button, true)
    }

    /// Button up.
    #[must_use]
    pub const fn mouse_up(button: MouseButton) -> Self {
        Self::mouse(button, false)
    }
}

/// Gesture recorder for the settings "press a binding" control.
///
/// Rules, in order:
///
/// 1. Hold modifiers, then press a non-modifier key. The binding completes
///    on that key's down. Modifiers are whatever modifier keys are down.
/// 2. Press a mouse button. Same as a key, when mouse buttons are allowed.
///    A disallowed button is ignored and capture keeps waiting.
/// 3. Press only modifiers. The binding completes when the first of those
///    modifiers is released. The released key is the trigger. Modifiers still
///    held become the modifier set.
///
/// This observes edges. It does not grab them. When the compositor owns
/// shortcuts, callers must not construct a session; [`crate::Listener::capture_next`]
/// returns [`crate::CapturedBinding::SetBySystem`] instead.
#[derive(Debug, Clone)]
pub struct CaptureSession {
    allow_mouse: bool,
    held: HashSet<Code>,
    gesture: Vec<Code>,
    done: bool,
}

impl CaptureSession {
    /// Session that accepts mouse buttons only when `allow_mouse` is set.
    #[must_use]
    pub fn new(allow_mouse: bool) -> Self {
        Self {
            allow_mouse,
            held: HashSet::new(),
            gesture: Vec::new(),
            done: false,
        }
    }

    /// Feed one edge. Returns the binding once, when the gesture completes.
    #[must_use]
    pub fn push(&mut self, event: InputEvent) -> Option<Binding> {
        if self.done {
            return None;
        }
        match event {
            InputEvent::Key {
                code,
                pressed: true,
            } if code.is_modifier() => {
                self.held.insert(code);
                if !self.gesture.contains(&code) {
                    self.gesture.push(code);
                }
                None
            }
            InputEvent::Key {
                code,
                pressed: true,
            } => {
                let binding =
                    Binding::key(code).with_modifiers(ModifierSet::from_key_set(&self.held));
                self.done = true;
                Some(binding)
            }
            InputEvent::Key {
                code,
                pressed: false,
            } if code.is_modifier() => {
                self.held.remove(&code);
                if !self.gesture.contains(&code) {
                    return None;
                }
                let binding =
                    Binding::key(code).with_modifiers(ModifierSet::from_key_set(&self.held));
                self.done = true;
                Some(binding)
            }
            InputEvent::Mouse {
                button,
                pressed: true,
            } => {
                if !self.allow_mouse {
                    return None;
                }
                let binding =
                    Binding::mouse(button).with_modifiers(ModifierSet::from_key_set(&self.held));
                self.done = true;
                Some(binding)
            }
            InputEvent::Key { .. } | InputEvent::Mouse { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::{
        ALL_CODES, Binding, CaptureSession, Code, HostPlatform, InputEvent, ModifierSet,
        MouseButton,
    };

    #[test]
    fn hid_usages_round_trip_and_stay_sorted() {
        let mut previous = 0u16;
        for code in ALL_CODES {
            let usage = code.hid_usage();
            assert!(
                usage > previous,
                "{code:?} usage {usage:#x} is out of order"
            );
            previous = usage;
            assert_eq!(Code::from_hid_usage(usage), Some(*code));
            assert_ne!(code.label(HostPlatform::Linux), "Unknown");
        }
        assert_eq!(Code::KeyA.hid_usage(), 0x04);
        assert_eq!(Code::KeyV.hid_usage(), 0x19);
        assert_eq!(Code::Digit1.hid_usage(), 0x1E);
        assert_eq!(Code::Digit0.hid_usage(), 0x27);
        assert_eq!(Code::Backspace.hid_usage(), 0x2A);
        assert_eq!(Code::Delete.hid_usage(), 0x4C);
        assert_eq!(Code::F1.hid_usage(), 0x3A);
        assert_eq!(Code::F12.hid_usage(), 0x45);
        assert_eq!(Code::F13.hid_usage(), 0x68);
        assert_eq!(Code::F24.hid_usage(), 0x73);
        assert_eq!(Code::ControlLeft.hid_usage(), 0xE0);
        assert_eq!(Code::MetaRight.hid_usage(), 0xE7);
        assert_eq!(Code::from_hid_usage(0x66), None);
        assert_eq!(MouseButton::Mouse4.hid_button(), 4);
        assert_eq!(MouseButton::from_hid_button(5), Some(MouseButton::Mouse5));
        assert_eq!(MouseButton::from_hid_button(6), None);
    }

    #[test]
    fn labels_follow_the_platform() {
        let chord = Binding::key(Code::KeyV).with_modifiers(ModifierSet::CTRL.with_alt());
        assert_eq!(chord.label_for(HostPlatform::Macos), "Control + Option + V");
        assert_eq!(
            Binding::key(Code::KeyV)
                .with_modifiers(ModifierSet::CTRL.with_meta())
                .label_for(HostPlatform::Windows),
            "Ctrl + Win + V",
        );
        assert_eq!(
            Binding::key(Code::KeyV)
                .with_modifiers(ModifierSet::SHIFT.with_meta())
                .label_for(HostPlatform::Linux),
            "Shift + Super + V",
        );
        assert_eq!(
            Binding::key(Code::F13).label_for(HostPlatform::Linux),
            "F13"
        );
        assert_eq!(
            Binding::mouse(MouseButton::Mouse4).label_for(HostPlatform::Windows),
            "Mouse 4"
        );
        assert_eq!(Binding::mouse(MouseButton::Middle).label(), "Middle Mouse");
        assert_eq!(Code::MetaLeft.label(HostPlatform::Macos), "Left Command");
        assert_eq!(Code::MetaLeft.label(HostPlatform::Windows), "Left Win");
        assert_eq!(Code::MetaLeft.label(HostPlatform::Linux), "Left Super");
        assert_eq!(Code::AltLeft.label(HostPlatform::Macos), "Left Option");
        assert_eq!(
            HostPlatform::current(),
            if cfg!(target_os = "macos") {
                HostPlatform::Macos
            } else if cfg!(target_os = "windows") {
                HostPlatform::Windows
            } else if cfg!(target_os = "linux") {
                HostPlatform::Linux
            } else {
                HostPlatform::Other
            }
        );
    }

    #[test]
    fn serde_round_trip_uses_hid_names() {
        let binding = Binding::key(Code::F13).with_modifiers(ModifierSet::CTRL.with_shift());
        let value = serde_json::to_value(binding).unwrap();
        assert_eq!(
            value,
            json!({
                "trigger": { "key": "F13" },
                "modifiers": { "ctrl": true, "shift": true }
            })
        );
        let restored: Binding = serde_json::from_value(value).unwrap();
        assert_eq!(restored, binding);

        let mouse = serde_json::to_value(Binding::mouse(MouseButton::Mouse5)).unwrap();
        assert_eq!(
            mouse,
            json!({
                "trigger": { "mouse": "Mouse5" },
                "modifiers": {}
            })
        );
        let bare: Binding = serde_json::from_value(json!({"trigger": {"key": "KeyV"}})).unwrap();
        assert_eq!(bare, Binding::key(Code::KeyV));
        assert!(serde_json::from_value::<Binding>(json!({"trigger": {"key": "NotAKey"}})).is_err());
    }

    #[test]
    fn ctrl_accepts_either_side_and_ignores_extra_modifiers() {
        let binding = Binding::key(Code::KeyV).with_modifiers(ModifierSet::CTRL);
        let buttons = HashSet::new();
        let mut keys = HashSet::from([Code::ControlRight, Code::KeyV, Code::ShiftLeft]);
        assert!(binding.is_held(&keys, &buttons));
        keys.remove(&Code::ControlRight);
        assert!(!binding.is_held(&keys, &buttons));

        let left_only = Binding::key(Code::ControlLeft);
        let right = HashSet::from([Code::ControlRight]);
        let left = HashSet::from([Code::ControlLeft]);
        assert!(!left_only.is_held(&right, &buttons));
        assert!(left_only.is_held(&left, &buttons));
    }

    #[test]
    fn capture_records_the_trigger_and_held_modifiers() {
        let mut session = CaptureSession::new(true);
        assert!(
            session
                .push(InputEvent::key_down(Code::ShiftLeft))
                .is_none()
        );
        assert!(
            session
                .push(InputEvent::key_down(Code::ControlRight))
                .is_none()
        );
        assert_eq!(
            session.push(InputEvent::key_down(Code::KeyV)),
            Some(Binding::key(Code::KeyV).with_modifiers(ModifierSet::CTRL.with_shift())),
        );
        assert!(session.push(InputEvent::key_down(Code::KeyB)).is_none());
    }

    #[test]
    fn capture_binds_a_modifier_when_it_is_released_alone() {
        let mut session = CaptureSession::new(true);
        assert!(session.push(InputEvent::key_down(Code::AltRight)).is_none());
        assert_eq!(
            session.push(InputEvent::key_up(Code::AltRight)),
            Some(Binding::key(Code::AltRight)),
        );
    }

    #[test]
    fn capture_accepts_mouse_and_skips_it_when_unsupported() {
        let mut session = CaptureSession::new(true);
        assert!(
            session
                .push(InputEvent::key_down(Code::ControlLeft))
                .is_none()
        );
        assert_eq!(
            session.push(InputEvent::mouse_down(MouseButton::Mouse5)),
            Some(Binding::mouse(MouseButton::Mouse5).with_modifiers(ModifierSet::CTRL)),
        );

        let mut keys_only = CaptureSession::new(false);
        assert!(
            keys_only
                .push(InputEvent::mouse_down(MouseButton::Middle))
                .is_none()
        );
        assert_eq!(
            keys_only.push(InputEvent::key_down(Code::F13)),
            Some(Binding::key(Code::F13)),
        );
    }
}
