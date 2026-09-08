#!/usr/bin/env python3
"""Ingest and recall benchmark for the embedding backend.

Built for the sentence-transformers → ONNX Runtime swap (v6.7): run it once
before the swap and once after, then compare. The harness uses whatever
``memory.embedder.Embedder`` is wired to, so the script itself never
changes across the swap — only the numbers do.

    # before the swap
    VALKEY_HOST=127.0.0.1 VALKEY_PORT=6399 VALKEY_PASSWORD=testpw \\
        python scripts/embedding_bench.py run --out bench/before.json

    # after the swap
    ... python scripts/embedding_bench.py run --out bench/after.json

    python scripts/embedding_bench.py compare bench/before.json bench/after.json

What it measures:

  speed        model cold-load time and peak RSS; embed() latency (p50/p95);
               embed_batch() throughput at several batch sizes; remember()
               end-to-end; remember_document(); RecallPipeline.recall() and
               recall_index() latency over a fixed query set.
  equivalence  the raw vector for a sample of texts, and the top-k keys and
               scores for every query, saved into the report. ``compare``
               turns those into per-text cosine similarity between the two
               backends, top-k overlap, and score drift — which is what
               tells you whether an ONNX export is faithful, not just fast.

The corpus is either a real backup dump (``--corpus backups/x.json``, the
``content`` of every memory) or a seeded synthetic corpus (default), so a
run is reproducible. Queries are derived from the corpus so recall has
something to find. Everything is written under one project name and
deleted afterwards, and the run refuses a store that already holds
memories unless told otherwise — point it at a throwaway valkey-search,
never at production.

Runs from the repo root or from mcp_server/ (the package path is fixed up
below). Needs a real valkey-search and the real embedding model; the test
suite exercises the same functions against the in-memory fakes.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import platform
import random
import resource
import statistics
import sys
import time
from pathlib import Path
from typing import Any, Callable

_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_ROOT / "mcp_server"))

import numpy as np  # noqa: E402

BENCH_PROJECT = "embedding-bench"
DEFAULT_SIZE = 400
DEFAULT_QUERIES = 40
DEFAULT_BATCH_SIZES = (1, 8, 32)
DEFAULT_VECTOR_SAMPLE = 48
TOP_K = 10
REPORT_VERSION = 1

# ---------------------------------------------------------------------------
# Corpus
# ---------------------------------------------------------------------------

_TOPICS = [
    ("python", ["packaging with uv", "asyncio cancellation", "pydantic v2 validators",
                "pytest fixtures", "type hints for generics", "dataclass slots"]),
    ("docker", ["multi-arch buildx", "layer caching", "compose healthchecks",
                "bind mount permissions", "distroless images", "buildkit secrets"]),
    ("valkey", ["HNSW vector index", "FT.SEARCH tag filters", "AOF persistence",
                "keyspace notifications", "pipeline batching", "connection pooling"]),
    ("css", ["table-layout fixed", "grid minmax", "focus-visible outlines",
             "prefers-color-scheme", "container queries", "logical properties"]),
    ("rust", ["borrow checker lifetimes", "tokio runtime", "serde derive",
              "cargo workspaces", "error handling with thiserror", "async traits"]),
    ("terraform", ["state locking", "module versioning", "workspaces",
                   "provider aliases", "for_each vs count", "import blocks"]),
    ("security", ["OAuth refresh rotation", "CSP headers", "secret scanning",
                  "rate limiting per IP", "constant-time compare", "SBOM generation"]),
    ("writing", ["British spelling", "no marketing fluff", "concrete numbers",
                 "changelog entries", "README as front door", "release notes"]),
]

_OUTCOMES = [
    "worked first time",
    "took three attempts before {fix}",
    "abandoned after {fix} failed twice",
    "fixed by {fix}",
    "pivoted to {fix} instead",
]

_FIXES = [
    "pinning the version", "reading the actual docs", "dropping the cache",
    "adding a retry with backoff", "moving it into a pipeline", "splitting the module",
    "testing against a real server", "rebalancing the column widths",
]

_SHAPES = [
    "Decision: for {project} we use {detail} ({topic}). Reason: {outcome}.",
    "Bug fix in {project}: symptom was {detail} misbehaving; cause was a wrong assumption "
    "about {topic}; {outcome}.",
    "Pattern learned on {project}: {detail} only holds when you {fix}. Tagged {topic}.",
    "Gotcha ({topic}): {detail} looks fine locally but breaks in CI unless you {fix}.",
    "{project} status: {detail} is done, next up is {topic} hardening; {outcome}.",
    "Preference: always prefer {detail} over the alternative for {topic} work in {project}.",
]

_PROJECTS = ["omnimem", "moo-reports", "litterwatch", "opendesk", "sqcows-infra"]


def synthetic_corpus(size: int, seed: int = 7) -> list[dict[str, Any]]:
    """A reproducible corpus of memory-shaped texts across four namespaces."""
    rng = random.Random(seed)
    corpus: list[dict[str, Any]] = []
    for i in range(size):
        topic, details = rng.choice(_TOPICS)
        detail = rng.choice(details)
        fix = rng.choice(_FIXES)
        outcome = rng.choice(_OUTCOMES).format(fix=fix)
        shape = rng.choice(_SHAPES)
        content = shape.format(
            project=rng.choice(_PROJECTS), detail=detail, topic=topic,
            outcome=outcome, fix=fix,
        )
        if shape.startswith("Preference"):
            namespace = "preference"
        elif shape.startswith("Gotcha") or shape.startswith("Pattern"):
            namespace = "knowledge" if i % 3 == 0 else "episodic"
        else:
            namespace = "episodic"
        corpus.append({
            "content": content,
            "namespace": namespace,
            "tags": [topic, "bench"],
        })
    return corpus


def corpus_from_dump(path: str | Path, size: int | None = None) -> list[dict[str, Any]]:
    """Memory contents from a dump_to_file backup, oldest key first.

    Only the text travels: every record is re-written under the bench
    project in its original namespace, so the store being benchmarked is
    never touched.
    """
    with open(path, encoding="utf-8") as fh:
        dump = json.load(fh)
    data = dump.get("data", dump)
    corpus: list[dict[str, Any]] = []
    for key in sorted(data):
        if not key.startswith(("mem:episodic:", "mem:knowledge:", "mem:preference:")):
            continue
        fields = data[key] or {}
        content = fields.get("content")
        if not isinstance(content, str) or not content.strip():
            continue
        try:
            tags = json.loads(fields.get("tags") or "[]")
        except (json.JSONDecodeError, TypeError):
            tags = []
        corpus.append({
            "content": content[:4000],
            "namespace": key.split(":")[1],
            "tags": [t for t in tags if isinstance(t, str)][:5] + ["bench"],
        })
    if size is not None:
        corpus = corpus[:size]
    return corpus


def derive_queries(corpus: list[dict[str, Any]], n: int, seed: int = 11) -> list[str]:
    """``n`` queries with known answers — a short fragment of a corpus text
    each — plus four generic ones."""
    rng = random.Random(seed)
    picks = rng.sample(corpus, min(n, len(corpus))) if corpus else []
    queries: list[str] = []
    for item in picks:
        words = item["content"].split()
        if len(words) <= 8:
            queries.append(item["content"])
            continue
        start = rng.randrange(0, max(1, len(words) - 8))
        queries.append(" ".join(words[start:start + 8]))
    # ...plus a handful of generic ones nothing matches exactly, so the
    # ranking on a weak signal is measured too.
    queries += [
        "why did the build fail on arm64",
        "what did we decide about caching",
        "how do we handle authentication tokens",
        "which approach was abandoned and why",
    ]
    return queries


# ---------------------------------------------------------------------------
# Measurement helpers
# ---------------------------------------------------------------------------


def _percentiles(samples_ms: list[float]) -> dict[str, float]:
    if not samples_ms:
        return {"n": 0, "p50": 0.0, "p95": 0.0, "mean": 0.0, "min": 0.0, "max": 0.0}
    ordered = sorted(samples_ms)

    def pct(p: float) -> float:
        k = max(0, min(len(ordered) - 1, math.ceil(p / 100 * len(ordered)) - 1))
        return ordered[k]

    return {
        "n": len(ordered),
        "p50": round(pct(50), 3),
        "p95": round(pct(95), 3),
        "mean": round(statistics.fmean(ordered), 3),
        "min": round(ordered[0], 3),
        "max": round(ordered[-1], 3),
    }


def _timed(fn: Callable[[], Any]) -> tuple[Any, float]:
    start = time.perf_counter()
    result = fn()
    return result, (time.perf_counter() - start) * 1000.0


def _peak_rss_mb() -> float:
    """Peak resident set size of this process in MB (Linux reports KB)."""
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return round(rss / 1024.0, 1) if platform.system() == "Linux" else round(rss / (1024.0 * 1024.0), 1)


def _backend_info(embedder: Any) -> dict[str, Any]:
    """Which libraries are behind the embedder, for the report header."""
    info: dict[str, Any] = {
        "embedder_class": type(embedder).__name__,
        "model": os.getenv("EMBEDDING_MODEL", "all-MiniLM-L6-v2"),
        "python": platform.python_version(),
        "platform": platform.platform(),
        "machine": platform.machine(),
    }
    for module_name in ("sentence_transformers", "torch", "onnxruntime", "optimum", "tokenizers"):
        try:
            module = __import__(module_name)
            info[module_name] = getattr(module, "__version__", "present")
        except ImportError:
            info[module_name] = None
    return info


# ---------------------------------------------------------------------------
# The benchmark
# ---------------------------------------------------------------------------


def run_benchmark(
    store: Any,
    embedder: Any,
    corpus: list[dict[str, Any]],
    queries: list[str],
    *,
    batch_sizes: tuple[int, ...] = DEFAULT_BATCH_SIZES,
    vector_sample: int = DEFAULT_VECTOR_SAMPLE,
    top_k: int = TOP_K,
    load_ms: float | None = None,
    label: str = "",
) -> dict[str, Any]:
    """Ingest the corpus, query it, and return a report dict.

    ``store`` and ``embedder`` are injected so the suite can run this
    against the in-memory fakes; the CLI wires the real ones. ``load_ms``
    is the measured model cold-load time when the caller has it.
    """
    from memory.lifecycle import MemoryLifecycle
    from memory.recall import RecallPipeline
    from tools import core as core_tools
    import tools as tools_pkg

    lifecycle = MemoryLifecycle(store)
    pipeline = RecallPipeline(store, embedder, lifecycle)
    tools_pkg._store = store
    tools_pkg._embedder = embedder
    tools_pkg._lifecycle = lifecycle
    tools_pkg._pipeline = pipeline

    texts = [item["content"] for item in corpus]
    # Recall logs its events; snapshot them so only this run's are removed.
    recall_logs_before = set(store.scan_prefix("log:recall:"))
    report: dict[str, Any] = {
        "report_version": REPORT_VERSION,
        "label": label,
        "generated_at": time.time(),
        "backend": _backend_info(embedder),
        "corpus": {"size": len(corpus), "queries": len(queries), "top_k": top_k},
        "speed": {},
        "equivalence": {},
    }
    if load_ms is not None:
        report["speed"]["model_load_ms"] = round(load_ms, 1)

    # --- embedding: single-text latency (warm-up first so the first-call
    # cost — lazy init, kernel selection — doesn't masquerade as p95)
    warm = texts[:4] or ["warm up"]
    for text in warm:
        embedder.embed(text)
    single: list[float] = []
    for text in texts[:min(len(texts), 200)]:
        _, ms = _timed(lambda t=text: embedder.embed(t))
        single.append(ms)
    report["speed"]["embed_single_ms"] = _percentiles(single)

    # --- embedding: batch throughput
    batch_stats: dict[str, Any] = {}
    for size in batch_sizes:
        if size > len(texts):
            continue
        batches = [texts[i:i + size] for i in range(0, len(texts), size)]
        batches = batches[:max(1, min(len(batches), 400 // size))]
        _, ms = _timed(lambda b=batches: [embedder.embed_batch(batch) for batch in b])
        embedded = sum(len(b) for b in batches)
        batch_stats[str(size)] = {
            "texts": embedded,
            "total_ms": round(ms, 1),
            "texts_per_s": round(embedded / (ms / 1000.0), 1) if ms else 0.0,
        }
    report["speed"]["embed_batch"] = batch_stats

    # --- ingest: remember() end-to-end, raw mode (no Haiku), dedup on so
    # the measured path is the real one. Duplicates in a synthetic corpus
    # are skipped by the tool and counted here.
    remember_ms: list[float] = []
    duplicates = 0
    keys: list[str] = []
    for item in corpus:
        result, ms = _timed(lambda i=item: core_tools.remember(
            i["content"], project=BENCH_PROJECT, tags=i["tags"],
            namespace=i["namespace"], mode="raw",
        ))
        remember_ms.append(ms)
        if result.get("status") == "duplicate_found":
            duplicates += 1
        else:
            keys.append(result["key"])
    report["speed"]["remember_ms"] = _percentiles(remember_ms)
    report["speed"]["remember_duplicates_skipped"] = duplicates

    # --- ingest: one long document through the chunker
    document = "\n\n".join(texts[:min(len(texts), 40)])
    doc_result, doc_ms = _timed(lambda: core_tools.remember_document(
        document, chunk_strategy="paragraphs", project=BENCH_PROJECT,
        tags=["bench", "document"], mode="raw",
    ))
    keys.extend(doc_result.get("keys", []))
    report["speed"]["remember_document"] = {
        "chunks_stored": doc_result.get("chunks_stored", 0),
        "total_ms": round(doc_ms, 1),
        "ms_per_chunk": round(doc_ms / doc_result["chunks_stored"], 3)
        if doc_result.get("chunks_stored") else 0.0,
    }

    # --- recall: pipeline latency and the ranked answers
    for query in queries[:3]:
        pipeline.recall(query, top_k=top_k, project_filter=BENCH_PROJECT, expand_queries=False)
    recall_ms: list[float] = []
    top: dict[str, list[dict[str, Any]]] = {}
    for query in queries:
        results, ms = _timed(lambda q=query: pipeline.recall(
            q, top_k=top_k, project_filter=BENCH_PROJECT, expand_queries=False,
        ))
        recall_ms.append(ms)
        top[query] = [
            {"key": r.key, "score": round(float(r.adjusted_score), 6),
             "content": r.content[:80]}
            for r in results if r.result_type != "abandoned_warning"
        ]
    report["speed"]["recall_ms"] = _percentiles(recall_ms)

    index_ms: list[float] = []
    for query in queries:
        _, ms = _timed(lambda q=query: core_tools.recall_index(
            q, top_k=top_k, project_filter=BENCH_PROJECT, expand_queries=False,
        ))
        index_ms.append(ms)
    report["speed"]["recall_index_ms"] = _percentiles(index_ms)

    # --- equivalence material: vectors for a fixed sample, top-k per query
    sample = texts[:min(vector_sample, len(texts))]
    vectors = embedder.embed_batch(sample) if sample else []
    report["equivalence"]["vector_sample"] = [
        {"text": text, "vector": [round(float(x), 7) for x in vec]}
        for text, vec in zip(sample, vectors)
    ]
    report["equivalence"]["vector_dim"] = int(len(vectors[0])) if len(vectors) else 0
    report["equivalence"]["top_k"] = top

    report["speed"]["peak_rss_mb"] = _peak_rss_mb()

    # --- clean up: only what this run wrote (memories and recall logs)
    bench_keys = [k for k in keys if k.startswith("mem:")]
    bench_keys += [k for k in store.scan_prefix("log:recall:") if k not in recall_logs_before]
    if bench_keys:
        store.delete_many(bench_keys)
    report["cleanup"] = {"deleted": len(bench_keys)}
    return report


# ---------------------------------------------------------------------------
# Comparison
# ---------------------------------------------------------------------------


def _cosine(a: list[float], b: list[float]) -> float:
    va = np.asarray(a, dtype=np.float64)
    vb = np.asarray(b, dtype=np.float64)
    denom = float(np.linalg.norm(va) * np.linalg.norm(vb))
    return float(np.dot(va, vb) / denom) if denom else 0.0


def _content_key(entry: dict[str, Any]) -> str:
    """Match results across runs by content, not key — every run writes
    fresh ULIDs."""
    return entry.get("content", "")


def compare(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    """Speed deltas and equivalence statistics between two reports."""
    out: dict[str, Any] = {
        "before": {"label": before.get("label", ""), "backend": before.get("backend", {})},
        "after": {"label": after.get("label", ""), "backend": after.get("backend", {})},
        "speed": {},
        "equivalence": {},
        "warnings": [],
    }

    def delta(path: tuple[str, ...]) -> dict[str, Any] | None:
        b: Any = before.get("speed", {})
        a: Any = after.get("speed", {})
        for part in path:
            b = b.get(part) if isinstance(b, dict) else None
            a = a.get(part) if isinstance(a, dict) else None
        if not isinstance(b, (int, float)) or not isinstance(a, (int, float)):
            return None
        change = ((a - b) / b * 100.0) if b else 0.0
        return {"before": b, "after": a, "change_pct": round(change, 1)}

    speed_paths = {
        "model_load_ms": ("model_load_ms",),
        "peak_rss_mb": ("peak_rss_mb",),
        "embed_single_p50_ms": ("embed_single_ms", "p50"),
        "embed_single_p95_ms": ("embed_single_ms", "p95"),
        "remember_p50_ms": ("remember_ms", "p50"),
        "remember_p95_ms": ("remember_ms", "p95"),
        "remember_document_ms_per_chunk": ("remember_document", "ms_per_chunk"),
        "recall_p50_ms": ("recall_ms", "p50"),
        "recall_p95_ms": ("recall_ms", "p95"),
        "recall_index_p50_ms": ("recall_index_ms", "p50"),
    }
    for name, path in speed_paths.items():
        d = delta(path)
        if d is not None:
            out["speed"][name] = d
    for size in sorted(
        set(before.get("speed", {}).get("embed_batch", {})) &
        set(after.get("speed", {}).get("embed_batch", {})),
        key=int,
    ):
        d = delta(("embed_batch", size, "texts_per_s"))
        if d is not None:
            out["speed"][f"embed_batch_{size}_texts_per_s"] = d

    # --- vectors: same text, both backends
    b_vecs = {e["text"]: e["vector"] for e in before.get("equivalence", {}).get("vector_sample", [])}
    a_vecs = {e["text"]: e["vector"] for e in after.get("equivalence", {}).get("vector_sample", [])}
    shared = [t for t in b_vecs if t in a_vecs]
    if before.get("equivalence", {}).get("vector_dim") != after.get("equivalence", {}).get("vector_dim"):
        out["warnings"].append(
            f"vector dimension changed: {before.get('equivalence', {}).get('vector_dim')} → "
            f"{after.get('equivalence', {}).get('vector_dim')} — stored vectors must be re-embedded"
        )
    cosines = [_cosine(b_vecs[t], a_vecs[t]) for t in shared
               if len(b_vecs[t]) == len(a_vecs[t]) and b_vecs[t] and a_vecs[t]]
    if cosines:
        out["equivalence"]["vector_cosine"] = {
            "n": len(cosines),
            "mean": round(statistics.fmean(cosines), 6),
            "min": round(min(cosines), 6),
            "below_0_999": sum(1 for c in cosines if c < 0.999),
            "below_0_99": sum(1 for c in cosines if c < 0.99),
        }
        if min(cosines) < 0.99:
            out["warnings"].append(
                "at least one sampled text embeds differently (cosine < 0.99): the "
                "backends are not equivalent — check tokeniser, pooling and normalisation"
            )

    # --- top-k: same query, both backends, matched by content
    b_top = before.get("equivalence", {}).get("top_k", {})
    a_top = after.get("equivalence", {}).get("top_k", {})
    overlaps: list[float] = []
    top1_same = 0
    score_deltas: list[float] = []
    shared_queries = [q for q in b_top if q in a_top]
    for query in shared_queries:
        b_list = b_top[query]
        a_list = a_top[query]
        b_set = {_content_key(e) for e in b_list}
        a_set = {_content_key(e) for e in a_list}
        union = b_set | a_set
        overlaps.append(len(b_set & a_set) / len(union) if union else 1.0)
        if b_list and a_list and _content_key(b_list[0]) == _content_key(a_list[0]):
            top1_same += 1
        b_scores = {_content_key(e): e["score"] for e in b_list}
        for entry in a_list:
            ck = _content_key(entry)
            if ck in b_scores:
                score_deltas.append(abs(entry["score"] - b_scores[ck]))
    if shared_queries:
        out["equivalence"]["top_k"] = {
            "queries": len(shared_queries),
            "mean_jaccard": round(statistics.fmean(overlaps), 4),
            "min_jaccard": round(min(overlaps), 4),
            "top1_identical": top1_same,
            "top1_identical_pct": round(top1_same / len(shared_queries) * 100.0, 1),
            "score_abs_delta_mean": round(statistics.fmean(score_deltas), 6) if score_deltas else 0.0,
            "score_abs_delta_max": round(max(score_deltas), 6) if score_deltas else 0.0,
        }
        if statistics.fmean(overlaps) < 0.8:
            out["warnings"].append(
                "mean top-k overlap below 0.8: recall is returning materially different "
                "results on the same corpus"
            )
    return out


def format_comparison(cmp: dict[str, Any]) -> str:
    """Markdown summary of a comparison."""
    lines = ["# Embedding benchmark: before vs after", ""]
    b_backend = cmp["before"].get("backend", {})
    a_backend = cmp["after"].get("backend", {})
    lines.append(
        f"Before: `{cmp['before'].get('label') or 'before'}` "
        f"(sentence-transformers {b_backend.get('sentence_transformers')}, "
        f"torch {b_backend.get('torch')}, onnxruntime {b_backend.get('onnxruntime')})"
    )
    lines.append(
        f"After: `{cmp['after'].get('label') or 'after'}` "
        f"(sentence-transformers {a_backend.get('sentence_transformers')}, "
        f"torch {a_backend.get('torch')}, onnxruntime {a_backend.get('onnxruntime')})"
    )
    lines += ["", "## Speed", "", "| Metric | Before | After | Change |", "|---|---:|---:|---:|"]
    for name, d in cmp["speed"].items():
        better = d["change_pct"] < 0
        if name.endswith("texts_per_s"):
            better = d["change_pct"] > 0
        arrow = "better" if better else ("same" if d["change_pct"] == 0 else "worse")
        lines.append(f"| {name} | {d['before']} | {d['after']} | {d['change_pct']:+.1f}% ({arrow}) |")
    lines += ["", "## Equivalence", ""]
    vc = cmp["equivalence"].get("vector_cosine")
    if vc:
        lines.append(
            f"- Same text, both backends: cosine mean **{vc['mean']}**, min **{vc['min']}** "
            f"over {vc['n']} texts; {vc['below_0_999']} below 0.999, {vc['below_0_99']} below 0.99"
        )
    tk = cmp["equivalence"].get("top_k")
    if tk:
        lines.append(
            f"- Top-{TOP_K} overlap over {tk['queries']} queries: mean Jaccard **{tk['mean_jaccard']}** "
            f"(min {tk['min_jaccard']}); top-1 identical for {tk['top1_identical']} "
            f"({tk['top1_identical_pct']}%); score drift mean {tk['score_abs_delta_mean']}, "
            f"max {tk['score_abs_delta_max']}"
        )
    if not vc and not tk:
        lines.append("- No shared equivalence material between the two reports.")
    lines += ["", "## Warnings", ""]
    lines += [f"- {w}" for w in cmp["warnings"]] or ["- none"]
    return "\n".join(lines) + "\n"


def format_report(report: dict[str, Any]) -> str:
    """Markdown summary of a single run."""
    s = report["speed"]
    lines = [f"# Embedding benchmark run: {report.get('label') or 'unlabelled'}", ""]
    b = report["backend"]
    lines.append(
        f"Backend: {b.get('embedder_class')} / {b.get('model')} — sentence-transformers "
        f"{b.get('sentence_transformers')}, torch {b.get('torch')}, onnxruntime {b.get('onnxruntime')} "
        f"on {b.get('machine')}"
    )
    lines.append(f"Corpus: {report['corpus']['size']} memories, {report['corpus']['queries']} queries")
    lines += ["", "| Metric | Value |", "|---|---:|"]
    if "model_load_ms" in s:
        lines.append(f"| model load | {s['model_load_ms']} ms |")
    lines.append(f"| peak RSS | {s['peak_rss_mb']} MB |")
    lines.append(f"| embed() p50 / p95 | {s['embed_single_ms']['p50']} / {s['embed_single_ms']['p95']} ms |")
    for size, st in s["embed_batch"].items():
        lines.append(f"| embed_batch({size}) | {st['texts_per_s']} texts/s |")
    lines.append(f"| remember() p50 / p95 | {s['remember_ms']['p50']} / {s['remember_ms']['p95']} ms |")
    lines.append(f"| remember_document() | {s['remember_document']['ms_per_chunk']} ms/chunk |")
    lines.append(f"| recall() p50 / p95 | {s['recall_ms']['p50']} / {s['recall_ms']['p95']} ms |")
    lines.append(f"| recall_index() p50 | {s['recall_index_ms']['p50']} ms |")
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _connect_real(allow_existing: bool) -> tuple[Any, Any, float]:
    """Real store and embedder, guarded against a populated store."""
    from memory.embedder import Embedder
    from memory.store import ValkeyStore

    store = ValkeyStore()
    store.connect()
    existing = len(store.scan_prefix("mem:"))
    if existing and not allow_existing:
        sys.exit(
            f"refusing to run: the store at {os.getenv('VALKEY_HOST', 'valkey')}:"
            f"{os.getenv('VALKEY_PORT', '6379')} already holds {existing} memories. "
            "Point the benchmark at a throwaway valkey-search (see docs), or pass "
            "--allow-existing if you really mean it."
        )
    embedder = Embedder()
    _, load_ms = _timed(embedder.load)
    return store, embedder, load_ms


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="ingest, query, and write a report")
    run.add_argument("--out", required=True, help="report JSON path")
    run.add_argument("--label", default="", help="free-text label, e.g. 'torch' or 'onnx'")
    run.add_argument("--corpus", help="a dump_to_file backup to use as the corpus")
    run.add_argument("--size", type=int, default=DEFAULT_SIZE, help="corpus size")
    run.add_argument("--queries", type=int, default=DEFAULT_QUERIES, help="number of queries")
    run.add_argument("--seed", type=int, default=7)
    run.add_argument("--allow-existing", action="store_true",
                     help="run even if the store already holds memories")

    cmp_p = sub.add_parser("compare", help="compare two reports")
    cmp_p.add_argument("before")
    cmp_p.add_argument("after")
    cmp_p.add_argument("--out", help="write the comparison JSON here too")

    args = parser.parse_args(argv)

    if args.command == "compare":
        with open(args.before, encoding="utf-8") as fh:
            before = json.load(fh)
        with open(args.after, encoding="utf-8") as fh:
            after = json.load(fh)
        result = compare(before, after)
        if args.out:
            Path(args.out).parent.mkdir(parents=True, exist_ok=True)
            with open(args.out, "w", encoding="utf-8") as fh:
                json.dump(result, fh, indent=2)
        print(format_comparison(result))
        return 1 if result["warnings"] else 0

    corpus = (corpus_from_dump(args.corpus, args.size) if args.corpus
              else synthetic_corpus(args.size, args.seed))
    if not corpus:
        sys.exit("corpus is empty")
    queries = derive_queries(corpus, args.queries, args.seed + 4)
    store, embedder, load_ms = _connect_real(args.allow_existing)
    report = run_benchmark(store, embedder, corpus, queries, load_ms=load_ms, label=args.label)
    report["corpus"]["source"] = args.corpus or f"synthetic(seed={args.seed})"
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=2)
    print(format_report(report))
    print(f"report written to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
