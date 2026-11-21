//! Configurable policy for opted-in X11 paste when editable-field identity is unavailable.

use crate::settings::{InsertMethod, InsertSettings};

/// Consent must exist at both ends of a recording; interrupted audio always needs review.
pub fn paste_on_completion(settings: &InsertSettings, opted_in_at_start: bool, interrupted: bool) -> bool {
    opted_in_at_start && settings.paste_on_completion && settings.method == InsertMethod::Paste && !interrupted
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasteShortcut {
    /// Canonical X11 real modifiers: Shift=1, Ctrl=4, Alt=8, Super=64.
    pub modifiers: u8,
    /// Logical, unshifted keysym, not a physical key position.
    pub keysym: u32,
}

impl PasteShortcut {
    pub fn parse(shortcut: &str) -> Result<Self, String> {
        if shortcut.chars().any(char::is_control) {
            return Err("Paste shortcut must be a single key combination, without control characters.".into());
        }
        let mut modifiers = 0;
        let mut keysym = None;
        for part in shortcut.split('+') {
            let part = part.trim().to_ascii_lowercase();
            if part.is_empty() || keysym.is_some() {
                return Err("Paste shortcut must contain modifiers followed by exactly one key.".into());
            }
            let modifier = match part.as_str() {
                "ctrl" | "control" => 4,
                "alt" => 8,
                "shift" => 1,
                "super" | "meta" => 64,
                _ => 0,
            };
            if modifier != 0 {
                if modifiers & modifier != 0 {
                    return Err(format!("Duplicate paste shortcut modifier: {part}."));
                }
                modifiers |= modifier;
                continue;
            }
            let letter = part.strip_prefix("key").unwrap_or(&part);
            let digit = part.strip_prefix("digit").unwrap_or(&part);
            keysym = Some(if letter.len() == 1 && letter.as_bytes()[0].is_ascii_lowercase() {
                u32::from(letter.as_bytes()[0])
            } else if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() {
                u32::from(digit.as_bytes()[0])
            } else if let Some(number) = part.strip_prefix('f').filter(|n| !n.starts_with('0')).and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
                0xffbd + number
            } else {
                match part.as_str() {
                    "insert" => 0xff63,
                    "home" => 0xff50,
                    "left" | "arrowleft" => 0xff51,
                    "up" | "arrowup" => 0xff52,
                    "right" | "arrowright" => 0xff53,
                    "down" | "arrowdown" => 0xff54,
                    "pageup" => 0xff55,
                    "pagedown" => 0xff56,
                    "end" => 0xff57,
                    "space" => 0x20,
                    "tab" => 0xff09,
                    "backspace" => 0xff08,
                    "delete" => 0xffff,
                    "escape" | "esc" => 0xff1b,
                    "minus" => 0x2d,
                    "equal" => 0x3d,
                    "bracketleft" => 0x5b,
                    "bracketright" => 0x5d,
                    "backslash" => 0x5c,
                    "semicolon" => 0x3b,
                    "quote" => 0x27,
                    "backquote" => 0x60,
                    "comma" => 0x2c,
                    "period" => 0x2e,
                    "slash" => 0x2f,
                    "enter" | "return" | "numpadenter" | "kp_enter" => return Err("Enter is not allowed in a paste shortcut.".into()),
                    _ => return Err(format!("Unsupported paste shortcut key: {part}.")),
                }
            });
        }
        let keysym = keysym.ok_or("Paste shortcut needs a primary key, not just modifiers.")?;
        if modifiers == 0 && !(0xffbe..=0xffd5).contains(&keysym) {
            return Err("Paste shortcut needs at least one modifier, except for F1 through F24.".into());
        }
        Ok(Self { modifiers, keysym })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct X11Key {
    pub group_info: u8,
    pub width: u8,
    pub types: [u8; 4],
    pub symbols: Vec<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct X11KeyType {
    /// Effective real modifiers, with XKB's virtual modifiers already resolved by the server.
    pub modifiers: u8,
    pub num_levels: u8,
    /// Active XKB entries: (modifier mask, level, preserved modifier mask).
    pub levels: Vec<(u8, u8, u8)>,
}

/// Resolve the active layout without changing its map or group. Press in returned order
/// (Ctrl, Alt, Shift, Super, primary, omitting unused modifiers), then release in reverse.
/// The caller must establish that no modifiers are held, latched or locked.
pub fn paste_keycodes(min: u8, keys: &[X11Key], types: &[X11KeyType], group: u8, modifier_map: &[u8], shortcut: &PasteShortcut) -> Option<Vec<u8>> {
    if group > 3 || modifier_map.is_empty() || !modifier_map.len().is_multiple_of(8)
        || shortcut.modifiers & !0x4d != 0 || matches!(shortcut.keysym, 0 | 0xff0d | 0xff8d)
        || (shortcut.modifiers == 0 && !(0xffbe..=0xffd5).contains(&shortcut.keysym)) {
        return None;
    }
    let symbol = |key: &X11Key, mods: u8| -> Option<(u32, u8)> {
        let groups = key.group_info & 0x0f;
        if !(1..=4).contains(&groups) || key.width == 0 || key.symbols.len() != usize::from(groups) * usize::from(key.width) {
            return None;
        }
        let group = if group < groups { group } else {
            match key.group_info & 0xc0 {
                0x00 => group % groups,
                0x40 => groups - 1,
                0x80 => {
                    let redirect = (key.group_info >> 4) & 3;
                    if redirect < groups { redirect } else { 0 }
                }
                _ => return None,
            }
        };
        let kind = types.get(usize::from(key.types[usize::from(group)]))?;
        if kind.num_levels == 0 || kind.num_levels > key.width || kind.levels.iter().any(|&(mask, level, preserve)| {
            mask & !kind.modifiers != 0 || level >= kind.num_levels || preserve & !mask != 0
        }) { return None; }
        let (_, level, preserve) = kind.levels.iter().find(|(mask, _, _)| mods & kind.modifiers == *mask).copied().unwrap_or((0, 0, 0));
        if level >= key.width || level >= kind.num_levels { return None; }
        Some((key.symbols[usize::from(group) * usize::from(key.width) + usize::from(level)], kind.modifiers & !preserve))
    };
    let per_modifier = modifier_map.len() / 8;
    let keycode = |index| min.checked_add(u8::try_from(index).ok()?).filter(|&key| key >= 8);
    let mut codes = Vec::new();
    let mut pressed = 0;
    for (mask, slot, left, right) in [(4, 2, 0xffe3, 0xffe4), (8, 3, 0xffe9, 0xffea), (1, 0, 0xffe1, 0xffe2), (64, 6, 0xffeb, 0xffec)] {
        if shortcut.modifiers & mask == 0 { continue; }
        let range = slot * per_modifier..(slot + 1) * per_modifier;
        let code = keys.iter().enumerate().find_map(|(index, key)| {
            let code = keycode(index)?;
            let base = symbol(key, 0)?.0;
            let active = symbol(key, pressed)?.0;
            // Reverse release removes later modifiers before this key.
            ((base == left || base == right) && (active == left || active == right)
                && !matches!(symbol(key, pressed | mask)?.0, 0 | 0xff0d | 0xff8d)
                && modifier_map[range.clone()].contains(&code)
                && !modifier_map.iter().enumerate().any(|(i, &key)| key == code && !range.contains(&i))
                && !codes.contains(&code)).then_some(code)
        })?;
        codes.push(code);
        pressed |= mask;
    }
    let primary = keys.iter().enumerate().find_map(|(index, key)| {
        let code = keycode(index)?;
        let (active, consumed) = symbol(key, shortcut.modifiers)?;
        (symbol(key, 0)?.0 == shortcut.keysym && active == symbol(key, shortcut.modifiers & 1)?.0
            && !matches!(active, 0 | 0xff0d | 0xff8d) && consumed & shortcut.modifiers & !1 == 0
            && !modifier_map.contains(&code) && !codes.contains(&code)).then_some(code)
    })?;
    codes.push(primary);
    Some(codes)
}

pub fn allows_explicit_x11_paste(class: &str, text: &str, any_window: bool) -> bool {
    const EDITORS: &[&str] = &[
        "gedit", "org.gnome.gedit", "gnome-text-editor", "org.gnome.texteditor",
        "kate", "org.kde.kate", "kwrite", "org.kde.kwrite", "mousepad", "xed", "pluma", "leafpad",
    ];
    !text.trim().is_empty()
        && !text.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        && (any_window || EDITORS.iter().any(|editor| class.eq_ignore_ascii_case(editor)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_shortcut_parses_logical_keys_and_modifier_aliases() {
        assert_eq!(PasteShortcut::parse(&InsertSettings::default().paste_shortcut).unwrap(), PasteShortcut { modifiers: 4, keysym: 0x76 });
        for (input, modifiers, keysym) in [
            ("Ctrl+KeyV", 4, 0x76), ("Control+v", 4, 0x76), ("ctrl + V", 4, 0x76),
            ("Ctrl+Shift+KeyV", 5, 0x76), ("Shift+Insert", 1, 0xff63),
            ("Alt+KeyQ", 8, 0x71), ("Meta+F12", 64, 0xffc9),
            ("Super+Shift+Alt+Control+KeyZ", 77, 0x7a), ("Shift+A", 1, 0x61),
            ("Ctrl+Digit1", 4, 0x31), ("Alt+0", 8, 0x30),
            ("Ctrl+Home", 4, 0xff50), ("Ctrl+ArrowLeft", 4, 0xff51),
            ("Alt+Up", 8, 0xff52), ("Alt+ArrowRight", 8, 0xff53), ("Alt+Down", 8, 0xff54),
            ("Ctrl+PageUp", 4, 0xff55), ("Ctrl+PageDown", 4, 0xff56), ("Ctrl+End", 4, 0xff57),
            ("Ctrl+Space", 4, 0x20), ("Ctrl+Tab", 4, 0xff09),
            ("Ctrl+Backspace", 4, 0xff08), ("Ctrl+Delete", 4, 0xffff), ("Ctrl+Escape", 4, 0xff1b),
            ("Ctrl+Minus", 4, 0x2d), ("Ctrl+Equal", 4, 0x3d),
            ("Ctrl+BracketLeft", 4, 0x5b), ("Ctrl+BracketRight", 4, 0x5d),
            ("Ctrl+Backslash", 4, 0x5c), ("Ctrl+Semicolon", 4, 0x3b), ("Ctrl+Quote", 4, 0x27),
            ("Ctrl+Backquote", 4, 0x60), ("Ctrl+Comma", 4, 0x2c),
            ("Ctrl+Period", 4, 0x2e), ("Ctrl+Slash", 4, 0x2f),
        ] {
            assert_eq!(PasteShortcut::parse(input).unwrap(), PasteShortcut { modifiers, keysym }, "{input}");
        }
        for n in 1..=24 {
            assert_eq!(PasteShortcut::parse(&format!("F{n}")).unwrap(), PasteShortcut { modifiers: 0, keysym: 0xffbd + n });
        }
    }

    #[test]
    fn paste_shortcut_rejects_enter_bare_typing_and_malformed_chords() {
        for input in [
            "", " ", "V", "KeyV", "1", "Digit1", "Space", "Tab", "Insert", "Comma", "hello",
            "Ctrl", "Ctrl+Shift", "Ctrl+", "+V", "Ctrl++V", "Ctrl+V+", "V+Ctrl", "Ctrl+V+X",
            "Ctrl+Ctrl+V", "Ctrl+Control+V", "Alt+Alt+V", "Shift+Shift+V", "Super+Meta+V",
            "Ctrl+V Ctrl+C", "Ctrl+V,Ctrl+C", "Ctrl+V;Ctrl+C", "Ctrl+V\n", "Ctrl+\tV",
            "Ctrl+Key1", "Ctrl+DigitV", "Ctrl+KeyVV", "Cmd+V", "AltGr+V", "Ctrl+Unidentified",
            "F0", "F25", "Ctrl+F0", "Ctrl+F25", "F01", "Ctrl+F01", "Ctrl+Numpad1",
            "Enter", "Return", "NumpadEnter", "Ctrl+Enter", "Shift+Return", "Alt+NumpadEnter", "Meta+KP_Enter",
        ] {
            assert!(PasteShortcut::parse(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn completion_needs_continuous_consent_and_complete_audio() {
        let mut settings = InsertSettings::default();
        assert!(!paste_on_completion(&settings, true, false));
        settings.paste_on_completion = true;
        assert!(!paste_on_completion(&settings, false, false));
        assert!(!paste_on_completion(&settings, true, true));
        assert!(paste_on_completion(&settings, true, false));
        settings.method = InsertMethod::Type;
        assert!(!paste_on_completion(&settings, true, false));
    }

    #[test]
    fn ctrl_v_uses_the_active_layout_and_valid_modifier_keys() {
        let shortcut = PasteShortcut::parse(&InsertSettings::default().paste_shortcut).unwrap();
        let types = [X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] }];
        let mut keys = [
            X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe3] },
            X11Key { group_info: 2, width: 1, types: [0; 4], symbols: vec![0x76, 0x78] },
            X11Key { group_info: 2, width: 1, types: [0; 4], symbols: vec![0x78, 0x76] },
        ];
        let mut modifiers = [0, 0, 8, 0, 0, 0, 0, 0];
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), Some(vec![8, 9]));
        assert_eq!(paste_keycodes(8, &keys, &types, 1, &modifiers, &shortcut), Some(vec![8, 10]));
        modifiers[3] = 9;
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None);
        modifiers[3] = 8;
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None);
        modifiers[3] = 0;
        keys[0].symbols = vec![0xffe4];
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), Some(vec![8, 9]), "Right Ctrl works too");
        assert_eq!(paste_keycodes(8, &keys, &types, 4, &modifiers, &shortcut), None);
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &[], &shortcut), None);
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &[0; 7], &shortcut), None);
        assert_eq!(paste_keycodes(255, &keys, &types, 0, &modifiers, &shortcut), None);
        keys[1].width = 0;
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None);
    }

    #[test]
    fn colemak_altgr_symbols_do_not_make_ctrl_v_ambiguous() {
        let shortcut = PasteShortcut::parse(&InsertSettings::default().paste_shortcut).unwrap();
        let types = [
            X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] },
            X11KeyType { modifiers: 0x81, num_levels: 4, levels: vec![(1, 1, 0), (0x80, 2, 0), (0x81, 3, 0)] },
        ];
        let mut keys: Vec<_> = (0..19).map(|_| X11Key { group_info: 0, width: 0, types: [0; 4], symbols: vec![] }).collect();
        keys[0] = X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe3] };
        keys[13] = X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe1] };
        // XKB's four levels become xmodmap's expanded "v V v V oe OE oe" row.
        keys[18] = X11Key { group_info: 1, width: 4, types: [1; 4], symbols: vec![0x76, 0x56, 0x13bd, 0x13bc] };
        let mut modifiers = [0; 24];
        modifiers[0] = 50;
        modifiers[6] = 37;
        assert_eq!(paste_keycodes(37, &keys, &types, 0, &modifiers, &shortcut), Some(vec![37, 55]));
        let shifted = PasteShortcut::parse("Ctrl+Shift+KeyV").unwrap();
        assert_eq!(paste_keycodes(37, &keys, &types, 0, &modifiers, &shifted), Some(vec![37, 50, 55]), "Consumed Shift produces uppercase V, not the AltGr symbol");
    }

    #[test]
    fn ctrl_v_honors_key_group_wrap_clamp_and_redirect() {
        let shortcut = PasteShortcut::parse(&InsertSettings::default().paste_shortcut).unwrap();
        let types = [X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] }];
        let mut keys = [
            X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe3] },
            X11Key { group_info: 2, width: 1, types: [0; 4], symbols: vec![0x78, 0x76] },
        ];
        let modifiers = [0, 0, 8, 0, 0, 0, 0, 0];
        for (info, group, expected) in [(2, 3, true), (2, 2, false), (0x42, 2, true), (0x92, 2, true), (0xb2, 2, false)] {
            keys[1].group_info = info;
            assert_eq!(paste_keycodes(8, &keys, &types, group, &modifiers, &shortcut).is_some(), expected);
        }
        for info in [0, 5, 0xc2] {
            keys[1].group_info = info;
            assert_eq!(paste_keycodes(8, &keys, &types, 3, &modifiers, &shortcut), None);
        }
        keys[1].group_info = 2;
        keys[1].types = [9; 4];
        assert_eq!(paste_keycodes(8, &keys, &types, 1, &modifiers, &shortcut), None);
    }

    #[test]
    fn ctrl_v_rejects_changed_or_consumed_control_levels() {
        let shortcut = PasteShortcut::parse(&InsertSettings::default().paste_shortcut).unwrap();
        let mut types = [
            X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] },
            X11KeyType { modifiers: 4, num_levels: 2, levels: vec![(4, 1, 0)] },
        ];
        let mut keys = [
            X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe3] },
            X11Key { group_info: 1, width: 2, types: [1; 4], symbols: vec![0x76, 0x78] },
        ];
        let modifiers = [0, 0, 8, 0, 0, 0, 0, 0];
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None);
        keys[1].symbols = vec![0x76, 0x76];
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "Ctrl must remain part of the shortcut");
        types[1].levels[0].2 = 4;
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), Some(vec![8, 9]));
        types[1].levels[0].1 = 2;
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None);
    }

    #[test]
    fn common_chords_use_canonical_modifier_slots_and_consistent_press_order() {
        let types = [
            X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] },
            X11KeyType { modifiers: 1, num_levels: 2, levels: vec![(1, 1, 0)] },
        ];
        let mut keys: Vec<_> = [0xffe3, 0xffe9, 0xffe1, 0xffeb, 0x76, 0xff63, 0x78, 0xffc9, 0x31, 0x3b]
            .into_iter().map(|s| X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![s] }).collect();
        for (index, shifted) in [(4, 0x56), (6, 0x58), (8, 0x21), (9, 0x3a)] {
            keys[index].width = 2;
            keys[index].types = [1; 4];
            keys[index].symbols.push(shifted);
        }
        let modifiers = [10, 0, 8, 9, 0, 0, 11, 0];
        for right_only in [false, true] {
            if right_only {
                for key in &mut keys[..4] { key.symbols[0] += 1; }
            }
            for (input, expected) in [
                ("Ctrl+KeyV", vec![8, 12]), ("Ctrl+Shift+V", vec![8, 10, 12]),
                ("Shift+Insert", vec![10, 13]), ("Alt+KeyX", vec![9, 14]),
                ("Super+F12", vec![11, 15]), ("Meta+KeyX", vec![11, 14]), ("F12", vec![15]),
                ("Super+Shift+Alt+Control+KeyV", vec![8, 9, 10, 11, 12]),
                ("Shift+Digit1", vec![10, 16]), ("Ctrl+Shift+Semicolon", vec![8, 10, 17]),
            ] {
                let shortcut = PasteShortcut::parse(input).unwrap();
                assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), Some(expected), "{input}, right_only={right_only}");
            }
        }
        let shortcut = PasteShortcut::parse("Ctrl+Alt+Shift+Super+V").unwrap();
        for slot in [0, 2, 3, 6] {
            let mut bad = modifiers;
            bad[4] = bad[slot];
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &bad, &shortcut), None, "Modifier in two slots");
            bad[slot] = 0;
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &bad, &shortcut), None, "Modifier in the wrong slot");
        }
        let mut bad = modifiers;
        bad[4] = 12;
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &bad, &shortcut), None, "Primary must not be any modifier");
    }

    #[test]
    fn shifted_chords_reject_consumed_or_changed_nonshift_modifiers() {
        for (input, mask, slot, symbol) in [("Ctrl+Shift+V", 4, 2, 0xffe3), ("Alt+Shift+V", 8, 3, 0xffe9), ("Super+Shift+V", 64, 6, 0xffeb)] {
            let shortcut = PasteShortcut::parse(input).unwrap();
            let mut types = [
                X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] },
                X11KeyType { modifiers: mask | 1, num_levels: 4, levels: vec![(1, 1, 0), (mask, 2, 0), (mask | 1, 3, 0)] },
            ];
            let mut keys = [
                X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![symbol] },
                X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe1] },
                X11Key { group_info: 1, width: 4, types: [1; 4], symbols: vec![0x76, 0x56, 0x76, 0x56] },
            ];
            let mut modifiers = [0; 8];
            modifiers[slot] = 8;
            modifiers[0] = 9;
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "{input}: nonshift modifier consumed");
            types[1].levels[2].2 = mask;
            let expected = if mask == 64 { vec![9, 8, 10] } else { vec![8, 9, 10] };
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), Some(expected), "{input}: only Shift consumed");
            keys[2].symbols[3] = 0x58;
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "{input}: modifier changes the shifted symbol");
            keys[2].symbols[3] = 0x56;
            types[1].levels[2].2 = 0x80;
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "{input}: malformed preserve mask");
        }
    }

    #[test]
    fn active_levels_never_emit_enter_and_malformed_types_fail_closed() {
        let shortcut = PasteShortcut::parse("Shift+V").unwrap();
        let mut types = [
            X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] },
            X11KeyType { modifiers: 1, num_levels: 2, levels: vec![(1, 1, 0)] },
        ];
        let mut keys = [
            X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe1] },
            X11Key { group_info: 1, width: 2, types: [1; 4], symbols: vec![0x76, 0x56] },
        ];
        let modifiers = [8, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), Some(vec![8, 9]));
        for enter in [0, 0xff0d, 0xff8d] {
            keys[1].symbols[1] = enter;
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "Primary's actual Shift level");
            keys[1].symbols[1] = 0x56;
            keys[0] = X11Key { group_info: 1, width: 2, types: [1; 4], symbols: vec![0xffe1, enter] };
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "Modifier's actual release level");
            keys[0] = X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe1] };
        }
        for (levels, entry) in [(0, (1, 1, 0)), (3, (1, 1, 0)), (2, (1, 2, 0)), (2, (4, 1, 0)), (2, (1, 1, 4))] {
            types[1].num_levels = levels;
            types[1].levels[0] = entry;
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None);
        }
        types[1] = X11KeyType { modifiers: 1, num_levels: 2, levels: vec![(1, 1, 0)] };
        keys[1].symbols.pop();
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "Malformed symbol count");
        keys[1].symbols.push(0x56);
        for shortcut in [PasteShortcut { modifiers: 0x80, keysym: 0x76 }, PasteShortcut { modifiers: 0, keysym: 0x76 }, PasteShortcut { modifiers: 1, keysym: 0xff0d }] {
            assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "Direct construction cannot bypass basic safety");
        }
    }

    #[test]
    fn modifier_levels_follow_press_and_reverse_release_order() {
        let shortcut = PasteShortcut::parse("Ctrl+Shift+V").unwrap();
        let types = [
            X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] },
            X11KeyType { modifiers: 5, num_levels: 2, levels: vec![(4, 1, 0)] },
            X11KeyType { modifiers: 1, num_levels: 2, levels: vec![(1, 1, 0)] },
        ];
        let mut keys = [
            X11Key { group_info: 1, width: 2, types: [1; 4], symbols: vec![0xffe3, 0xff0d] },
            X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe1] },
            X11Key { group_info: 1, width: 2, types: [2; 4], symbols: vec![0x76, 0x56] },
        ];
        let modifiers = [9, 0, 8, 0, 0, 0, 0, 0];
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "Ctrl is released after Shift, not with the full chord's modifiers");
        keys[0].symbols[1] = 0xffe3;
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), Some(vec![8, 9, 10]));
        keys[1] = X11Key { group_info: 1, width: 2, types: [1; 4], symbols: vec![0xffe1, 0xff8d] };
        assert_eq!(paste_keycodes(8, &keys, &types, 0, &modifiers, &shortcut), None, "Shift is pressed with Ctrl already active");
    }

    #[test]
    fn only_known_editors_and_single_line_text_are_eligible() {
        assert!(allows_explicit_x11_paste("org.gnome.TextEditor", "Hello, world.", false));
        assert!(allows_explicit_x11_paste("Kate", "Gr\u{fc}\u{df}e", false));
        for class in ["firefox", "chromium", "konsole", "gnome-terminal", "", "kate-terminal"] {
            assert!(!allows_explicit_x11_paste(class, "hello", false), "{class}");
            assert!(allows_explicit_x11_paste(class, "hello", true), "{class}");
        }
        for text in ["", "  ", "hello\nworld", "hello\u{2028}world", "hello\u{2029}world", "\r", "\t", "\0", "\u{1b}[31m"] {
            for any_window in [false, true] {
                assert!(!allows_explicit_x11_paste("gedit", text, any_window));
                assert!(!allows_explicit_x11_paste("firefox", text, any_window));
            }
        }
    }
}
