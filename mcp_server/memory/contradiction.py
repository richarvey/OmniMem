"""Contradiction detection between memories.

Two-tier approach:
  Tier 1 (keyword heuristic) — fast, runs on every remember() call.
  Tier 2 (Claude API) — deeper analysis, runs on-demand via check_contradictions tool.
"""

import json
import logging
import os
import re
import time
from dataclasses import dataclass
from typing import Any

import numpy as np

from .embedder import Embedder
from .store import ValkeyStore

logger = logging.getLogger(__name__)

# Negation patterns that suggest contradiction when paired with similar content
_NEGATION_PATTERNS = [
    (r"\bdon't\b", r"\bdo\b"),
    (r"\bdo not\b", r"\bdo\b"),
    (r"\bnever\b", r"\balways\b"),
    (r"\bavoid\b", r"\buse\b"),
    (r"\bdon't use\b", r"\buse\b"),
    (r"\bshouldn't\b", r"\bshould\b"),
    (r"\bshould not\b", r"\bshould\b"),
    (r"\bdisable\b", r"\benable\b"),
    (r"\bremove\b", r"\badd\b"),
    (r"\bwithout\b", r"\bwith\b"),
    (r"\bnot recommended\b", r"\brecommended\b"),
    (r"\bdeprecated\b", r"\brecommended\b"),
    (r"\babandoned\b", r"\badopted\b"),
    (r"\bfailed\b", r"\bsucceeded\b"),
    (r"\bwon't work\b", r"\bworks\b"),
    (r"\bdoes not work\b", r"\bworks\b"),
    (r"\bdoesn't work\b", r"\bworks\b"),
]


@dataclass
class ContradictionMatch:
    """A potential contradiction between two memories."""

    key_a: str
    key_b: str
    content_a: str
    content_b: str
    similarity: float
    detection_method: str  # "heuristic" or "api"
    explanation: str


def _has_negation_pair(text_a: str, text_b: str) -> bool:
    """Check if one text contains a negation pattern while the other contains its positive form."""
    a_lower = text_a.lower()
    b_lower = text_b.lower()
    for neg_pattern, pos_pattern in _NEGATION_PATTERNS:
        if (re.search(neg_pattern, a_lower) and re.search(pos_pattern, b_lower)) or \
           (re.search(neg_pattern, b_lower) and re.search(pos_pattern, a_lower)):
            return True
    return False


def check_contradiction_heuristic(
    store: ValkeyStore,
    namespace: str,
    vector: np.ndarray,
    content: str,
    threshold: float | None = None,
    project_filter: str | None = None,
) -> ContradictionMatch | None:
    """Tier 1: Fast keyword-based contradiction check.

    Finds semantically similar memories and checks for negation pattern mismatches.
    Runs on every remember() call — cheap and fast.

    Args:
        store: ValkeyStore instance.
        namespace: Namespace to search.
        vector: Pre-computed embedding vector.
        content: The new content text.
        threshold: Similarity threshold for candidate search (default from env, 0.7).
        project_filter: Optional project scope.

    Returns:
        A ContradictionMatch if a heuristic contradiction is found, else None.
    """
    if threshold is None:
        threshold = float(os.getenv("CONTRADICTION_SIMILARITY_THRESHOLD", "0.7"))

    results = store.search(namespace, vector, top_k=10)
    if not results:
        return None

    for doc in results:
        state = doc.get("state", "active")
        if state in ("archived", "deleted"):
            continue

        if project_filter:
            doc_project = doc.get("project") or doc.get("project_name")
            if doc_project != project_filter:
                continue

        similarity = 1.0 - float(doc.get("similarity_score", "1.0"))
        if similarity < threshold:
            continue

        existing_content = doc.get("content", "")
        if _has_negation_pair(content, existing_content):
            return ContradictionMatch(
                key_a="(new memory)",
                key_b=doc.get("key", ""),
                content_a=content[:200],
                content_b=existing_content[:200],
                similarity=similarity,
                detection_method="heuristic",
                explanation=(
                    "These memories discuss the same topic but contain "
                    "opposing language (negation patterns detected)."
                ),
            )

    return None


def check_contradiction_api(
    content_a: str,
    content_b: str,
) -> dict[str, Any]:
    """Tier 2: Use Claude API for deeper contradiction analysis.

    Only called on-demand via check_contradictions tool — more expensive.

    Args:
        content_a: First memory content.
        content_b: Second memory content.

    Returns:
        Dict with is_contradiction, confidence, and explanation.
    """
    try:
        import anthropic
    except ImportError:
        return {
            "is_contradiction": False,
            "confidence": 0.0,
            "explanation": "Anthropic SDK not available for API-based contradiction check.",
        }

    api_key = os.getenv("ANTHROPIC_API_KEY")
    if not api_key or api_key == "your_key_here":
        return {
            "is_contradiction": False,
            "confidence": 0.0,
            "explanation": "ANTHROPIC_API_KEY not configured for API-based contradiction check.",
        }

    prompt = (
        "You are analysing two memories from a knowledge base for contradictions.\n\n"
        f"Memory A:\n{content_a[:1000]}\n\n"
        f"Memory B:\n{content_b[:1000]}\n\n"
        "Do these two memories contradict each other? Consider:\n"
        "- Direct factual contradictions\n"
        "- Opposing recommendations or advice\n"
        "- Conflicting decisions or approaches\n"
        "- One says to use something the other says to avoid\n\n"
        "Respond in JSON format:\n"
        '{"is_contradiction": true/false, "confidence": 0.0-1.0, "explanation": "brief explanation"}'
    )

    try:
        client = anthropic.Anthropic(api_key=api_key)
        response = client.messages.create(
            model="claude-haiku-4-5-20251001",
            max_tokens=256,
            messages=[{"role": "user", "content": prompt}],
        )
        text = response.content[0].text.strip()
        # Extract JSON from response
        json_match = re.search(r'\{[^}]+\}', text)
        if json_match:
            return json.loads(json_match.group())
        return {"is_contradiction": False, "confidence": 0.0, "explanation": text[:200]}
    except Exception as exc:
        logger.error("Contradiction API check failed: %s", exc)
        return {
            "is_contradiction": False,
            "confidence": 0.0,
            "explanation": f"API check failed: {exc}",
        }


def link_contradiction(
    store: ValkeyStore,
    key_a: str,
    key_b: str,
    explanation: str,
) -> None:
    """Cross-reference two memories as contradicting each other.

    Appends each key to the other's 'contradictions' JSON field.

    Args:
        store: ValkeyStore instance.
        key_a: First memory key.
        key_b: Second memory key.
        explanation: Brief explanation of the contradiction.
    """
    now = str(time.time())
    entry = {
        "explanation": explanation,
        "detected_at": now,
    }

    for src_key, ref_key in [(key_a, key_b), (key_b, key_a)]:
        data = store.get(src_key)
        if data is None:
            continue
        existing_raw = data.get("contradictions", "[]")
        try:
            existing = json.loads(existing_raw)
        except (json.JSONDecodeError, TypeError):
            existing = []

        # Avoid duplicate links
        if any(c.get("key") == ref_key for c in existing if isinstance(c, dict)):
            continue

        existing.append({"key": ref_key, **entry})
        store.set_fields(src_key, {
            "contradictions": json.dumps(existing),
            "updated_at": now,
        })

    logger.info("Linked contradiction between %s and %s", key_a, key_b)
