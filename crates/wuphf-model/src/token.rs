//! Comparing a presented bearer token with an expected one in constant time:
//! a comparison that stops at the first differing byte answers sooner the
//! fewer bytes match, so its timing tells an attacker how much of a guess is
//! right.

use std::hint::black_box;

/// Whether `presented` is `expected`. It takes the same time whatever the
/// bytes are: it reads every byte of `expected` (and as many of `presented`),
/// so timing reveals at most the expected token's length.
pub fn tokens_match(presented: &str, expected: &str) -> bool {
    let (presented, expected) = (presented.as_bytes(), expected.as_bytes());
    let mut difference = u8::from(presented.len() != expected.len());
    for (index, byte) in expected.iter().enumerate() {
        let other = presented.get(index).copied().unwrap_or(!byte);
        difference |= black_box(byte ^ other);
    }
    black_box(difference) == 0
}

/// Which of `expected` `presented` matches, comparing it with every one (no
/// early return), so timing reveals neither which nor how much.
pub fn matching_token<'a, T>(
    presented: &str,
    expected: impl IntoIterator<Item = (T, &'a str)>,
) -> Option<T> {
    let mut found = None;
    for (item, token) in expected {
        if tokens_match(presented, token) && found.is_none() {
            found = Some(item);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_token_matches() {
        assert!(tokens_match("s3cret", "s3cret"));
        assert!(!tokens_match("s3creT", "s3cret"));
        assert!(!tokens_match("s3cre", "s3cret"), "a prefix");
        assert!(!tokens_match("s3crets", "s3cret"), "longer");
        assert!(!tokens_match("", "s3cret"));
        assert!(tokens_match("", ""));
        // A byte past the presented end never counts as matching.
        assert!(!tokens_match("a", "a\u{0}"));
    }

    #[test]
    fn the_matching_one_is_found_among_many() {
        let tokens = [("tg", "one"), ("wa", "two"), ("ig", "three")];
        assert_eq!(matching_token("two", tokens), Some("wa"));
        assert_eq!(matching_token("four", tokens), None);
    }
}
