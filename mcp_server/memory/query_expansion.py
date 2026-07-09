"""Optional Claude-powered query expansion for recall().

Improves recall coverage when the query phrasing doesn't share vocabulary
with the stored content. Generates N alternative phrasings via Claude Haiku,
caches them in Valkey, and lets the recall pipeline run all variants and
union the results.

Discovered during the LongMemEval benchmark — single-query recall fails on
questions like "What degree did I graduate with?" against stored content
"I studied Business Administration at university" because the surface
words don't overlap.
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
import time
from typing import TYPE_CHECKING

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from .store import ValkeyStore

# Cache TTL for expanded variants — 24h is plenty given query expansions are
# usually similar across sessions for the same project.
_CACHE_TTL = 86400

_anthropic_client = None
_client_init_attempted = False

_PROMPT_TEMPLATE = (
    "Generate {n} alternative phrasings of the following search query to improve "
    "semantic search recall. Each variant should preserve the original meaning but "
    "use different vocabulary, synonyms, or related concepts that might appear in "
    "stored content.\n\n"
    "Return ONLY a JSON array of {n} strings. No markdown fences, no explanation.\n\n"
    "Original query: {query}\n"
)


def _get_client():
    """Return a cached Anthropic client, or None if no valid API key."""
    global _anthropic_client, _client_init_attempted
    if _client_init_attempted:
        return _anthropic_client
    _client_init_attempted = True

    api_key = os.getenv("ANTHROPIC_API_KEY", "").strip()
    if not api_key or api_key == "your_key_here":
        logger.debug("Query expansion disabled: no ANTHROPIC_API_KEY")
        return None
    try:
        import anthropic

        _anthropic_client = anthropic.Anthropic(api_key=api_key)
    except Exception as exc:
        logger.warning("Failed to initialise Anthropic client for query expansion: %s", exc)
        _anthropic_client = None
    return _anthropic_client


def _cache_key(query: str, n: int) -> str:
    digest = hashlib.sha1(f"{n}|{query}".encode("utf-8"), usedforsecurity=False).hexdigest()
    return f"qexp:{digest}"


def _read_cache(store: "ValkeyStore", key: str) -> list[str] | None:
    try:
        data = store.client.hgetall(key)
    except Exception:
        return None
    if not data:
        return None
    raw = data.get(b"variants") if isinstance(next(iter(data)), bytes) else data.get("variants")
    if not raw:
        return None
    if isinstance(raw, bytes):
        raw = raw.decode("utf-8")
    try:
        variants = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return None
    if isinstance(variants, list):
        return [str(v) for v in variants if v]
    return None


def _write_cache(store: "ValkeyStore", key: str, variants: list[str]) -> None:
    try:
        pipe = store.client.pipeline(transaction=False)
        pipe.hset(key, mapping={"variants": json.dumps(variants), "ts": str(time.time())})
        pipe.expire(key, _CACHE_TTL)
        pipe.execute()
    except Exception as exc:
        logger.debug("Failed to cache query expansion: %s", exc)


def expand_query(
    query: str,
    n: int | None = None,
    store: "ValkeyStore | None" = None,
) -> list[str]:
    """Generate N alternative phrasings of `query`.

    Returns a list of variant strings (NOT including the original). Returns
    an empty list on any failure — callers should fall back to the original
    query alone in that case.
    """
    if not query or not query.strip():
        return []

    if n is None:
        n = int(os.getenv("RECALL_EXPAND_COUNT", "3"))
    n = max(1, min(n, 10))

    if store is not None:
        cached = _read_cache(store, _cache_key(query, n))
        if cached is not None:
            logger.debug("Query expansion cache hit (%d variants)", len(cached))
            return cached

    client = _get_client()
    if client is None:
        return []

    try:
        message = client.messages.create(
            model=os.getenv("QUERY_EXPANSION_MODEL", "claude-haiku-4-5-20251001"),
            max_tokens=512,
            messages=[
                {
                    "role": "user",
                    "content": _PROMPT_TEMPLATE.format(n=n, query=query),
                }
            ],
        )
        text = message.content[0].text.strip()
        if text.startswith("```"):
            text = text.split("\n", 1)[1]
            text = text.rsplit("```", 1)[0].strip()
        variants = json.loads(text)
    except Exception as exc:
        logger.warning("Query expansion failed for %r: %s", query[:80], exc)
        return []

    if not isinstance(variants, list):
        return []
    cleaned = [str(v).strip() for v in variants if isinstance(v, (str, int, float)) and str(v).strip()]
    cleaned = cleaned[:n]

    if store is not None and cleaned:
        _write_cache(store, _cache_key(query, n), cleaned)

    return cleaned


def reset_client_for_tests() -> None:
    """Test hook — reset the cached client between tests."""
    global _anthropic_client, _client_init_attempted
    _anthropic_client = None
    _client_init_attempted = False
