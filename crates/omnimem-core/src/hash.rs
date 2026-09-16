//! The v7 content hash (`docs/v7-change-spec.md` §1).
//!
//! It has to be byte-identical across every implementation: the Mycelium
//! daemon runs on Windows, macOS and Linux, and a custodian must be able to
//! verify content that originated on another platform. So the content is
//! normalised before hashing:
//!
//! 1. the `content` field only
//! 2. Unicode NFC
//! 3. line endings to LF
//! 4. trailing whitespace stripped from each line
//! 5. leading and trailing blank lines stripped
//! 6. UTF-8, SHA-256, lowercase hex, prefixed `sha256:`
//!
//! The accepted trade-off: an edit that only changes whitespace at the end
//! of a line, or blank lines at the edges, does not change the hash. That is
//! right for prose and would be wrong for content where indentation or
//! trailing space carries meaning; leading indentation is preserved.

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// Prefix on every content hash, naming the algorithm.
pub const HASH_PREFIX: &str = "sha256:";

/// The normalised form of `content` that [`content_hash`] digests.
///
/// "Whitespace" is Rust's `char::is_whitespace`, the Unicode `White_Space`
/// property. Conformance vectors shared with other implementations must pin
/// this, because Python's `str.rstrip()` also strips a few control
/// characters (U+001C..U+001F) that are not `White_Space`.
pub fn normalise_content(content: &str) -> String {
    let nfc: String = content.nfc().collect();
    let unified = nfc.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = unified.split('\n').map(str::trim_end).collect();
    let first = lines.iter().position(|l| !l.is_empty());
    let last = lines.iter().rposition(|l| !l.is_empty());
    match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].join("\n"),
        _ => String::new(),
    }
}

/// `sha256:` plus the lowercase hex SHA-256 of the normalised content.
pub fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(normalise_content(content).as_bytes());
    let mut out = String::with_capacity(HASH_PREFIX.len() + digest.len() * 2);
    out.push_str(HASH_PREFIX);
    for byte in &digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digest() {
        // sha256("hello"), the standard test vector.
        assert_eq!(
            content_hash("hello"),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn empty_and_blank_content_hash_the_same() {
        let empty = content_hash("");
        assert_eq!(content_hash("   \n\t\n\r\n"), empty);
        // sha256 of zero bytes.
        assert_eq!(
            empty,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn line_endings_do_not_change_the_hash() {
        let lf = content_hash("first line\nsecond line");
        assert_eq!(content_hash("first line\r\nsecond line"), lf);
        assert_eq!(content_hash("first line\rsecond line"), lf);
    }

    #[test]
    fn nfd_and_nfc_hash_the_same() {
        assert_eq!(content_hash("Cafe\u{0301}"), content_hash("Caf\u{00e9}"));
    }

    #[test]
    fn trailing_whitespace_and_edge_blank_lines_are_ignored() {
        let clean = content_hash("a decision\nwith a reason");
        assert_eq!(
            content_hash("\n\n  \na decision   \nwith a reason\t\n\n"),
            clean
        );
    }

    #[test]
    fn meaningful_differences_still_change_the_hash() {
        let base = content_hash("a\nb");
        assert_ne!(
            content_hash("a\n\nb"),
            base,
            "interior blank lines are content"
        );
        assert_ne!(
            content_hash("  a\nb"),
            base,
            "leading indentation is content"
        );
        assert_ne!(content_hash("A\nb"), base, "case is content");
    }

    #[test]
    fn emoji_and_cjk_are_stable() {
        let text = "日本語のテキストと絵文字 🚀🔥";
        assert_eq!(content_hash(text), content_hash(&format!("{text}  \r\n")));
        assert!(content_hash(text).starts_with(HASH_PREFIX));
        assert_eq!(content_hash(text).len(), HASH_PREFIX.len() + 64);
    }
}
