/// Replaces terminal control characters with a visible neutral marker.
///
/// Structured report values remain unchanged; this helper is for the final
/// plain-text and terminal-output boundary only.
pub fn sanitize_terminal_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sanitize_terminal_text;

    #[test]
    fn replaces_terminal_control_characters() {
        let sanitized = sanitize_terminal_text("before\n\r\t\u{1b}[31m\u{7f}\u{80}after");

        assert_eq!(sanitized, "before����[31m��after");
        assert!(sanitized.chars().all(|character| !character.is_control()));
    }

    #[test]
    fn preserves_ordinary_unicode_text() {
        assert_eq!(
            sanitize_terminal_text("Crème 日本語 — 😀"),
            "Crème 日本語 — 😀"
        );
    }
}
