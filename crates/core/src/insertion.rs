//! Conservative policy for explicit X11 paste when editable-field identity is unavailable.

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
