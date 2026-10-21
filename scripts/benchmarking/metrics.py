#!/usr/bin/env python3
"""Judge-independent scoring, plus the aggregation the report needs.

mem0's current harness reports a single LLM-judge percentage and nothing else:
no F1, no BLEU, no latency percentiles, and no token counts, despite its
marketing quoting latency and token figures. Their paper did report F1 and
BLEU-1, and dropping them means there is now no signal at all that does not
depend on a model's opinion.

Keeping them costs nothing and is worth it for one specific reason: if our
judge score and our F1 move together, the judge is probably measuring
something real. If they diverge sharply, the judge is doing the work and the
number deserves a caveat. That is a check a reader can apply to our numbers,
and cannot apply to mem0's.

F1 here is deliberately **set based**, matching the definition mem0's paper
inherited from A-Mem: tokens are deduplicated before overlap is computed. A
multiset implementation gives different, generally higher, numbers, so results
would not be comparable with the published literature.
"""

from __future__ import annotations

import math
import re
import statistics
from collections import Counter

_TOKEN = re.compile(r"[a-z0-9]+")
# Stripped before scoring, as is conventional for this family of benchmarks.
_ARTICLES = {"a", "an", "the"}


def tokenize(text: str) -> list[str]:
    """Lowercase, strip punctuation and articles, split on words."""
    return [t for t in _TOKEN.findall((text or "").lower()) if t not in _ARTICLES]


def f1(prediction: str, gold: str) -> float:
    """Set-based token F1 between a prediction and a gold answer."""
    pred, truth = set(tokenize(prediction)), set(tokenize(gold))
    if not pred or not truth:
        return float(pred == truth)
    overlap = len(pred & truth)
    if overlap == 0:
        return 0.0
    precision, recall = overlap / len(pred), overlap / len(truth)
    return 2 * precision * recall / (precision + recall)


def bleu1(prediction: str, gold: str) -> float:
    """Unigram BLEU with a brevity penalty and add-epsilon smoothing.

    Equivalent to nltk's ``sentence_bleu`` with ``weights=(1,0,0,0)`` and
    ``SmoothingFunction().method1``, implemented directly so the harness does
    not need nltk and its corpora just for one number.
    """
    pred, truth = tokenize(prediction), tokenize(gold)
    if not pred or not truth:
        return 0.0
    counts, available = Counter(pred), Counter(truth)
    matched = sum(min(n, available[token]) for token, n in counts.items())
    precision = matched / len(pred) if matched else 0.1 / len(pred)  # method1 epsilon
    brevity = 1.0 if len(pred) > len(truth) else math.exp(1 - len(truth) / len(pred))
    return brevity * precision


def percentile(values: list[float], pct: float) -> float:
    """Nearest-rank percentile. Returns 0.0 for an empty series."""
    if not values:
        return 0.0
    ordered = sorted(values)
    rank = max(1, math.ceil(pct / 100 * len(ordered)))
    return ordered[min(rank, len(ordered)) - 1]


def latency_summary(values: list[float]) -> dict:
    """The percentile spread mem0 publishes for search but never computes."""
    if not values:
        return {"count": 0}
    return {
        "count": len(values),
        "mean": statistics.fmean(values),
        "p50": percentile(values, 50),
        "p95": percentile(values, 95),
        "p99": percentile(values, 99),
        "max": max(values),
    }


def accuracy(labels: list[str]) -> dict:
    """Accuracy over judged answers, counting ERROR separately.

    Errors are excluded from the denominator and reported, rather than being
    folded in as failures. Hiding them would make a flaky judge look like a
    weak memory system.
    """
    correct = sum(1 for label in labels if label == "CORRECT")
    wrong = sum(1 for label in labels if label == "WRONG")
    errors = sum(1 for label in labels if label not in ("CORRECT", "WRONG"))
    scored = correct + wrong
    return {
        "correct": correct,
        "wrong": wrong,
        "errors": errors,
        "scored": scored,
        "accuracy": (correct / scored) if scored else None,
    }


def group_by(rows: list[dict], key: str, label_key: str = "label") -> dict:
    """Accuracy broken down by a field, usually the question category."""
    buckets: dict[str, list[str]] = {}
    for row in rows:
        buckets.setdefault(row.get(key, "unknown"), []).append(row.get(label_key, "ERROR"))
    return {name: accuracy(labels) for name, labels in sorted(buckets.items())}


def token_summary(counts: list[int]) -> dict:
    """Retrieved-context tokens, the figure mem0's paper reports per query."""
    if not counts:
        return {"count": 0}
    return {
        "count": len(counts),
        "mean": statistics.fmean(counts),
        "p50": percentile([float(c) for c in counts], 50),
        "max": max(counts),
        "total": sum(counts),
    }


def estimate_tokens(text: str) -> int:
    """Rough token count without pulling in a tokeniser.

    Only ever used for reporting retrieved-context size, never for billing or
    for any headline claim, and the report says it is an estimate. Four
    characters per token is the usual approximation for English prose.
    """
    return max(1, len(text or "") // 4)
