"""Claude API summarisation for RSS articles."""

import json
import logging
import os
import time

import anthropic

logger = logging.getLogger(__name__)

# Reuse a single Anthropic client across all summarise() calls to avoid
# re-creating the HTTP client (and its connection pool) per article.
_anthropic_client: anthropic.Anthropic | None = None

# Transient errors worth retrying
_RETRYABLE = (
    anthropic.APITimeoutError,
    anthropic.RateLimitError,
    anthropic.InternalServerError,
    anthropic.APIConnectionError,
)

_MAX_RETRIES = 2
_RETRY_DELAY = 3


def _get_client() -> anthropic.Anthropic | None:
    """Return a cached Anthropic client, or None if no valid API key."""
    global _anthropic_client
    if _anthropic_client is not None:
        return _anthropic_client
    api_key = os.getenv("ANTHROPIC_API_KEY", "")
    if not api_key or api_key == "your_key_here":
        return None
    _anthropic_client = anthropic.Anthropic(api_key=api_key)
    return _anthropic_client


def _is_refusal(summary: str) -> bool:
    """Detect when the model refuses to summarise (e.g. can't access URLs)."""
    lower = summary.lower()
    refusal_phrases = [
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
    ]
    return any(phrase in lower for phrase in refusal_phrases)


def summarise(title: str, url: str, content_text: str) -> str | None:
    """Summarise an article using Claude Haiku.

    Returns None if summarisation fails or the model refuses (e.g. cannot
    access the URL).  Falls back to truncation only when the API is
    unavailable or hits transient errors AND valid content was provided.

    Args:
        title: Article title.
        url: Article URL.
        content_text: Plain text content of the article.

    Returns:
        A 2-3 sentence summary, or None if the article could not be
        meaningfully summarised.
    """
    client = _get_client()
    if client is None:
        logger.warning("No valid ANTHROPIC_API_KEY set, falling back to truncation")
        return _fallback_summary(title, content_text)

    last_exc: Exception | None = None
    for attempt in range(1, _MAX_RETRIES + 1):
        try:
            message = client.messages.create(
                model="claude-haiku-4-5-20251001",
                max_tokens=256,
                messages=[
                    {
                        "role": "user",
                        "content": (
                            "Summarise the following article in 2-3 sentences, "
                            "focusing on what a developer might find actionable or useful. "
                            "Include the main technology or concept. Be concise.\n\n"
                            f"Title: {title}\n"
                            f"URL: {url}\n\n"
                            f"{content_text[:3000]}"
                        ),
                    }
                ],
            )
            summary = message.content[0].text.strip()
            if _is_refusal(summary):
                logger.warning(
                    "Model refused to summarise '%s' (%s) — skipping",
                    title, url,
                )
                return None
            logger.debug("Summarised: %s", title)
            return summary
        except _RETRYABLE as exc:
            last_exc = exc
            logger.warning(
                "Summarisation attempt %d/%d for '%s' failed: %s: %s",
                attempt, _MAX_RETRIES, title, type(exc).__name__, exc,
            )
            if attempt < _MAX_RETRIES:
                time.sleep(_RETRY_DELAY * attempt)
        except Exception as exc:
            logger.error(
                "Summarisation failed for '%s': %s: %s", title, type(exc).__name__, exc,
            )
            return _fallback_summary(title, content_text)

    logger.error(
        "Summarisation failed after %d retries for '%s': %s: %s",
        _MAX_RETRIES, title, type(last_exc).__name__, last_exc,
    )
    return _fallback_summary(title, content_text)


def _fallback_summary(title: str, content_text: str) -> str:
    """Truncation fallback when API is unavailable."""
    preview = content_text[:800].strip()
    if len(content_text) > 800:
        preview += "..."
    return f"{title}. {preview}"


_EXTRACT_PROMPT = (
    "You are analysing an article to extract individual news items, "
    "announcements, or updates. The article may cover a single topic "
    "or be a newsletter/digest containing many distinct items.\n\n"
    "For EACH distinct item, project, announcement, or piece of news, extract:\n"
    "- title: A short descriptive title (max 10 words)\n"
    "- who: Who or what is this about? (person, project, organisation, technology)\n"
    "- what: What happened, what does it do, or what was announced?\n"
    "- why: Why does this matter? Why should the reader care?\n\n"
    "Return ONLY a valid JSON array of objects. No markdown fences, no explanation.\n"
    "If the article covers a single topic, return an array with one object.\n"
    "Skip promotional content, sponsor mentions, and calls-to-action.\n"
    "Skip items where you cannot determine meaningful who/what/why.\n\n"
)


def extract_items(
    title: str, url: str, content_text: str,
) -> list[dict[str, str]] | None:
    """Extract individual items from a newsletter or multi-topic article.

    Uses Claude to identify each distinct news item, project, or announcement
    and return structured who/what/why for each.

    Args:
        title: Article title.
        url: Article URL.
        content_text: Plain text content of the article.

    Returns:
        List of dicts with title/who/what/why keys, or None on failure.
    """
    client = _get_client()
    if client is None:
        logger.warning("No valid ANTHROPIC_API_KEY — cannot extract items")
        return None

    last_exc: Exception | None = None
    for attempt in range(1, _MAX_RETRIES + 1):
        try:
            message = client.messages.create(
                model="claude-haiku-4-5-20251001",
                max_tokens=4096,
                messages=[
                    {
                        "role": "user",
                        "content": (
                            f"{_EXTRACT_PROMPT}"
                            f"Article title: {title}\n"
                            f"Article URL: {url}\n\n"
                            f"{content_text[:12000]}"
                        ),
                    }
                ],
            )

            response_text = message.content[0].text.strip()

            if _is_refusal(response_text):
                logger.warning(
                    "Model refused to extract items from '%s' (%s) — skipping",
                    title, url,
                )
                return None

            # Strip markdown fences if the model wrapped the JSON
            if response_text.startswith("```"):
                response_text = response_text.split("\n", 1)[1]
                response_text = response_text.rsplit("```", 1)[0].strip()

            items = json.loads(response_text)
            if not isinstance(items, list):
                logger.warning(
                    "Expected JSON array from extraction of '%s', got %s",
                    title, type(items).__name__,
                )
                return None

            valid: list[dict[str, str]] = []
            required = ("title", "who", "what", "why")
            for item in items:
                if isinstance(item, dict) and all(k in item for k in required):
                    valid.append({k: str(item[k]).strip() for k in required})

            if not valid:
                logger.warning("No valid items extracted from '%s'", title)
                return None

            logger.info("Extracted %d items from '%s'", len(valid), title)
            return valid

        except json.JSONDecodeError as exc:
            logger.warning(
                "Failed to parse JSON from extraction of '%s': %s", title, exc,
            )
            return None
        except _RETRYABLE as exc:
            last_exc = exc
            logger.warning(
                "Extraction attempt %d/%d for '%s' failed: %s: %s",
                attempt, _MAX_RETRIES, title, type(exc).__name__, exc,
            )
            if attempt < _MAX_RETRIES:
                time.sleep(_RETRY_DELAY * attempt)
        except Exception as exc:
            logger.error(
                "Extraction failed for '%s': %s: %s",
                title, type(exc).__name__, exc,
            )
            return None

    logger.error(
        "Extraction failed after %d retries for '%s': %s: %s",
        _MAX_RETRIES, title, type(last_exc).__name__, last_exc,
    )
    return None
