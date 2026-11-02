//! Translating between GPUI input and CEF's: modifiers, key codes, cursors.

use cef::CursorType;
use gpui::{CursorStyle, Keystroke, Modifiers};

// cef_event_flags_t
const SHIFT: u32 = 1 << 1;
const CONTROL: u32 = 1 << 2;
const ALT: u32 = 1 << 3;
pub(crate) const LEFT_BUTTON: u32 = 1 << 4;
pub(crate) const MIDDLE_BUTTON: u32 = 1 << 5;
pub(crate) const RIGHT_BUTTON: u32 = 1 << 6;
const COMMAND: u32 = 1 << 7;
pub(crate) const IS_REPEAT: u32 = 1 << 13;

pub(crate) fn modifiers(m: &Modifiers) -> u32 {
    let mut flags = 0;
    if m.shift {
        flags |= SHIFT;
    }
    if m.control {
        flags |= CONTROL;
    }
    if m.alt {
        flags |= ALT;
    }
    if m.platform {
        flags |= COMMAND;
    }
    flags
}

/// The Windows virtual-key code Chromium keys its events on.
///
/// GPUI names keys by the symbol they produce, so a shifted symbol ("!")
/// maps back through the US layout to its key ("1").
pub(crate) fn windows_key_code(k: &Keystroke) -> i32 {
    let key = k.key.as_str();
    match key {
        "backspace" => 0x08,
        "tab" => 0x09,
        "enter" => 0x0D,
        "shift" => 0x10,
        "control" => 0x11,
        "alt" => 0x12,
        "pause" => 0x13,
        "capslock" => 0x14,
        "escape" => 0x1B,
        "space" => 0x20,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "platform" | "super" => 0x5B,
        "menu" => 0x5D,
        "back" => 0xA6,
        "forward" => 0xA7,
        ";" | ":" => 0xBA,
        "=" | "+" => 0xBB,
        "," | "<" => 0xBC,
        "-" | "_" => 0xBD,
        "." | ">" => 0xBE,
        "/" | "?" => 0xBF,
        "`" | "~" => 0xC0,
        "[" | "{" => 0xDB,
        "\\" | "|" => 0xDC,
        "]" | "}" => 0xDD,
        "'" | "\"" => 0xDE,
        ")" => 0x30,
        "!" => 0x31,
        "@" => 0x32,
        "#" => 0x33,
        "$" => 0x34,
        "%" => 0x35,
        "^" => 0x36,
        "&" => 0x37,
        "*" => 0x38,
        "(" => 0x39,
        _ => {
            if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<i32>().ok())
                && (1..=24).contains(&n)
            {
                0x70 + n - 1
            } else {
                let mut chars = key.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_alphabetic() => c.to_ascii_uppercase() as i32,
                    (Some(c), None) if c.is_ascii_digit() => c as i32,
                    _ => 0,
                }
            }
        }
    }
}

/// The text a key press types, for the CHAR event, if any.
pub(crate) fn typed_char(k: &Keystroke) -> Option<u16> {
    // Control/command chords are shortcuts, not text.
    if k.modifiers.control || k.modifiers.platform {
        return None;
    }
    match k.key.as_str() {
        "enter" => return Some(0x0D),
        "tab" => return Some(0x09),
        "backspace" => return Some(0x08),
        _ => {}
    }
    let text = k.key_char.as_deref()?;
    let mut units = text.encode_utf16();
    let first = units.next()?;
    units.next().is_none().then_some(first)
}

pub(crate) fn cursor(t: CursorType) -> CursorStyle {
    match t {
        CursorType::HAND => CursorStyle::PointingHand,
        CursorType::IBEAM => CursorStyle::IBeam,
        CursorType::VERTICALTEXT => CursorStyle::IBeamCursorForVerticalLayout,
        CursorType::CROSS | CursorType::CELL => CursorStyle::Crosshair,
        CursorType::EASTRESIZE => CursorStyle::ResizeRight,
        CursorType::WESTRESIZE => CursorStyle::ResizeLeft,
        CursorType::NORTHRESIZE => CursorStyle::ResizeUp,
        CursorType::SOUTHRESIZE => CursorStyle::ResizeDown,
        CursorType::EASTWESTRESIZE => CursorStyle::ResizeLeftRight,
        CursorType::NORTHSOUTHRESIZE => CursorStyle::ResizeUpDown,
        CursorType::NORTHEASTSOUTHWESTRESIZE | CursorType::NORTHEASTRESIZE | CursorType::SOUTHWESTRESIZE => {
            CursorStyle::ResizeUpRightDownLeft
        }
        CursorType::NORTHWESTSOUTHEASTRESIZE | CursorType::NORTHWESTRESIZE | CursorType::SOUTHEASTRESIZE => {
            CursorStyle::ResizeUpLeftDownRight
        }
        CursorType::COLUMNRESIZE => CursorStyle::ResizeColumn,
        CursorType::ROWRESIZE => CursorStyle::ResizeRow,
        CursorType::GRAB | CursorType::MOVE => CursorStyle::OpenHand,
        CursorType::GRABBING => CursorStyle::ClosedHand,
        CursorType::NOTALLOWED | CursorType::NODROP | CursorType::DND_NONE => CursorStyle::OperationNotAllowed,
        CursorType::ALIAS | CursorType::DND_LINK => CursorStyle::DragLink,
        CursorType::COPY | CursorType::DND_COPY => CursorStyle::DragCopy,
        _ => CursorStyle::Arrow,
    }
}
