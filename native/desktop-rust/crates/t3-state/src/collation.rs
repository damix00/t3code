//! An approximation of `String.prototype.localeCompare` (ICU root collation) for ids.
//!
//! The projection reducer breaks ordinal ties with `localeCompare`. Ids are ASCII in practice
//! (UUIDs and provider-native ids), so this reproduces root collation for ASCII: spaces and
//! punctuation in ICU's order, then digits, then letters compared case-insensitively, with
//! lowercase before uppercase on a tie and code point order as the final tiebreak.

use std::cmp::Ordering;

/// ASCII spaces, punctuation and symbols in ICU root order (derived from Node's `localeCompare`).
const ICU_ASCII_SYMBOL_ORDER: &str = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn primary(c: char) -> (u8, u32) {
    if c.is_ascii_digit() {
        (2, u32::from(c))
    } else if c.is_alphabetic() {
        (3, u32::from(c.to_lowercase().next().unwrap_or(c)))
    } else if let Some(rank) = ICU_ASCII_SYMBOL_ORDER.find(c) {
        // `rank` is a byte offset into an ASCII string, so it is small.
        (1, u32::try_from(rank).unwrap_or(u32::MAX))
    } else {
        // Non-ASCII symbols: after the ASCII ones, by code point.
        (1, 0x100 + u32::from(c))
    }
}

fn tertiary(c: char) -> u8 {
    u8::from(c.is_uppercase())
}

pub fn locale_compare(left: &str, right: &str) -> Ordering {
    let primary_order = left.chars().map(primary).cmp(right.chars().map(primary));
    if primary_order != Ordering::Equal {
        return primary_order;
    }
    let tertiary_order = left.chars().map(tertiary).cmp(right.chars().map(tertiary));
    if tertiary_order != Ordering::Equal {
        return tertiary_order;
    }
    left.cmp(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn symbol_table_is_icu_order() {
        // `[...symbols].sort((a, b) => a.localeCompare(b))` over printable ASCII in Node.
        let mut symbols: Vec<char> = (32u8..127)
            .map(char::from)
            .filter(|c| !c.is_ascii_alphanumeric())
            .collect();
        symbols.sort_by(|a, b| locale_compare(&a.to_string(), &b.to_string()));
        let sorted: String = symbols.into_iter().collect();
        assert_eq!(sorted, ICU_ASCII_SYMBOL_ORDER);
    }

    #[test]
    fn matches_known_locale_compare_results() {
        // Each pair is `a.localeCompare(b) < 0` in Node (ICU root locale).
        for (a, b) in [
            ("item-a", "item-b"),
            ("a", "B"),
            ("B", "c"),
            ("a", "A"),
            ("-", "0"),
            ("9", "a"),
            ("_x", "0"),
            ("item-10", "item-2"),
            ("abc", "abcd"),
            ("a_b", "a-b"),
            ("a b", "a-b"),
            ("Ab", "aC"),
            ("aB", "Ab"),
            ("a-b", "ab"),
            ("x", "x "),
        ] {
            assert_eq!(locale_compare(a, b), Ordering::Less, "{a} < {b}");
            assert_eq!(locale_compare(b, a), Ordering::Greater, "{b} > {a}");
        }
        assert_eq!(locale_compare("same", "same"), Ordering::Equal);
    }

    proptest! {
        #[test]
        fn is_a_total_order(a in "[a-zA-Z0-9_-]{0,8}", b in "[a-zA-Z0-9_-]{0,8}") {
            let forward = locale_compare(&a, &b);
            prop_assert_eq!(forward, locale_compare(&b, &a).reverse());
            prop_assert_eq!(forward == Ordering::Equal, a == b);
        }

        #[test]
        fn is_transitive(a in "[a-zA-Z0-9-]{0,4}", b in "[a-zA-Z0-9-]{0,4}", c in "[a-zA-Z0-9-]{0,4}") {
            let mut items = [a, b, c];
            items.sort_by(|x, y| locale_compare(x, y));
            prop_assert!(locale_compare(&items[0], &items[2]) != Ordering::Greater);
        }
    }
}
