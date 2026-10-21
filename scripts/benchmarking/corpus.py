#!/usr/bin/env python3
"""Benchmark corpora: loading, sampling, and turning them into memories.

Currently LongMemEval, with BEAM to follow. Both are licensed for commercial
use, which is why they are here and LOCOMO is not: LOCOMO ships under
CC BY-NC 4.0, and the whole point of this harness is to produce numbers we can
publish.

    LongMemEval   MIT           github.com/xiaowu0162/LongMemEval
    BEAM          CC BY-SA 4.0  huggingface.co/datasets/Mohammadta/BEAM

Sampling is not optional. Every LongMemEval question carries its own haystack
of roughly 48 sessions and 122,000 tokens, and those haystacks must not bleed
into each other, so a full 500-question run means ingesting about 61 million
tokens into 500 separate stores. The default is therefore a stratified sample
with a recorded seed, and any report generated from it states both.
"""

from __future__ import annotations

import json
import os
import random
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_DATA_DIR = Path(os.environ.get("OMNIMEM_BENCH_DATA", REPO_ROOT / "data" / "benchmarks"))

LONGMEMEVAL_FILE = "longmemeval_s_cleaned.json"
LONGMEMEVAL_URL = (
    "https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/"
    "resolve/main/longmemeval_s_cleaned.json"
)

# The six LongMemEval question types, as they appear in the data.
LONGMEMEVAL_CATEGORIES = (
    "single-session-user",
    "single-session-assistant",
    "single-session-preference",
    "multi-session",
    "temporal-reasoning",
    "knowledge-update",
)


@dataclass
class Memory:
    """One unit of text to store, with the metadata the store will keep."""

    content: str
    session_id: str
    session_date: str
    index: int


@dataclass
class Question:
    """One benchmark question and the haystack it must be answered from."""

    qid: str
    question: str
    answer: str
    category: str
    question_date: str
    sessions: list[dict] = field(repr=False, default_factory=list)
    answer_session_ids: list[str] = field(default_factory=list)

    def memories(self, granularity: str = "turn") -> list[Memory]:
        """Flatten the haystack into the units that get stored.

        The session date is prefixed onto every memory. Without it the
        temporal-reasoning questions, a quarter of the set, are simply
        unanswerable from the store no matter how good retrieval is, and the
        benchmark would be measuring the harness rather than the system.

        ``turn`` stores each message separately, which is how an agent would
        actually write a conversation down as it happens. ``session`` stores
        one memory per session, which is coarser and much faster to ingest.
        """
        out: list[Memory] = []
        for session in self.sessions:
            sid, date, turns = session["id"], session["date"], session["turns"]
            if granularity == "session":
                body = "\n".join(f"{t['role']}: {t['content']}" for t in turns)
                out.append(Memory(f"[{date}] {body}", sid, date, len(out)))
                continue
            for turn in turns:
                body = f"{turn['role']}: {turn['content']}"
                out.append(Memory(f"[{date}] {body}", sid, date, len(out)))
        return out

    def is_evidence(self, session_id: str) -> bool:
        """Whether this session is one the gold answer actually came from."""
        return session_id in self.answer_session_ids


def data_path(filename: str, data_dir: Path | None = None) -> Path:
    return (data_dir or DEFAULT_DATA_DIR) / filename


def load_longmemeval(data_dir: Path | None = None) -> list[Question]:
    """Read LongMemEval into Question objects.

    The file is roughly 277MB, so this is deliberately called once and the
    result sampled, rather than being re-read per question.
    """
    path = data_path(LONGMEMEVAL_FILE, data_dir)
    if not path.exists():
        raise FileNotFoundError(
            f"{path} is missing. Fetch it with:\n"
            f"  mkdir -p {path.parent} && curl -L -o {path} {LONGMEMEVAL_URL}"
        )
    raw = json.loads(path.read_text())
    questions: list[Question] = []
    for record in raw:
        sessions = [
            {"id": sid, "date": date, "turns": turns}
            for sid, date, turns in zip(
                record.get("haystack_session_ids", []),
                record.get("haystack_dates", []),
                record.get("haystack_sessions", []),
            )
        ]
        questions.append(
            Question(
                qid=record["question_id"],
                question=record["question"],
                answer=str(record["answer"]),
                category=record["question_type"],
                question_date=record.get("question_date", ""),
                sessions=sessions,
                answer_session_ids=list(record.get("answer_session_ids", [])),
            )
        )
    return questions


def stratified_sample(questions: list[Question], size: int, seed: int = 7) -> list[Question]:
    """Take ``size`` questions, spread across categories in proportion.

    Deterministic for a given seed, and the seed goes into the report. Largest
    remainder is used to hand out the leftover slots so the sample really does
    add up to ``size`` rather than drifting a few either way.
    """
    if size >= len(questions):
        return list(questions)
    by_category: dict[str, list[Question]] = defaultdict(list)
    for question in questions:
        by_category[question.category].append(question)

    total = len(questions)
    exact = {cat: len(items) * size / total for cat, items in by_category.items()}
    quota = {cat: int(value) for cat, value in exact.items()}
    remainder = size - sum(quota.values())
    for cat, _ in sorted(exact.items(), key=lambda kv: kv[1] - int(kv[1]), reverse=True):
        if remainder <= 0:
            break
        quota[cat] += 1
        remainder -= 1

    rng = random.Random(seed)
    chosen: list[Question] = []
    for cat, items in sorted(by_category.items()):
        picks = min(quota.get(cat, 0), len(items))
        chosen.extend(rng.sample(items, picks))
    rng.shuffle(chosen)
    return chosen


def describe(questions: list[Question]) -> dict:
    """Summary of a question set, for the run metadata and the report."""
    counts = Counter(q.category for q in questions)
    sessions = [len(q.sessions) for q in questions]
    return {
        "questions": len(questions),
        "categories": dict(sorted(counts.items())),
        "sessions_per_question": {
            "min": min(sessions) if sessions else 0,
            "max": max(sessions) if sessions else 0,
        },
    }


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(description="Inspect or sample a benchmark corpus.")
    parser.add_argument("--data-dir", type=Path, default=None)
    parser.add_argument("--size", type=int, default=60, help="stratified sample size")
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--granularity", choices=("turn", "session"), default="turn")
    args = parser.parse_args()

    all_questions = load_longmemeval(args.data_dir)
    print("full set:  ", json.dumps(describe(all_questions)))
    sample = stratified_sample(all_questions, args.size, args.seed)
    print(f"sample({args.size}, seed={args.seed}):", json.dumps(describe(sample)))
    first = sample[0]
    memories = first.memories(args.granularity)
    print(f"\nexample question [{first.category}] {first.qid}")
    print(f"  Q: {first.question}")
    print(f"  A: {first.answer}")
    print(f"  memories at {args.granularity} granularity: {len(memories)}")
    print(f"  first memory: {memories[0].content[:160]!r}")
    ingest_units = sum(len(q.memories(args.granularity)) for q in sample)
    print(f"\ntotal memories to ingest for this sample: {ingest_units:,}")
