//! Article summaries and digest item extraction (`rss_worker/summariser.py`).
//!
//! 6.x wrapped each call in its own two-attempt retry on top of the SDK's;
//! the `omnimem-llm` client already retries the same failures, so a failed
//! call here goes straight to the fallback.

use omnimem_core::LanguageModel;
use omnimem_engine::pyfmt::take_chars;
use omnimem_engine::skills::py_str;
use serde_json::Value;
use tracing::{debug, error, info, warn};

/// 6.x hard-coded the model for both calls.
pub const MODEL: &str = "claude-haiku-4-5-20251001";

const REFUSAL_PHRASES: [&str; 10] = [
    "i don't have access",
    "i do not have access",
    "i'm unable to",
    "i am unable to",
    "i cannot access",
    "i can't access",
    "i can't browse",
    "i cannot browse",
    "i'm not able to",
    "i am not able to",
];

const EXTRACT_PROMPT: &str = "You are analysing an article to extract individual news items, \
announcements, or updates. The article may cover a single topic \
or be a newsletter/digest containing many distinct items.\n\n\
For EACH distinct item, project, announcement, or piece of news, extract:\n\
- title: A short descriptive title (max 10 words)\n\
- who: Who or what is this about? (person, project, organisation, technology)\n\
- what: What happened, what does it do, or what was announced?\n\
- why: Why does this matter? Why should the reader care?\n\n\
Return ONLY a valid JSON array of objects. No markdown fences, no explanation.\n\
If the article covers a single topic, return an array with one object.\n\
Skip promotional content, sponsor mentions, and calls-to-action.\n\
Skip items where you cannot determine meaningful who/what/why.\n\n";

/// One item pulled out of a digest article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestItem {
    pub title: String,
    pub who: String,
    pub what: String,
    pub why: String,
}

/// The model declined, typically because it was asked about a URL.
pub fn is_refusal(text: &str) -> bool {
    let lower = text.to_lowercase();
    REFUSAL_PHRASES.iter().any(|p| lower.contains(p))
}

/// The truncation stand-in when no summary can be had.
pub fn fallback_summary(title: &str, content: &str) -> String {
    let mut preview = take_chars(content, 800).trim().to_owned();
    if content.chars().count() > 800 {
        preview.push_str("...");
    }
    format!("{title}. {preview}")
}

/// A two or three sentence summary, the truncation fallback when there is no
/// model or the call fails, or `None` when the model refuses.
pub fn summarise(
    llm: Option<&dyn LanguageModel>,
    title: &str,
    url: &str,
    content: &str,
) -> Option<String> {
    let Some(llm) = llm else {
        warn!("No valid ANTHROPIC_API_KEY set, falling back to truncation");
        return Some(fallback_summary(title, content));
    };
    let prompt = format!(
        "Summarise the following article in 2-3 sentences, focusing on what a developer might find \
         actionable or useful. Include the main technology or concept. Be concise.\n\n\
         Title: {title}\nURL: {url}\n\n{}",
        take_chars(content, 3000)
    );
    match llm.complete(MODEL, &prompt, 256) {
        Ok(reply) => {
            let summary = reply.trim();
            if is_refusal(summary) {
                warn!(title, url, "model refused to summarise, skipping");
                return None;
            }
            debug!(title, "summarised");
            Some(summary.to_owned())
        }
        Err(e) => {
            error!(title, error = %e, "summarisation failed");
            Some(fallback_summary(title, content))
        }
    }
}

/// Each distinct item in a newsletter or multi-topic article, or `None` when
/// there is no model, the call fails, the model refuses or nothing is usable.
pub fn extract_items(
    llm: Option<&dyn LanguageModel>,
    title: &str,
    url: &str,
    content: &str,
) -> Option<Vec<DigestItem>> {
    let Some(llm) = llm else {
        warn!("No valid ANTHROPIC_API_KEY, cannot extract items");
        return None;
    };
    let prompt = format!(
        "{EXTRACT_PROMPT}Article title: {title}\nArticle URL: {url}\n\n{}",
        take_chars(content, 12_000)
    );
    let reply = match llm.complete(MODEL, &prompt, 4096) {
        Ok(reply) => reply,
        Err(e) => {
            error!(title, error = %e, "extraction failed");
            return None;
        }
    };
    let mut text = reply.trim().to_owned();
    if is_refusal(&text) {
        warn!(title, url, "model refused to extract items, skipping");
        return None;
    }
    if text.starts_with("```") {
        let Some((_, rest)) = text.split_once('\n') else {
            error!(title, "extraction failed: unterminated code fence");
            return None;
        };
        text = rest
            .rfind("```")
            .map_or(rest, |i| &rest[..i])
            .trim()
            .to_owned();
    }
    let items = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(items)) => items,
        Ok(_) => {
            warn!(title, "expected a JSON array from extraction");
            return None;
        }
        Err(e) => {
            warn!(title, error = %e, "failed to parse JSON from extraction");
            return None;
        }
    };
    let valid: Vec<DigestItem> = items
        .iter()
        .filter_map(Value::as_object)
        .filter(|item| {
            ["title", "who", "what", "why"]
                .iter()
                .all(|k| item.contains_key(*k))
        })
        .map(|item| {
            let field = |k: &str| py_str(&item[k]).trim().to_owned();
            DigestItem {
                title: field("title"),
                who: field("who"),
                what: field("what"),
                why: field("why"),
            }
        })
        .collect();
    if valid.is_empty() {
        warn!(title, "no valid items extracted");
        return None;
    }
    info!(title, items = valid.len(), "extracted digest items");
    Some(valid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_are_spotted_case_insensitively() {
        assert!(is_refusal("Sorry, I'm Unable To open links."));
        assert!(!is_refusal("Rust 1.96 stabilises let chains."));
    }

    #[test]
    fn the_fallback_truncates_at_800_characters() {
        let long = "é".repeat(900);
        let summary = fallback_summary("T", &long);
        assert_eq!(summary.chars().count(), "T. ".len() + 800 + 3);
        assert!(summary.ends_with("..."));
        assert_eq!(fallback_summary("T", "  short  "), "T. short");
    }
}
