//! Configurable policy for opted-in X11 paste when editable-field identity is unavailable.

use crate::settings::{InsertMethod, InsertSettings};

/// Consent must exist at both ends of a recording; interrupted audio always needs review.
pub fn paste_on_completion(settings: &InsertSettings, opted_in_at_start: bool, interrupted: bool) -> bool {
    opted_in_at_start && settings.paste_on_completion && settings.method == InsertMethod::Paste && !interrupted
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

/// Resolve only the active layout and the levels used by Ctrl+V, not AltGr/other layouts.
/// The caller must establish that no modifiers are held, latched or locked.
pub fn ctrl_v_keycodes(min: u8, keys: &[X11Key], types: &[X11KeyType], group: u8, modifiers: &[u8]) -> Option<(u8, u8)> {
    if group > 3 || modifiers.is_empty() || !modifiers.len().is_multiple_of(8) {
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
        let (_, level, preserve) = kind.levels.iter().find(|(mask, _, _)| mods & kind.modifiers == *mask).copied().unwrap_or((0, 0, 0));
        if level >= key.width || level >= kind.num_levels { return None; }
        Some((key.symbols[usize::from(group) * usize::from(key.width) + usize::from(level)], kind.modifiers & !preserve))
    };
    let per_modifier = modifiers.len() / 8;
    let control_range = 2 * per_modifier..3 * per_modifier;
    let keycode = |index| min.checked_add(u8::try_from(index).ok()?).filter(|&key| key >= 8);
    let control = keys.iter().enumerate().find_map(|(index, key)| {
        let code = keycode(index)?;
        (matches!(symbol(key, 0)?.0, 0xffe3 | 0xffe4)
            && modifiers[control_range.clone()].contains(&code)
            && !modifiers.iter().enumerate().any(|(i, &key)| key == code && !control_range.contains(&i))).then_some(code)
    })?;
    let v = keys.iter().enumerate().find_map(|(index, key)| {
        let code = keycode(index)?;
        let (with_control, consumed) = symbol(key, 4)?;
        (symbol(key, 0)?.0 == 0x76 && with_control == 0x76 && consumed & 4 == 0 && !modifiers.contains(&code)).then_some(code)
    })?;
    Some((control, v))
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
        let types = [X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] }];
        let mut keys = [
            X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe3] },
            X11Key { group_info: 2, width: 1, types: [0; 4], symbols: vec![0x76, 0x78] },
            X11Key { group_info: 2, width: 1, types: [0; 4], symbols: vec![0x78, 0x76] },
        ];
        let mut modifiers = [0, 0, 8, 0, 0, 0, 0, 0];
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &modifiers), Some((8, 9)));
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 1, &modifiers), Some((8, 10)));
        modifiers[3] = 9;
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &modifiers), None);
        modifiers[3] = 8;
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &modifiers), None);
        modifiers[3] = 0;
        keys[0].symbols = vec![0xffe4];
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &modifiers), Some((8, 9)), "Right Ctrl works too");
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 4, &modifiers), None);
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &[]), None);
        assert_eq!(ctrl_v_keycodes(255, &keys, &types, 0, &modifiers), None);
        keys[1].width = 0;
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &modifiers), None);
    }

    #[test]
    fn colemak_altgr_symbols_do_not_make_ctrl_v_ambiguous() {
        let types = [
            X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] },
            X11KeyType { modifiers: 0x81, num_levels: 4, levels: vec![(1, 1, 0), (0x80, 2, 0), (0x81, 3, 0)] },
        ];
        let mut keys: Vec<_> = (0..19).map(|_| X11Key { group_info: 0, width: 0, types: [0; 4], symbols: vec![] }).collect();
        keys[0] = X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe3] };
        // XKB's four levels become xmodmap's expanded "v V v V oe OE oe" row.
        keys[18] = X11Key { group_info: 1, width: 4, types: [1; 4], symbols: vec![0x76, 0x56, 0x13bd, 0x13bc] };
        let mut modifiers = [0; 24];
        modifiers[6] = 37;
        assert_eq!(ctrl_v_keycodes(37, &keys, &types, 0, &modifiers), Some((37, 55)));
    }

    #[test]
    fn ctrl_v_honors_key_group_wrap_clamp_and_redirect() {
        let types = [X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] }];
        let mut keys = [
            X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe3] },
            X11Key { group_info: 2, width: 1, types: [0; 4], symbols: vec![0x78, 0x76] },
        ];
        let modifiers = [0, 0, 8, 0, 0, 0, 0, 0];
        for (info, group, expected) in [(2, 3, true), (2, 2, false), (0x42, 2, true), (0x92, 2, true), (0xb2, 2, false)] {
            keys[1].group_info = info;
            assert_eq!(ctrl_v_keycodes(8, &keys, &types, group, &modifiers).is_some(), expected);
        }
        for info in [0, 5, 0xc2] {
            keys[1].group_info = info;
            assert_eq!(ctrl_v_keycodes(8, &keys, &types, 3, &modifiers), None);
        }
        keys[1].group_info = 2;
        keys[1].types = [9; 4];
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 1, &modifiers), None);
    }

    #[test]
    fn ctrl_v_rejects_changed_or_consumed_control_levels() {
        let mut types = [
            X11KeyType { modifiers: 0, num_levels: 1, levels: vec![] },
            X11KeyType { modifiers: 4, num_levels: 2, levels: vec![(4, 1, 0)] },
        ];
        let mut keys = [
            X11Key { group_info: 1, width: 1, types: [0; 4], symbols: vec![0xffe3] },
            X11Key { group_info: 1, width: 2, types: [1; 4], symbols: vec![0x76, 0x78] },
        ];
        let modifiers = [0, 0, 8, 0, 0, 0, 0, 0];
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &modifiers), None);
        keys[1].symbols = vec![0x76, 0x76];
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &modifiers), None, "Ctrl must remain part of the shortcut");
        types[1].levels[0].2 = 4;
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &modifiers), Some((8, 9)));
        types[1].levels[0].1 = 2;
        assert_eq!(ctrl_v_keycodes(8, &keys, &types, 0, &modifiers), None);
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
