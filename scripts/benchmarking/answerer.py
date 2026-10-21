#!/usr/bin/env python3
"""Turning retrieved memories into an answer, and the baselines to beat.

The answerer is the most abusable part of a memory benchmark, because the
prompt can quietly do the memory system's job for it. mem0's published harness
ships an answer prompt of roughly 1,400 words containing instructions such as
"never output 2025 or 2026", "NEVER say 'not specified'", and open-domain
heuristics like "if the most recent attempt involved a bad experience, answer
'likely no'". That prompt was developed against mem0's own retrieval output
and is specific to one dataset. Any system scored through it inherits tuning
it did not earn, and the resulting number measures the harness.

So the prompt here is deliberately short, neutral, and dataset agnostic. It
contains no date ranges, no hints about answer shape, and no instruction to
guess rather than abstain. It is reproduced verbatim in the report, because a
benchmark whose prompt is not published is not a benchmark, it is a claim.

Three answerers, so the memory system's contribution can actually be isolated:

    none          no context at all. The floor: anything it gets right was
                  guessable from the question, or already in the model.
    memory        only what the memory system retrieved. The system under test.
    full_context  the entire conversation history stuffed into the prompt. The
                  ceiling, and an honest one: mem0's own paper shows full
                  context beating mem0 on accuracy, 72.90 against 66.88. If it
                  beats us too, the report says so.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path


def _sandbox_cwd() -> str:
    """A harmless working directory for spawned CLI calls.

    A spawned ``claude`` inherits the project's CLAUDE.md and file-based memory
    instructions from whatever directory it runs in. During development one such
    call decided the task was to record a memory, and duly wrote a file into the
    operator's real memory directory and indexed it in MEMORY.md. Running from an
    empty scratch directory keeps judge and answerer calls from touching the
    project, the repository, or anyone's memories.
    """
    base = os.environ.get("OMNIMEM_BENCH_TMP", tempfile.gettempdir())
    path = Path(base) / "cli-sandbox"
    path.mkdir(parents=True, exist_ok=True)
    return str(path)


NEUTRAL_PROMPT = """\
Answer the question using only the notes provided below.

Answer directly and concisely. Do not explain your reasoning, and do not
mention the notes. If the notes do not contain enough information to answer,
reply exactly: I don't know.

NOTES:
{context}

QUESTION: {question}"""

NO_CONTEXT_PROMPT = """\
Answer the question directly and concisely. Do not explain your reasoning. If
you do not have enough information to answer, reply exactly: I don't know.

QUESTION: {question}"""


@dataclass
class Answer:
    """One generated answer, with what it cost to produce."""

    text: str
    mode: str
    model: str
    latency_ms: float = 0.0
    input_tokens: int = 0
    output_tokens: int = 0
    cache_read_tokens: int = 0
    cache_creation_tokens: int = 0
    cost_usd: float = 0.0
    context_tokens: int = 0
    error: str = ""

    @property
    def total_input_tokens(self) -> int:
        """Every input token the call actually consumed, cache included.

        Reported rather than the bare ``input_tokens`` because prompt caching
        makes that field alone wildly understate the real context size.
        """
        return self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens


@dataclass
class ClaudeCliAnswerer:
    """Generate answers through the local ``claude`` CLI.

    Uses the CLI rather than the API so no API key is needed, and pins
    ``--strict-mcp-config`` with an empty server list so the answerer cannot
    reach a real memory server. Without that it could retrieve context it was
    never given, which would silently invalidate every number in the run.
    """

    model: str = "claude-haiku-4-5-20251001"
    timeout: float = 180.0

    def answer(self, question: str, context: str | None, mode: str = "memory") -> Answer:
        if context is None:
            prompt = NO_CONTEXT_PROMPT.format(question=question)
            context_tokens = 0
        else:
            prompt = NEUTRAL_PROMPT.format(context=context, question=question)
            context_tokens = max(1, len(context) // 4)

        started = time.monotonic()
        try:
            proc = subprocess.run(
                [
                    "claude", "-p", prompt,
                    "--output-format", "json",
                    "--model", self.model,
                    "--strict-mcp-config",
                    "--mcp-config", '{"mcpServers":{}}',
                ],
                capture_output=True, text=True, timeout=self.timeout,
                stdin=subprocess.DEVNULL,
                cwd=_sandbox_cwd(),
            )
        except subprocess.TimeoutExpired:
            return Answer("", mode, self.model,
                          latency_ms=(time.monotonic() - started) * 1000,
                          context_tokens=context_tokens, error="timeout")
        latency_ms = (time.monotonic() - started) * 1000

        if proc.returncode != 0:
            return Answer("", mode, self.model, latency_ms=latency_ms,
                          context_tokens=context_tokens, error=proc.stderr[:300])
        try:
            payload = json.loads(proc.stdout)
        except json.JSONDecodeError:
            return Answer("", mode, self.model, latency_ms=latency_ms,
                          context_tokens=context_tokens, error=proc.stdout[:300])

        usage = payload.get("usage", {}) or {}
        return Answer(
            text=str(payload.get("result", "")).strip(),
            mode=mode,
            model=self.model,
            latency_ms=latency_ms,
            input_tokens=int(usage.get("input_tokens", 0)),
            output_tokens=int(usage.get("output_tokens", 0)),
            cache_read_tokens=int(usage.get("cache_read_input_tokens", 0)),
            cache_creation_tokens=int(usage.get("cache_creation_input_tokens", 0)),
            cost_usd=float(payload.get("total_cost_usd", 0.0)),
            context_tokens=context_tokens,
        )


def format_memories(hits: list[dict], include_scores: bool = False) -> str:
    """Render retrieved memories as the notes block.

    Presented as a plain numbered list in retrieval order. Scores are omitted
    by default: showing the model how confident retrieval was is a hint the
    memory system would not have in production.
    """
    lines = []
    for index, hit in enumerate(hits, start=1):
        content = str(hit.get("content", "")).strip()
        if include_scores and "score" in hit:
            lines.append(f"{index}. [{hit['score']:.3f}] {content}")
        else:
            lines.append(f"{index}. {content}")
    return "\n".join(lines) if lines else "(no notes were retrieved)"


def format_full_context(sessions: list[dict]) -> str:
    """Render an entire haystack for the full-context ceiling baseline.

    Expensive on purpose: a LongMemEval haystack runs to roughly 122,000
    tokens, so this baseline costs far more per question than the memory path.
    That cost difference is itself a result worth reporting.
    """
    blocks = []
    for session in sessions:
        blocks.append(f"--- {session.get('date', 'unknown date')} ---")
        for turn in session.get("turns", []):
            blocks.append(f"{turn.get('role', '?')}: {turn.get('content', '')}")
    return "\n".join(blocks)
