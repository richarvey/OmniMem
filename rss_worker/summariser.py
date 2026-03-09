"""Claude API summarisation for RSS articles."""

import logging
import os

import anthropic

logger = logging.getLogger(__name__)


def summarise(title: str, url: str, content_text: str) -> str:
    """Summarise an article using Claude Haiku. Falls back to truncation on error.

    Args:
        title: Article title.
        url: Article URL.
        content_text: Plain text content of the article.

    Returns:
        A 2-3 sentence summary.
    """
    api_key = os.getenv("ANTHROPIC_API_KEY", "")
    if not api_key or api_key == "your_key_here":
        logger.warning("No valid ANTHROPIC_API_KEY set, falling back to truncation")
        return _fallback_summary(title, content_text)

    try:
        client = anthropic.Anthropic(api_key=api_key)
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
        logger.error("Summarisation API error for '%s': %s", title, exc)
        return _fallback_summary(title, content_text)


def _fallback_summary(title: str, content_text: str) -> str:
    """Truncation fallback when API is unavailable."""
    preview = content_text[:300].strip()
    if len(content_text) > 300:
        preview += "..."
    return f"{title}. {preview}"
