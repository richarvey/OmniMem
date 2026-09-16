//! Chunking strategies for `remember_document` (`memory/chunking.py`).

use std::sync::LazyLock;

use regex::Regex;

use crate::Result;
use crate::error::invalid;

pub const VALID_STRATEGIES: [&str; 4] = ["fixed_tokens", "paragraphs", "sentences", "turn_pairs"];

const DEFAULT_FIXED_TOKEN_SIZE: usize = 200;
/// Below this a document shatters into hundreds of near-empty chunks, each
/// embedded and stored on its own.
pub const MIN_FIXED_TOKEN_SIZE: usize = 20;
const DEFAULT_FIXED_TOKEN_OVERLAP: f64 = 0.1;

static TURN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^\s*(user|assistant|system)\s*:\s*").expect("valid regex"));
static PARAGRAPH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n\s*\n+").expect("valid regex"));
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid regex"));

const ABBREVIATIONS: [&str; 16] = [
    "mr", "mrs", "ms", "dr", "st", "vs", "etc", "e.g", "i.e", "fig", "no", "inc", "ltd", "co",
    "jr", "sr",
];

/// Pair each User turn with the Assistant turn after it; everything else
/// stands alone. No markers at all is one chunk.
pub fn chunk_turn_pairs(content: &str) -> Vec<String> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    let matches: Vec<regex::Captures<'_>> = TURN_RE.captures_iter(content).collect();
    if matches.is_empty() {
        return vec![content.trim().to_owned()];
    }
    let mut turns: Vec<(String, String)> = Vec::new();
    for (i, m) in matches.iter().enumerate() {
        let speaker = capitalise(&m[1].to_lowercase());
        let start = m.get(0).expect("whole match").end();
        let end = matches.get(i + 1).map_or(content.len(), |next| {
            next.get(0).expect("whole match").start()
        });
        let text = content[start..end].trim();
        if !text.is_empty() {
            turns.push((speaker, text.to_owned()));
        }
    }
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < turns.len() {
        let (speaker, text) = &turns[i];
        if speaker == "User" && turns.get(i + 1).is_some_and(|(s, _)| s == "Assistant") {
            chunks.push(format!("User: {text}\nAssistant: {}", turns[i + 1].1));
            i += 2;
        } else {
            chunks.push(format!("{speaker}: {text}"));
            i += 1;
        }
    }
    chunks
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Python's `(?<=[.!?])\s+(?=[A-Z"'\(])` split, done by hand because the
/// regex crate has no lookaround.
fn split_sentences(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        let (_, c) = chars[i];
        if matches!(c, '.' | '!' | '?') && chars.get(i + 1).is_some_and(|(_, n)| n.is_whitespace())
        {
            let mut j = i + 1;
            while chars.get(j).is_some_and(|(_, n)| n.is_whitespace()) {
                j += 1;
            }
            if let Some(&(next_at, next)) = chars.get(j)
                && (next.is_ascii_uppercase() || matches!(next, '"' | '\'' | '('))
            {
                let end = chars[i + 1].0;
                pieces.push(&text[start..end]);
                start = next_at;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    pieces.push(&text[start..]);
    pieces
}

/// Sentence boundaries, re-merging pieces that ended on an abbreviation.
pub fn chunk_sentences(content: &str) -> Vec<String> {
    let text = content.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buffer = String::new();
    for piece in split_sentences(text) {
        let candidate = if buffer.is_empty() {
            piece.trim().to_owned()
        } else {
            format!("{buffer} {piece}").trim().to_owned()
        };
        let stripped = candidate.trim_end_matches(['.', '!', '?']);
        let last_word = WHITESPACE_RE
            .split(stripped)
            .last()
            .unwrap_or("")
            .to_lowercase();
        let last_word = last_word.trim_end_matches('.');
        if ABBREVIATIONS.contains(&last_word) {
            buffer = candidate;
            continue;
        }
        out.push(candidate);
        buffer.clear();
    }
    if !buffer.is_empty() {
        out.push(buffer);
    }
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

pub fn chunk_paragraphs(content: &str) -> Vec<String> {
    PARAGRAPH_RE
        .split(content.trim())
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Word windows with 10% overlap.
pub fn chunk_fixed_tokens(content: &str, chunk_size: Option<i64>) -> Result<Vec<String>> {
    let size = match chunk_size {
        None | Some(0) => DEFAULT_FIXED_TOKEN_SIZE as i64,
        Some(n) => n,
    };
    if size < MIN_FIXED_TOKEN_SIZE as i64 {
        return Err(invalid(format!(
            "chunk_size must be >= {MIN_FIXED_TOKEN_SIZE}"
        )));
    }
    let size = size as usize;
    let words: Vec<&str> = content.split_whitespace().collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let overlap = (size as f64 * DEFAULT_FIXED_TOKEN_OVERLAP) as usize;
    let step = size.saturating_sub(overlap).max(1);
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let end = (i + size).min(words.len());
        chunks.push(words[i..end].join(" "));
        if i + size >= words.len() {
            break;
        }
        i += step;
    }
    Ok(chunks)
}

pub fn chunk(content: &str, strategy: &str, chunk_size: Option<i64>) -> Result<Vec<String>> {
    match strategy {
        "turn_pairs" => Ok(chunk_turn_pairs(content)),
        "sentences" => Ok(chunk_sentences(content)),
        "paragraphs" => Ok(chunk_paragraphs(content)),
        "fixed_tokens" => chunk_fixed_tokens(content, chunk_size),
        other => Err(invalid(format!(
            "Invalid chunk_strategy '{other}'. Must be one of: {}",
            VALID_STRATEGIES.join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_pairs_pair_user_with_assistant() {
        let text = "User: hello\nAssistant: hi there\nuser: another\nSYSTEM: note";
        assert_eq!(
            chunk_turn_pairs(text),
            [
                "User: hello\nAssistant: hi there",
                "User: another",
                "System: note"
            ]
        );
        assert_eq!(chunk_turn_pairs("no markers here"), ["no markers here"]);
        assert!(chunk_turn_pairs("   ").is_empty());
    }

    #[test]
    fn sentences_respect_abbreviations() {
        let text = "Dr. Smith arrived. He said hi! Was it \"fun\"? (Yes.) done e.g. This.";
        assert_eq!(
            chunk_sentences(text),
            [
                "Dr. Smith arrived.",
                "He said hi!",
                "Was it \"fun\"?",
                "(Yes.) done e.g. This."
            ]
        );
        assert_eq!(chunk_sentences("one. two. Three"), ["one. two.", "Three"]);
    }

    #[test]
    fn paragraphs_split_on_blank_lines() {
        assert_eq!(chunk_paragraphs("a\nb\n\n  \n\nc\n"), ["a\nb", "c"]);
    }

    #[test]
    fn fixed_tokens_overlap() {
        let text = (1..=50)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = chunk_fixed_tokens(&text, Some(20)).unwrap();
        assert_eq!(chunks.len(), 3);
        assert!(chunks[0].starts_with("1 ") && chunks[0].ends_with(" 20"));
        assert!(
            chunks[1].starts_with("19 20 21 "),
            "step 18 keeps a two-word overlap"
        );
        assert!(chunk_fixed_tokens("x", Some(-1)).is_err());
        assert!(chunk("x", "chapters", None).is_err());
    }

    #[test]
    fn fixed_tokens_refuse_tiny_windows() {
        let message = chunk_fixed_tokens("a b c", Some(1))
            .unwrap_err()
            .to_string();
        assert_eq!(message, "chunk_size must be >= 20");
        assert!(chunk_fixed_tokens("a b c", Some(19)).is_err());
        assert_eq!(chunk_fixed_tokens("a b c", Some(20)).unwrap(), ["a b c"]);
        assert_eq!(
            chunk_fixed_tokens("a b c", None).unwrap(),
            ["a b c"],
            "unset means the default window"
        );
    }
}
