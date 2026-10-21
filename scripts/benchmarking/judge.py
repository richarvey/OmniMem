#!/usr/bin/env python3
"""Grading answers, and being honest about how they were graded.

Accuracy on these benchmarks is decided by an LLM reading the gold answer and
the generated one. That makes the rubric part of the result: the same system
scores very differently under a strict rubric than a generous one. mem0's own
numbers demonstrate this, having moved from about 67% under the stricter
paper-era rubric to about 92% under a rubric that awards full credit for one
correct item out of several. Neither number is dishonest on its own, but
quoting either without its rubric is.

So this module does two things the published mem0 harness does not:

* It carries **both** rubrics and can run either. The strict one is the
  headline; the lenient one is reported beside it, labelled, so the gap is
  visible rather than hidden.
* It supports **two judges from different vendors** and reports their
  agreement rate. "Claude judged Claude's answers" is the first objection any
  sceptical reader raises, and an agreement figure against a non-Anthropic
  judge is the cheapest possible answer to it.

The Claude judge shells out to the ``claude`` CLI rather than the API, because
it authenticates from the existing session and needs no API key. The external
judge needs its own key and says so plainly if one is missing, rather than
silently scoring nothing.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Protocol


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


# The stricter rubric, adapted from the one mem0's paper used, which itself
# derives from MemGPT (Packer et al. 2023). Kept close to the original wording
# so results remain comparable with the paper-era literature.
STRICT_RUBRIC = """\
Your task is to label an answer to a question as CORRECT or WRONG.

You are given a question, a gold (correct) answer, and a generated answer.
The generated answer may be much longer than the gold answer. Judge only
whether it actually answers the question consistently with the gold answer.

Mark CORRECT when the generated answer conveys the same substantive answer as
the gold answer. Mark WRONG when it contradicts the gold answer, omits the
answer, hedges without committing, or answers a different question.

For time related questions, the generated answer is CORRECT if it refers to
the same date or time period as the gold answer.

First give a one sentence explanation of your reasoning. Then return your
verdict as JSON on the final line, in the form {"label": "CORRECT"} or
{"label": "WRONG"}."""

# mem0's current rubric, reproduced so we can show what the same answers score
# under it. Reported as a labelled secondary number, never as the headline.
LENIENT_RUBRIC = """\
Your task is to label an answer to a question as CORRECT or WRONG.

You are given a question, a gold (correct) answer, and a generated answer.
Apply these rules:

1. PARTIAL CREDIT: if the generated answer includes at least one correct item
   from the gold answer's list, mark CORRECT. One out of two, two out of four
   and so on are all acceptable. Only mark WRONG if none of the gold items
   appear.
2. EXTRA DETAIL IS FINE: never penalise an answer for being more detailed or
   more specific than the gold answer.
3. DATE TOLERANCE: dates within 14 days of each other are CORRECT. Durations
   within 50 percent are CORRECT.
4. SAME REFERENT: if the generated answer references the same named entity as
   the gold answer, mark CORRECT even if it describes it differently.

ONLY mark WRONG if the generated answer contains zero correct items from the
gold answer, or addresses a completely different topic.

First give a one sentence explanation of your reasoning. Then return your
verdict as JSON on the final line, in the form {"label": "CORRECT"} or
{"label": "WRONG"}."""

RUBRICS = {"strict": STRICT_RUBRIC, "lenient": LENIENT_RUBRIC}

_PROMPT = """{rubric}

QUESTION: {question}
GOLD ANSWER: {gold}
GENERATED ANSWER: {prediction}"""


@dataclass
class Judgement:
    """One judge's verdict on one answer."""

    judge: str
    model: str
    rubric: str
    label: str  # CORRECT, WRONG, or ERROR
    reasoning: str = ""
    cost_usd: float = 0.0
    input_tokens: int = 0
    output_tokens: int = 0
    error: str = ""

    @property
    def correct(self) -> bool:
        return self.label == "CORRECT"

    def to_dict(self) -> dict:
        return asdict(self)


def _extract_label(text: str) -> tuple[str, str]:
    """Pull a verdict out of whatever the judge actually said.

    Judges are asked for JSON on the last line but do not always comply, so
    this falls back to looking for the bare words. Anything unreadable becomes
    ERROR rather than being quietly counted as WRONG, because silently
    grading unparseable responses as failures would understate every system
    equally but unpredictably.
    """
    reasoning = text.strip()
    for match in re.finditer(r'\{[^{}]*"label"\s*:\s*"(CORRECT|WRONG)"[^{}]*\}', text, re.I):
        return match.group(1).upper(), reasoning
    tail = text.strip().upper()
    if tail.endswith("CORRECT") and not tail.endswith("WRONG"):
        return "CORRECT", reasoning
    has_correct = re.search(r"\bCORRECT\b", tail) is not None
    has_wrong = re.search(r"\bWRONG\b", tail) is not None
    if has_correct != has_wrong:
        return ("CORRECT" if has_correct else "WRONG"), reasoning
    return "ERROR", reasoning


class Judge(Protocol):
    name: str
    model: str

    def judge(self, question: str, gold: str, prediction: str, rubric: str) -> Judgement: ...


@dataclass
class ClaudeCliJudge:
    """Judge via the local ``claude`` CLI, which uses the existing session auth.

    ``--strict-mcp-config`` with an empty config is important: without it the
    CLI would load whatever MCP servers are configured for this machine,
    including a real OmniMem, which would both slow judging down and let the
    judge retrieve things it should not see.
    """

    model: str = "claude-haiku-4-5-20251001"
    name: str = "claude-cli"
    timeout: float = 180.0

    def judge(self, question: str, gold: str, prediction: str, rubric: str = "strict") -> Judgement:
        prompt = _PROMPT.format(
            rubric=RUBRICS[rubric], question=question, gold=gold, prediction=prediction
        )
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
                stdin=subprocess.DEVNULL,  # else the CLI waits 3s for stdin on every call
                cwd=_sandbox_cwd(),
            )
        except subprocess.TimeoutExpired:
            return Judgement(self.name, self.model, rubric, "ERROR", error="timeout")
        if proc.returncode != 0:
            return Judgement(
                self.name, self.model, rubric, "ERROR", error=proc.stderr[:300]
            )
        try:
            payload = json.loads(proc.stdout)
        except json.JSONDecodeError:
            return Judgement(
                self.name, self.model, rubric, "ERROR", error=proc.stdout[:300]
            )
        label, reasoning = _extract_label(payload.get("result", ""))
        usage = payload.get("usage", {}) or {}
        return Judgement(
            judge=self.name,
            model=self.model,
            rubric=rubric,
            label=label,
            reasoning=reasoning[:500],
            cost_usd=float(payload.get("total_cost_usd", 0.0)),
            input_tokens=int(usage.get("input_tokens", 0))
            + int(usage.get("cache_read_input_tokens", 0))
            + int(usage.get("cache_creation_input_tokens", 0)),
            output_tokens=int(usage.get("output_tokens", 0)),
        )


@dataclass
class OpenAiJudge:
    """Judge via an OpenAI-compatible endpoint, for the second opinion.

    Deliberately a different vendor from the answerer. Temperature is pinned
    to 0 where the model allows it; note that the o-series and gpt-5 refuse
    anything but the default, which is one reason mem0's headline run is not
    reproducible.
    """

    model: str = "gpt-4o"
    name: str = "openai"
    base_url: str = "https://api.openai.com/v1"
    timeout: float = 120.0
    temperature: float | None = 0.0

    def judge(self, question: str, gold: str, prediction: str, rubric: str = "strict") -> Judgement:
        import httpx

        key = os.environ.get("OPENAI_API_KEY")
        if not key:
            return Judgement(
                self.name, self.model, rubric, "ERROR",
                error="OPENAI_API_KEY is not set, so the second judge did not run",
            )
        prompt = _PROMPT.format(
            rubric=RUBRICS[rubric], question=question, gold=gold, prediction=prediction
        )
        body: dict = {"model": self.model, "messages": [{"role": "user", "content": prompt}]}
        if self.temperature is not None:
            body["temperature"] = self.temperature
        try:
            response = httpx.post(
                f"{self.base_url}/chat/completions",
                json=body,
                headers={"authorization": f"Bearer {key}"},
                timeout=self.timeout,
            )
        except httpx.HTTPError as exc:
            return Judgement(self.name, self.model, rubric, "ERROR", error=str(exc)[:300])
        if response.status_code >= 400:
            return Judgement(
                self.name, self.model, rubric, "ERROR",
                error=f"HTTP {response.status_code}: {response.text[:200]}",
            )
        payload = response.json()
        text = payload["choices"][0]["message"]["content"]
        label, reasoning = _extract_label(text)
        usage = payload.get("usage", {}) or {}
        return Judgement(
            judge=self.name,
            model=self.model,
            rubric=rubric,
            label=label,
            reasoning=reasoning[:500],
            input_tokens=int(usage.get("prompt_tokens", 0)),
            output_tokens=int(usage.get("completion_tokens", 0)),
        )


@dataclass
class JudgePanel:
    """Runs several judges over the same answer and records their agreement."""

    judges: list[Judge] = field(default_factory=list)

    def judge(self, question: str, gold: str, prediction: str, rubric: str = "strict") -> list[Judgement]:
        return [j.judge(question, gold, prediction, rubric) for j in self.judges]

    @staticmethod
    def agreement(rows: list[list[Judgement]]) -> dict:
        """How often the judges agreed, over answers where all of them ran.

        Reported alongside accuracy. A low agreement rate means the accuracy
        figure is soft and should be quoted with that caveat, not polished.
        """
        comparable = [
            [j for j in row if j.label in ("CORRECT", "WRONG")]
            for row in rows
        ]
        usable = [row for row in comparable if len(row) > 1]
        if not usable:
            return {"comparable_answers": 0, "agreement": None}
        agreed = sum(1 for row in usable if len({j.label for j in row}) == 1)
        return {
            "comparable_answers": len(usable),
            "agreed": agreed,
            "agreement": agreed / len(usable),
        }


def build_panel(use_openai: bool = True, claude_model: str | None = None,
                openai_model: str | None = None) -> JudgePanel:
    """The default panel: Claude plus a non-Anthropic second opinion."""
    judges: list[Judge] = [ClaudeCliJudge(model=claude_model or ClaudeCliJudge.model)]
    if use_openai and os.environ.get("OPENAI_API_KEY"):
        judges.append(OpenAiJudge(model=openai_model or OpenAiJudge.model))
    return JudgePanel(judges=judges)


if __name__ == "__main__":
    # Cheap end-to-end check of the judging path, including that a wrong
    # answer is actually marked wrong. Costs a couple of small model calls.
    panel = build_panel()
    print(f"judges: {[j.name for j in panel.judges]}")
    if not os.environ.get("OPENAI_API_KEY"):
        print("note: OPENAI_API_KEY unset, so only the Claude judge is active")
    cases = [
        ("What subject did the user study?", "Business Administration",
         "They studied Business Administration at university.", "expect CORRECT"),
        ("What subject did the user study?", "Business Administration",
         "They studied Marine Biology.", "expect WRONG"),
    ]
    for question, gold, prediction, expectation in cases:
        for verdict in panel.judge(question, gold, prediction, "strict"):
            print(f"  [{expectation}] {verdict.judge}: {verdict.label} "
                  f"(${verdict.cost_usd:.4f}) {verdict.error[:80]}")
