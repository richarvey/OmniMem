"""Fact extraction at ingest — turns raw text into discrete declarative facts.

When ingest mode is "full", remember() and remember_document() route content
through this module first. Each extracted fact is then stored as its own
memory; preference-shaped facts are routed to the preference namespace.

Designed to fail open: if extraction errors out or no API key is set, the
caller falls back to raw storage so we never silently lose data.
"""

from __future__ import annotations

import json
import logging
import os
from dataclasses import dataclass
from datetime import datetime

logger = logging.getLogger(__name__)

_anthropic_client = None
_client_init_attempted = False

_PROMPT = (
    "Extract discrete, atomic facts from the following text. Each fact should "
    "be a single declarative statement that stands on its own when read in "
    "isolation. Aim for facts that would survive being indexed and recalled "
    "later by a question-answering system.\n\n"
    "For each fact, identify:\n"
    "- text: the standalone declarative sentence\n"
    "- kind: one of 'fact' (general factual statement) or 'preference' "
    "(prescriptive rule about how someone wants to work, e.g. 'I prefer X', "
    "'always do Y', 'never do Z')\n"
    "- event_date: if the fact references a specific date or relative time "
    "('last March', '2026-03-15', 'yesterday'), provide it as ISO 8601 "
    "(YYYY-MM-DD). Omit this field if no date is mentioned.\n\n"
    "Return ONLY a JSON array of objects. No markdown fences, no explanation. "
    "If the text contains no extractable facts, return an empty array.\n"
    "Skip pleasantries, hedging, and meta-commentary about the conversation.\n\n"
    "Text:\n{content}\n"
)


@dataclass
class ExtractedFact:
    text: str
    kind: str  # "fact" | "preference"
    event_date: float | None = None  # Unix timestamp


def _get_client():
    global _anthropic_client, _client_init_attempted
    if _client_init_attempted:
        return _anthropic_client
    _client_init_attempted = True

    api_key = os.getenv("ANTHROPIC_API_KEY", "").strip()
    if not api_key or api_key == "your_key_here":
        logger.debug("Fact extraction disabled: no ANTHROPIC_API_KEY")
        return None
    try:
        import anthropic

        _anthropic_client = anthropic.Anthropic(api_key=api_key)
    except Exception as exc:
        logger.warning("Failed to initialise Anthropic client for fact extraction: %s", exc)
        _anthropic_client = None
    return _anthropic_client


def _parse_event_date(raw) -> float | None:
    if not raw:
        return None
    try:
        # ISO 8601 — date only or datetime
        if isinstance(raw, (int, float)):
            return float(raw)
        text = str(raw).strip()
        if not text:
            return None
        try:
            dt = datetime.fromisoformat(text)
        except ValueError:
            # Try date-only fallback
            dt = datetime.strptime(text[:10], "%Y-%m-%d")
        return dt.timestamp()
    except Exception:
        return None


def extract_facts(content: str) -> list[ExtractedFact]:
    """Extract discrete facts from content via Claude Haiku.

    Returns an empty list on any failure — caller should fall back to raw
    storage in that case.
    """
    if not content or not content.strip():
        return []

    client = _get_client()
    if client is None:
        return []

    try:
        message = client.messages.create(
            model=os.getenv("FACT_EXTRACTION_MODEL", "claude-haiku-4-5-20251001"),
            max_tokens=2048,
            messages=[
                {
                    "role": "user",
                    "content": _PROMPT.format(content=content[:12000]),
                }
            ],
        )
        text = message.content[0].text.strip()
        if text.startswith("```"):
            text = text.split("\n", 1)[1]
            text = text.rsplit("```", 1)[0].strip()
        items = json.loads(text)
    except Exception as exc:
        logger.warning("Fact extraction failed: %s", exc)
        return []

    if not isinstance(items, list):
        return []

    out: list[ExtractedFact] = []
    for item in items:
        if not isinstance(item, dict):
            continue
        text_val = str(item.get("text", "")).strip()
        if not text_val:
            continue
        kind = str(item.get("kind", "fact")).strip().lower()
        if kind not in ("fact", "preference"):
            kind = "fact"
        event_date = _parse_event_date(item.get("event_date"))
        out.append(ExtractedFact(text=text_val, kind=kind, event_date=event_date))
    return out


def reset_client_for_tests() -> None:
    global _anthropic_client, _client_init_attempted
    _anthropic_client = None
    _client_init_attempted = False
