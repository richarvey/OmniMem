"""Chunking strategies for remember_document().

Splits long-form content into smaller pieces so each chunk can be embedded and
retrieved independently. Discovered during the LongMemEval benchmark: storing
whole sessions as single memories collapses recall (45% zero-recall on 500-2000
word sessions). Turn-pair chunking dropped that to 0%.
"""

from __future__ import annotations

import re

VALID_STRATEGIES = {"turn_pairs", "sentences", "paragraphs", "fixed_tokens"}

DEFAULT_FIXED_TOKEN_SIZE = 200
DEFAULT_FIXED_TOKEN_OVERLAP = 0.1  # 10%

# Conversation turn marker: "User:" / "Assistant:" / "System:" at line start.
_TURN_RE = re.compile(r"(?im)^\s*(user|assistant|system)\s*:\s*", re.MULTILINE)

# Sentence boundary: ., !, ? followed by whitespace + capital, with abbreviation guard.
_ABBREVIATIONS = {
    "mr", "mrs", "ms", "dr", "st", "vs", "etc", "e.g", "i.e", "fig", "no",
    "inc", "ltd", "co", "jr", "sr",
}
_SENT_SPLIT_RE = re.compile(r"(?<=[.!?])\s+(?=[A-Z\"'\(])")


def chunk_turn_pairs(content: str) -> list[str]:
    """Split a transcript on User:/Assistant: markers and pair adjacent turns.

    A "pair" is one user turn + the next assistant turn. Trailing solo turns are
    emitted on their own. Markers are preserved in the output so context is
    obvious when the chunk is recalled in isolation.
    """
    if not content.strip():
        return []

    matches = list(_TURN_RE.finditer(content))
    if not matches:
        # No turn markers — fall back to one chunk
        return [content.strip()]

    turns: list[tuple[str, str]] = []
    for i, m in enumerate(matches):
        speaker = m.group(1).lower().capitalize()
        start = m.end()
        end = matches[i + 1].start() if i + 1 < len(matches) else len(content)
        text = content[start:end].strip()
        if text:
            turns.append((speaker, text))

    chunks: list[str] = []
    i = 0
    while i < len(turns):
        speaker, text = turns[i]
        if speaker == "User" and i + 1 < len(turns) and turns[i + 1][0] == "Assistant":
            nxt_speaker, nxt_text = turns[i + 1]
            chunks.append(f"User: {text}\nAssistant: {nxt_text}")
            i += 2
        else:
            chunks.append(f"{speaker}: {text}")
            i += 1
    return chunks


def chunk_sentences(content: str) -> list[str]:
    """Split on sentence boundaries with a basic abbreviation guard."""
    text = content.strip()
    if not text:
        return []

    # First-pass split, then re-merge any pieces that ended on a known abbreviation.
    raw = _SENT_SPLIT_RE.split(text)
    out: list[str] = []
    buffer = ""
    for piece in raw:
        candidate = (buffer + " " + piece).strip() if buffer else piece.strip()
        # Check the trailing token before the final punctuation
        last_word = re.split(r"\s+", candidate.rstrip(".!?"))[-1].lower().rstrip(".")
        if last_word in _ABBREVIATIONS:
            buffer = candidate
            continue
        out.append(candidate)
        buffer = ""
    if buffer:
        out.append(buffer)
    return [s for s in out if s]


def chunk_paragraphs(content: str) -> list[str]:
    """Split on blank lines (one or more)."""
    parts = re.split(r"\n\s*\n+", content.strip())
    return [p.strip() for p in parts if p.strip()]


def chunk_fixed_tokens(
    content: str,
    chunk_size: int | None = None,
    overlap_ratio: float = DEFAULT_FIXED_TOKEN_OVERLAP,
) -> list[str]:
    """Approximate token chunking via word-count windows with overlap.

    We deliberately avoid pulling in a real tokeniser — word count is close
    enough at this granularity and keeps the dep tree clean.
    """
    size = chunk_size or DEFAULT_FIXED_TOKEN_SIZE
    if size < 1:
        raise ValueError("chunk_size must be >= 1")
    words = content.split()
    if not words:
        return []
    overlap = max(0, int(size * overlap_ratio))
    step = max(1, size - overlap)
    chunks: list[str] = []
    i = 0
    while i < len(words):
        chunk = " ".join(words[i : i + size])
        if chunk:
            chunks.append(chunk)
        if i + size >= len(words):
            break
        i += step
    return chunks


def chunk(
    content: str,
    strategy: str,
    chunk_size: int | None = None,
) -> list[str]:
    """Dispatch to the requested chunking strategy."""
    if strategy not in VALID_STRATEGIES:
        raise ValueError(
            f"Invalid chunk_strategy '{strategy}'. "
            f"Must be one of: {', '.join(sorted(VALID_STRATEGIES))}"
        )
    if strategy == "turn_pairs":
        return chunk_turn_pairs(content)
    if strategy == "sentences":
        return chunk_sentences(content)
    if strategy == "paragraphs":
        return chunk_paragraphs(content)
    return chunk_fixed_tokens(content, chunk_size=chunk_size)
