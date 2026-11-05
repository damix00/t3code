//! Port of `packages/shared/src/desktopBootstrapToken.ts`.
//!
//! The desktop hands its backend a secret at launch. Both sides derive a token per 12-hour
//! window as `hex(HMAC-SHA256(secret, "t3-desktop-bootstrap:{window}"))`; the backend accepts the
//! previous, current and next window, so a long-running desktop always holds a valid credential
//! without the launch token living forever.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub const WINDOW_MS: u64 = 12 * 60 * 60 * 1000;

pub fn window_index(now_ms: u64) -> u64 {
    now_ms / WINDOW_MS
}

fn derive(secret: &str, window: u64) -> String {
    // HMAC accepts keys of any length.
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        unreachable!("HMAC-SHA256 takes keys of any length")
    };
    mac.update(format!("t3-desktop-bootstrap:{window}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// The token for the window containing `now_ms`.
pub fn current_token(secret: &str, now_ms: u64) -> String {
    derive(secret, window_index(now_ms))
}

/// Whether `token` is valid at `now_ms`, as the backend checks it.
pub fn is_valid_token(secret: &str, token: &str, now_ms: u64) -> bool {
    let current = window_index(now_ms);
    [
        current.checked_sub(1),
        Some(current),
        current.checked_add(1),
    ]
    .into_iter()
    .flatten()
    .any(|window| constant_time_eq(derive(secret, window).as_bytes(), token.as_bytes()))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// A fresh launch secret: 32 random bytes, hex encoded.
pub fn generate_secret() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    /// Computed with the upstream TypeScript implementation under Node.
    const VECTORS: &[(&str, u64, u64, &str)] = &[
        (
            "secret",
            0,
            0,
            "c0bf962c57658da8944979b3a9b7b8233f78e4b156eb44690e30d2106eea0813",
        ),
        (
            "secret",
            43_199_999,
            0,
            "c0bf962c57658da8944979b3a9b7b8233f78e4b156eb44690e30d2106eea0813",
        ),
        (
            "secret",
            43_200_000,
            1,
            "cafc7bd90664460949f40741d82c4b67a23963ccde52f4da8e32c3b31cdc923e",
        ),
        (
            "s3cr3t-✓",
            1_762_300_000_000,
            40_793,
            "748d0702360a4a5966c66d8b798e639b73d13c2e218c9d60a701a5412b523bc1",
        ),
        (
            "",
            1_762_300_000_000,
            40_793,
            "51df984ac5b9d929619d394838b1796ffa29808d6d2a1a46b50bf736b3577c61",
        ),
    ];

    #[test]
    fn matches_the_upstream_vectors() {
        for &(secret, now, window, expected) in VECTORS {
            assert_eq!(window_index(now), window, "{secret} at {now}");
            assert_eq!(current_token(secret, now), expected, "{secret} at {now}");
        }
    }

    #[test]
    fn accepts_adjacent_windows_only() {
        let now = 1_762_300_000_000;
        let token = current_token("secret", now);
        assert!(is_valid_token("secret", &token, now));
        assert!(is_valid_token("secret", &token, now + WINDOW_MS));
        assert!(is_valid_token("secret", &token, now - WINDOW_MS));
        assert!(!is_valid_token("secret", &token, now + 2 * WINDOW_MS));
        assert!(!is_valid_token("secret", &token, now - 2 * WINDOW_MS));
        assert!(!is_valid_token("other", &token, now));
        assert!(!is_valid_token("secret", &token[1..], now));
        assert!(is_valid_token("secret", &current_token("secret", 0), 0));
    }

    #[test]
    fn secrets_are_random_hex() {
        let first = generate_secret();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(first, generate_secret());
    }

    #[test]
    fn now_is_after_2026() {
        assert!(now_ms() > 1_767_225_600_000);
    }

    proptest! {
        #[test]
        fn tokens_are_stable_within_a_window(secret in ".{0,40}", window in 0u64..100_000, offset in 0..WINDOW_MS) {
            let start = window * WINDOW_MS;
            prop_assert_eq!(current_token(&secret, start), current_token(&secret, start + offset));
            prop_assert!(is_valid_token(&secret, &current_token(&secret, start + offset), start));
        }

        #[test]
        fn tokens_are_64_hex_chars(secret in ".{0,40}", now in any::<u64>()) {
            let token = current_token(&secret, now);
            prop_assert_eq!(token.len(), 64);
            prop_assert!(token.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        }
    }
}
