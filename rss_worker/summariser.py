"""Claude API summarisation for RSS articles."""

import logging
import os

import anthropic

logger = logging.getLogger(__name__)

# Reuse a single Anthropic client across all summarise() calls to avoid
# re-creating the HTTP client (and its connection pool) per article.
_anthropic_client: anthropic.Anthropic | None = None


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


def summarise(title: str, url: str, content_text: str) -> str:
    """Summarise an article using Claude Haiku. Falls back to truncation on error.

    Args:
        title: Article title.
        url: Article URL.
        content_text: Plain text content of the article.

    Returns:
        A 2-3 sentence summary.
    """
    client = _get_client()
    if client is None:
        logger.warning("No valid ANTHROPIC_API_KEY set, falling back to truncation")
        return _fallback_summary(title, content_text)

    try:
        message = client.messages.create(
            model="claude-haiku-4-5-20241022",
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
        logger.debug("Summarised: %s", title)
        return summary
    except Exception as exc:
        # Log only the exception type, not the full message which may contain the API key
        logger.error("Summarisation API error for '%s': %s", title, type(exc).__name__)
        return _fallback_summary(title, content_text)


def _fallback_summary(title: str, content_text: str) -> str:
    """Truncation fallback when API is unavailable."""
    preview = content_text[:300].strip()
    if len(content_text) > 300:
        preview += "..."
    return f"{title}. {preview}"
