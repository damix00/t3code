//! Port of `deriveThreadTitleSeed` (client-runtime `operations/threadTitle.ts`) for plain text
//! composer input, plus `truncate` from `@t3tools/shared/String`.

/// JavaScript's `\s` and `String.prototype.trim` whitespace. Unlike Rust's `char::is_whitespace`
/// it includes U+FEFF and excludes U+0085.
fn is_js_whitespace(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `truncate(text, 50)`: trims, then cuts to 50 code points and appends `...`.
pub fn truncate(text: &str, max_chars: usize) -> String {
    let trimmed = js_trim(text);
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }
    let mut cut: String = trimmed.chars().take(max_chars).collect();
    cut.push_str("...");
    cut
}

/// `trim()` then collapse runs of whitespace to one space.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in js_trim(text).chars() {
        if is_js_whitespace(c) {
            pending_space = true;
        } else {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        }
    }
    out
}

/// The title a new thread starts with before the server generates one.
pub fn derive_thread_title_seed(text: &str) -> String {
    let normalized = normalize(text);
    if normalized.is_empty() {
        "New thread".to_owned()
    } else {
        truncate(&normalized, 50)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn short_text_is_normalized() {
        assert_eq!(
            derive_thread_title_seed("  fix   the\n\tbug  "),
            "fix the bug"
        );
    }

    #[test]
    fn long_text_is_cut_at_fifty_code_points() {
        let text = "a".repeat(60);
        assert_eq!(
            derive_thread_title_seed(&text),
            format!("{}...", "a".repeat(50))
        );
        let exactly = "b".repeat(50);
        assert_eq!(derive_thread_title_seed(&exactly), exactly);
    }

    #[test]
    fn counts_code_points_not_bytes() {
        let text = "é".repeat(51);
        assert_eq!(
            derive_thread_title_seed(&text),
            format!("{}...", "é".repeat(50))
        );
        let emoji = "😀".repeat(50);
        assert_eq!(derive_thread_title_seed(&emoji), emoji);
    }

    #[test]
    fn empty_input_falls_back() {
        assert_eq!(derive_thread_title_seed(""), "New thread");
        assert_eq!(derive_thread_title_seed(" \n\t\u{feff}"), "New thread");
    }

    #[test]
    fn whitespace_matches_javascript() {
        // U+FEFF is JS whitespace; U+0085 is not.
        assert_eq!(derive_thread_title_seed("\u{feff}a\u{feff}b"), "a b");
        assert_eq!(derive_thread_title_seed("a\u{85}b"), "a\u{85}b");
        assert_eq!(derive_thread_title_seed("a\u{3000}\u{2028}b"), "a b");
    }

    #[test]
    fn truncate_trims_before_measuring() {
        assert_eq!(truncate("   abc   ", 3), "abc");
        assert_eq!(truncate("abcd", 3), "abc...");
    }

    proptest! {
        #[test]
        fn seeds_are_bounded_and_clean(text in "\\PC{0,200}") {
            let seed = derive_thread_title_seed(&text);
            prop_assert!(seed.chars().count() <= 53);
            prop_assert!(!seed.contains("  "));
            prop_assert_eq!(js_trim(&seed).len(), seed.len());
        }
    }
}
