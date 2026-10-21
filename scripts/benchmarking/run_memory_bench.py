#!/usr/bin/env python3
"""Run a memory benchmark against one OmniMem version and record everything.

    python run_memory_bench.py --version v7 --size 60 --out results/v7-lme.jsonl
    python run_memory_bench.py --version v6 --size 60 --out results/v6-lme.jsonl
    python run_memory_bench.py --version v7 --size 2 --dry-run    # cost estimate

The shape of a run, per question:

    reset  ->  ingest  ->  settle  ->  retrieve  ->  answer  ->  judge

Every question gets a **fresh empty store**, because LongMemEval gives each
question its own haystack and mixing them would let one question's evidence
answer another's. That is why this is slow, and why it is correct.

``settle`` matters more than it looks. Both versions enrich memories on a
background queue, so retrieving the instant after ingest would measure a
half-built index. The run waits for the queue to drain, and reports how long
that took as its own number rather than hiding it inside recall latency.

Results stream to JSONL as they are produced, one record per question, so a
run that dies at question 47 of 60 loses nothing and ``--resume`` picks it up.
Per-question records are meant to be committed: a benchmark nobody can audit
is marketing, and the whole argument for this harness is that mem0's numbers
cannot be reproduced from their own published code.
"""

from __future__ import annotations

import argparse
import json
import platform
import socket
import sys
import time
from dataclasses import asdict
from datetime import datetime, timezone
from pathlib import Path

import answerer as answerer_mod
import corpus as corpus_mod
import instances as instances_mod
import judge as judge_mod
import metrics as metrics_mod
from mcpclient import McpClient, McpError

SCHEMA_VERSION = 1
BENCH_PROJECT = "omnimem-bench"
MAX_TOP_K = 50  # 6.x refuses more; enforced here so both versions match


def settle(client: McpClient, timeout: float = 180.0) -> dict:
    """Wait for background enrichment to drain before measuring retrieval.

    Returns how long it took and whether it actually finished. A run where
    the queue never drained is still reported, flagged, rather than quietly
    producing fast-looking recall over an unfinished index.
    """
    started = time.monotonic()
    last: dict = {}
    while time.monotonic() - started < timeout:
        try:
            last = client.call_ok("queue_status").json()
        except (McpError, ValueError):
            return {"settled": True, "wait_ms": 0.0, "note": "queue_status unavailable"}
        pending = last.get("pending", last.get("queued", 0))
        try:
            pending = int(pending)
        except (TypeError, ValueError):
            pending = 0
        if pending <= 0:
            return {"settled": True, "wait_ms": (time.monotonic() - started) * 1000}
        time.sleep(0.25)
    return {
        "settled": False,
        "wait_ms": (time.monotonic() - started) * 1000,
        "note": f"queue did not drain within {timeout}s: {last}",
    }


def ingest(client: McpClient, memories: list, progress_every: int = 250,
           force: bool = False) -> dict:
    """Store every memory for one question, timing each write.

    ``force`` is off by default, and that matters more than it looks. The
    engine computes ``enrich_after = mode == "full" && ns != "knowledge" &&
    !force``, so forcing every write silently disables fact extraction for the
    entire corpus. An earlier version of this harness forced unconditionally
    to stop dedup dropping repeated conversation turns, and so measured
    OmniMem with extraction switched off without saying so.

    The cost of leaving it off is that near-duplicate turns may be dropped.
    That is the honest trade, and it is now a visible flag rather than a
    buried default.
    """
    latencies: list[float] = []
    failures = 0
    started = time.monotonic()
    for position, memory in enumerate(memories, start=1):
        try:
            result = client.call(
                "remember",
                {
                    "content": memory.content,
                    "project": BENCH_PROJECT,
                    "namespace": "episodic",
                    "force": force,
                },
            )
            latencies.append(result.latency_ms)
            if result.is_error:
                failures += 1
        except McpError:
            failures += 1
        if progress_every and position % progress_every == 0:
            print(f"      ingested {position}/{len(memories)}", flush=True)
    return {
        "memories": len(memories),
        "failures": failures,
        "wall_ms": (time.monotonic() - started) * 1000,
        "latency": metrics_mod.latency_summary(latencies),
    }


def retrieve(client: McpClient, question: str, top_k: int) -> tuple[list[dict], float, str]:
    """One recall call, returning hits, latency and any error."""
    try:
        result = client.call("recall", {"query": question, "top_k": min(top_k, MAX_TOP_K)})
    except McpError as exc:
        return [], 0.0, str(exc)[:300]
    if result.is_error:
        return [], result.latency_ms, result.text[:300]
    try:
        payload = result.json()
    except McpError as exc:
        return [], result.latency_ms, str(exc)[:300]
    hits = payload if isinstance(payload, list) else payload.get("results", [])
    return list(hits), result.latency_ms, ""


def run_question(
    question: corpus_mod.Question,
    manager,
    args,
    answer_engine: answerer_mod.ClaudeCliAnswerer,
    panel: judge_mod.JudgePanel,
) -> dict:
    """Reset, ingest, retrieve, answer and judge one question."""
    instance = manager.reset()
    record: dict = {
        "qid": question.qid,
        "category": question.category,
        "question": question.question,
        "gold": question.answer,
        "question_date": question.question_date,
        "sessions": len(question.sessions),
        "omnimem_version": instance.omnimem_version,
        "answers": {},
    }

    with McpClient(instance.base_url) as client:
        instances_mod.assert_empty(client)
        memories = question.memories(args.granularity)
        record["ingest"] = ingest(client, memories, args.progress_every,
                                  force=args.force_ingest)
        record["settle"] = settle(client)

        hits, latency_ms, error = retrieve(client, question.question, args.top_k)
        context = answerer_mod.format_memories(hits)
        record["retrieval"] = {
            "top_k": args.top_k,
            "latency_ms": latency_ms,
            "returned": len(hits),
            "error": error,
            "context_tokens": metrics_mod.estimate_tokens(context),
            # Truncated so committed results stay readable, but enough to see
            # what retrieval actually surfaced when a judgement looks wrong.
            "hits": [
                {
                    "key": h.get("key"),
                    "score": h.get("score"),
                    "content": str(h.get("content", ""))[:300],
                }
                for h in hits
            ],
        }

    for mode in args.modes:
        if mode == "memory":
            answer = answer_engine.answer(question.question, context, mode)
        elif mode == "none":
            answer = answer_engine.answer(question.question, None, mode)
        elif mode == "full_context":
            answer = answer_engine.answer(
                question.question, answerer_mod.format_full_context(question.sessions), mode
            )
        else:
            continue

        entry = asdict(answer)
        entry["f1"] = metrics_mod.f1(answer.text, question.answer)
        entry["bleu1"] = metrics_mod.bleu1(answer.text, question.answer)
        entry["judgements"] = {}
        for rubric in args.rubrics:
            verdicts = panel.judge(question.question, question.answer, answer.text, rubric)
            entry["judgements"][rubric] = [v.to_dict() for v in verdicts]
        record["answers"][mode] = entry

    return record


def summarise(records: list[dict], args) -> dict:
    """Aggregate a finished run into the numbers the report quotes."""
    summary: dict = {"questions": len(records), "modes": {}}
    ingest_latencies, settle_waits, recall_latencies, context_tokens = [], [], [], []
    for record in records:
        ingest_stats = record.get("ingest", {}).get("latency", {})
        if ingest_stats.get("p50"):
            ingest_latencies.append(ingest_stats["p50"])
        settle_waits.append(record.get("settle", {}).get("wait_ms", 0.0))
        recall_latencies.append(record.get("retrieval", {}).get("latency_ms", 0.0))
        context_tokens.append(record.get("retrieval", {}).get("context_tokens", 0))

    summary["ingest_p50_per_question_ms"] = metrics_mod.latency_summary(ingest_latencies)
    summary["settle_ms"] = metrics_mod.latency_summary(settle_waits)
    summary["recall_ms"] = metrics_mod.latency_summary(recall_latencies)
    summary["retrieved_context_tokens"] = metrics_mod.token_summary(context_tokens)

    for mode in args.modes:
        mode_summary: dict = {"rubrics": {}}
        f1_scores = [
            r["answers"][mode]["f1"] for r in records if mode in r.get("answers", {})
        ]
        bleu_scores = [
            r["answers"][mode]["bleu1"] for r in records if mode in r.get("answers", {})
        ]
        cost = sum(
            r["answers"][mode].get("cost_usd", 0.0)
            for r in records if mode in r.get("answers", {})
        )
        tokens = [
            r["answers"][mode].get("input_tokens", 0)
            + r["answers"][mode].get("cache_read_tokens", 0)
            + r["answers"][mode].get("cache_creation_tokens", 0)
            for r in records if mode in r.get("answers", {})
        ]
        mode_summary["f1_mean"] = (sum(f1_scores) / len(f1_scores)) if f1_scores else None
        mode_summary["bleu1_mean"] = (sum(bleu_scores) / len(bleu_scores)) if bleu_scores else None
        mode_summary["answer_cost_usd"] = cost
        mode_summary["answer_input_tokens"] = metrics_mod.token_summary(tokens)

        for rubric in args.rubrics:
            per_judge: dict = {}
            rows_for_agreement = []
            for record in records:
                entry = record.get("answers", {}).get(mode)
                if not entry:
                    continue
                verdicts = entry.get("judgements", {}).get(rubric, [])
                rows_for_agreement.append(
                    [judge_mod.Judgement(**v) for v in verdicts]
                )
                for verdict in verdicts:
                    per_judge.setdefault(verdict["judge"], []).append(
                        {"label": verdict["label"], "category": record["category"]}
                    )
            rubric_summary = {}
            for judge_name, rows in per_judge.items():
                rubric_summary[judge_name] = {
                    "overall": metrics_mod.accuracy([r["label"] for r in rows]),
                    "by_category": metrics_mod.group_by(rows, "category"),
                }
            rubric_summary["agreement"] = judge_mod.JudgePanel.agreement(rows_for_agreement)
            mode_summary["rubrics"][rubric] = rubric_summary
        summary["modes"][mode] = mode_summary
    return summary


def estimate_cost(questions: list, args) -> dict:
    """What a run will cost before it is started.

    Each ``claude`` CLI call carries roughly 18,000 tokens of system prompt
    regardless of how small the actual prompt is, so cost tracks the number of
    calls far more than the size of any one of them.
    """
    per_call_usd = 0.033
    answers = len(questions) * len(args.modes)
    judgements = answers * len(args.rubrics) * max(1, args.judge_count)
    memories = sum(len(q.memories(args.granularity)) for q in questions)
    return {
        "questions": len(questions),
        "memories_to_ingest": memories,
        "answer_calls": answers,
        "judge_calls": judgements,
        "llm_calls_total": answers + judgements,
        "estimated_usd": round((answers + judgements) * per_call_usd, 2),
        "note": "estimate at roughly $0.033 per claude CLI call, measured on Haiku",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--version", choices=("v6", "v7"), required=True)
    parser.add_argument("--size", type=int, default=60, help="stratified sample size")
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--granularity", choices=("turn", "session"), default="turn")
    parser.add_argument("--top-k", type=int, default=20)
    parser.add_argument("--modes", default="memory,none",
                        help="comma separated: memory, none, full_context")
    parser.add_argument("--rubrics", default="strict,lenient")
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--resume", action="store_true", help="skip questions already in --out")
    parser.add_argument("--dry-run", action="store_true", help="print the plan and cost, run nothing")
    parser.add_argument("--no-openai-judge", action="store_true")
    parser.add_argument("--progress-every", type=int, default=250)
    parser.add_argument("--force-ingest", action="store_true",
                        help="pass force=True on every remember. Keeps near-duplicate "
                             "turns, but disables fact extraction for the whole run")
    parser.add_argument("--data-dir", type=Path, default=None)
    args = parser.parse_args()

    args.modes = [m.strip() for m in args.modes.split(",") if m.strip()]
    args.rubrics = [r.strip() for r in args.rubrics.split(",") if r.strip()]
    if args.top_k > MAX_TOP_K:
        print(f"--top-k capped at {MAX_TOP_K}, which is 6.x's limit", file=sys.stderr)
        args.top_k = MAX_TOP_K

    questions = corpus_mod.stratified_sample(
        corpus_mod.load_longmemeval(args.data_dir), args.size, args.seed
    )
    panel = judge_mod.build_panel(use_openai=not args.no_openai_judge)
    args.judge_count = len(panel.judges)

    plan = estimate_cost(questions, args)
    print(json.dumps({"plan": plan, "corpus": corpus_mod.describe(questions)}, indent=2))
    if args.dry_run:
        return 0

    out_path = args.out or Path("results") / f"{args.version}-longmemeval-{args.size}.jsonl"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    done: set[str] = set()
    if args.resume and out_path.exists():
        for line in out_path.read_text().splitlines():
            if line.strip():
                done.add(json.loads(line)["qid"])
        print(f"resuming: {len(done)} questions already recorded")

    manager = instances_mod.build(args.version)
    answer_engine = answerer_mod.ClaudeCliAnswerer()
    started = time.monotonic()
    records: list[dict] = []
    if out_path.exists() and args.resume:
        records = [json.loads(l) for l in out_path.read_text().splitlines() if l.strip()]

    try:
        manager.start()
        with out_path.open("a") as sink:
            for index, question in enumerate(questions, start=1):
                if question.qid in done:
                    continue
                print(f"[{index}/{len(questions)}] {question.category} {question.qid}", flush=True)
                record = run_question(question, manager, args, answer_engine, panel)
                sink.write(json.dumps(record) + "\n")
                sink.flush()
                records.append(record)
    finally:
        manager.stop()

    metadata = {
        "schema_version": SCHEMA_VERSION,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "omnimem_version": records[0].get("omnimem_version") if records else "unknown",
        "version_line": args.version,
        "dataset": "LongMemEval (longmemeval_s_cleaned)",
        "dataset_licence": "MIT",
        "sample_size": args.size,
        "seed": args.seed,
        "granularity": args.granularity,
        "force_ingest": args.force_ingest,
        "top_k": args.top_k,
        "modes": args.modes,
        "rubrics": args.rubrics,
        "judges": [{"name": j.name, "model": j.model} for j in panel.judges],
        "answerer_model": answer_engine.model,
        "answer_prompt": answerer_mod.NEUTRAL_PROMPT,
        "host": {"node": socket.gethostname(), "platform": platform.platform()},
        "wall_seconds": time.monotonic() - started,
    }
    report = {"metadata": metadata, "summary": summarise(records, args)}
    summary_path = out_path.with_suffix(".summary.json")
    summary_path.write_text(json.dumps(report, indent=2))
    print(json.dumps(report["summary"], indent=2))
    print(f"\nper-question records: {out_path}\nsummary: {summary_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
