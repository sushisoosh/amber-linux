//! Conservative policy for opted-in X11 paste when editable-field identity is unavailable.

use crate::settings::{InsertMethod, InsertSettings};

/// Consent must exist at both ends of a recording; interrupted audio always needs review.
pub fn paste_on_completion(settings: &InsertSettings, opted_in_at_start: bool, interrupted: bool) -> bool {
    opted_in_at_start && settings.paste_on_completion && settings.method == InsertMethod::Paste && !interrupted
}

/// Only layouts where unshifted V is unambiguous in every populated group are safe for Ctrl+V.
pub fn ctrl_v_keycodes(min: u8, width: usize, symbols: &[u32], modifiers: &[u8]) -> Option<(u8, u8)> {
    if width < 2 || !symbols.len().is_multiple_of(width) || modifiers.is_empty() || !modifiers.len().is_multiple_of(8) {
        return None;
    }
    let rows = || symbols.chunks_exact(width).enumerate();
    let control = rows().find(|(_, row)| row.contains(&0xffe3) && row.iter().all(|&s| s == 0 || s == 0xffe3))?.0;
    let v = rows().find(|(_, row)| {
        row[0] == 0x76 && row.chunks(2).all(|pair| pair.iter().all(|&s| s == 0) || pair == [0x76, 0x56])
    })?.0;
    let (control, v) = (min.checked_add(u8::try_from(control).ok()?)?, min.checked_add(u8::try_from(v).ok()?)?);
    let per_modifier = modifiers.len() / 8;
    let control_range = 2 * per_modifier..3 * per_modifier;
    (modifiers[control_range.clone()].contains(&control)
        && !modifiers.iter().enumerate().any(|(i, &key)| key == control && !control_range.contains(&i))
        && !modifiers.contains(&v)).then_some((control, v))
}

pub fn allows_explicit_x11_paste(class: &str, text: &str) -> bool {
    const EDITORS: &[&str] = &[
        "gedit", "org.gnome.gedit", "gnome-text-editor", "org.gnome.texteditor",
        "kate", "org.kde.kate", "kwrite", "org.kde.kwrite", "mousepad", "xed", "pluma", "leafpad",
    ];
    !text.trim().is_empty()
        && !text.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        && EDITORS.iter().any(|editor| class.eq_ignore_ascii_case(editor))
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
    fn ctrl_v_requires_standard_modifiers_and_unambiguous_layout_groups() {
        let symbols = [0xffe3, 0, 0, 0, 0x76, 0x56, 0x76, 0x56];
        let mut modifiers = [0, 0, 8, 0, 0, 0, 0, 0];
        assert_eq!(ctrl_v_keycodes(8, 4, &symbols, &modifiers), Some((8, 9)));
        assert_eq!(ctrl_v_keycodes(8, 3, &[0xffe3, 0, 0, 0x76, 0x56, 0], &modifiers), Some((8, 9)));
        modifiers[3] = 9;
        assert_eq!(ctrl_v_keycodes(8, 4, &symbols, &modifiers), None);
        modifiers[3] = 8;
        assert_eq!(ctrl_v_keycodes(8, 4, &symbols, &modifiers), None);
        modifiers[3] = 0;
        let mut ambiguous = symbols;
        ambiguous[6] = 0x78;
        assert_eq!(ctrl_v_keycodes(8, 4, &ambiguous, &modifiers), None);
        assert_eq!(ctrl_v_keycodes(8, 0, &symbols, &modifiers), None);
        assert_eq!(ctrl_v_keycodes(8, 4, &symbols, &[]), None);
    }

    #[test]
    fn only_known_editors_and_single_line_text_are_eligible() {
        assert!(allows_explicit_x11_paste("org.gnome.TextEditor", "Hello, world."));
        assert!(allows_explicit_x11_paste("Kate", "Gr\u{fc}\u{df}e"));
        for class in ["firefox", "chromium", "konsole", "gnome-terminal", "", "kate-terminal"] {
            assert!(!allows_explicit_x11_paste(class, "hello"), "{class}");
        }
        for text in ["", "  ", "hello\nworld", "hello\u{2028}world", "hello\u{2029}world", "\r", "\t", "\0", "\u{1b}[31m"] {
            assert!(!allows_explicit_x11_paste("gedit", text));
        }
    }
}
