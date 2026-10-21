#!/usr/bin/env python3
"""Does OmniMem actually save tokens? Two sessions, with and without it.

    python run_session_bench.py --out results/session.jsonl
    python run_session_bench.py --dry-run

The question this answers is not "is memory nice to have" but "what does it
cost and when does it pay for itself". Both arms run the same two tasks:

    session 1   learn a body of project facts and do a small task with them
    session 2   a FRESH session, related task, needing those same facts

    control     no MCP servers at all. Session 2 has no memory of session 1, so
                the facts must be pasted in again. That is what a real person
                does, and those re-pasted tokens are the honest cost of not
                having memory.
    omnimem     OmniMem attached over MCP. Session 1 stores what matters,
                session 2 recalls it. Pays a fixed overhead on every call for
                the tool definitions, and pays again for the recall round trip.

Reported as three separate numbers rather than one, because a single headline
would flatter whichever arm the task length happened to favour:

    fixed overhead   what attaching the server costs before any work is done
    task tokens      what the work itself consumed
    cache split      creation versus read, since re-runs hit cache differently

From those comes the crossover: the amount of carried context beyond which
OmniMem is cheaper than re-pasting. That number is more use, and far more
credible, than a percentage.

PREREQUISITES, and they are strict:

* An OmniMem server on the fixed port with its `.mcp.json` **approved once
  interactively**. Claude Code will not connect to an unapproved MCP server and
  gives no useful error when it refuses; it simply reports zero tools.
* A **patched** v7 binary. Unpatched v7 connects and returns no tools at all,
  because it omits the cache-hint fields protocol 2026-07-28 requires. The
  treatment arm would then silently measure a model with no memory access, and
  look like a fair result.

The harness therefore verifies tool callability by side effect before it
measures anything. It never asks the model whether it has tools: during
development the model twice reported confidently on its own tool situation and
was wrong both times.

Permissions: both arms are launched with an identical tool configuration, so
neither is advantaged. Note that ``--allowed-tools`` was measured NOT to be an
exclusive allowlist: an agent passed ``--allowed-tools Read,Glob,Grep`` still
executed Bash with no permission denial recorded. Any claim that an arm "could
not" reach the filesystem must therefore be backed by an enforcement mechanism
that has actually been tested, not by the presence of an allowlist flag.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
import time
import uuid
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path

import judge as judge_mod
from mcpclient import McpClient, McpError

BENCH_DIR = Path(os.environ.get("OMNIMEM_BENCH_DIR", Path.home() / ".omnimem-bench"))
BENCH_URL = os.environ.get("OMNIMEM_BENCH_URL", "http://127.0.0.1:8766")
BENCH_PROJECT = "omnimem-session-bench"
MODEL = "claude-haiku-4-5-20251001"
# The complete set of tools either arm may use. Deliberately tiny: no Bash, no
# file access, nothing that could answer the task by a route other than memory.
# Two different mechanisms. Both established by side effect, not by asking the
# model, because in this session the model misdescribed its own tool situation
# three separate times:
#
#   --allowed-tools     PERMITS calls. Without it an MCP tool call stalls in
#                       headless mode waiting for an approval nobody can give.
#                       It does NOT restrict: an agent given only Read/Glob/Grep
#                       still executed Bash, with zero permission denials.
#   --disallowed-tools  REMOVES tools. An agent instructed to write a file could
#                       not, and the file never appeared on disk.
#
# Read/Glob/Grep stay available deliberately: the control arm has to be able to
# genuinely re-derive the dead end, or there is no saving to measure. Both arms
# get identical flags; the control simply has no MCP server attached.
ALLOWED_TOOLS = ("Read", "Glob", "Grep", "mcp__omnimem-bench")
DISALLOWED_TOOLS = ("Bash", "Write", "Edit", "NotebookEdit", "WebFetch", "WebSearch")

# --- The scenario: a dead end that has to be walked before it is rejected ----
#
# Fixture lives under the approved bench directory, addressed relatively,
# because MCP approval is keyed to the working directory.
# The fixture lives OUTSIDE the approved working directory, and that is not
# cosmetic. It used to sit at ~/.omnimem-bench/fixture, and the CLI surfaced
# enough working-directory context that a session with Read, Glob and Grep all
# denied still quoted `Limiter::for_partition(shard_id, config)` from a vendored
# README and a verbatim line from docs/rollout.md. Proven by control: the
# identical prompt from an empty directory produced none of it and asked for the
# project path. Every run made while the fixture sat in the cwd was contaminated
# in both arms, so "withholding the codebase" withheld nothing.
FIXTURE = str(Path(os.environ.get("OMNIMEM_FIXTURE_DIR",
                                  Path.home() / ".omnimem-fixtures")) / "harrier")

# The task never names a library. TODO.md points at kestrel-rs as the obvious
# candidate, so an agent with no memory goes there first, reads its README,
# reads main.go, finds that a process-wide governor cannot work under
# per-shard runtimes, rejects it, and only then searches vendor/ for something
# else and finds pellham. That whole journey is what memory should replace.
TASK_ONE = (
    f"Read {FIXTURE}/TODO.md and work the rate limiting task it describes. "
    f"Investigate the vendored crates under {FIXTURE}/vendor/ against the "
    f"design in {FIXTURE}/cmd/ingestd/main.go and {FIXTURE}/docs/rollout.md. "
    f"Decide which crate to use and give the specific technical reason."
)
# Identical for both arms. Only tool availability differs.
TASK_TWO = (
    "We need rate limiting on the Harrier ingest path. Which vendored crate "
    "should we use, and why? Give the crate name and the specific technical "
    f"reason. The project is at {FIXTURE}."
)
# Session 2 runs twice per arm, with the codebase withheld both times.
#
#   cold     TASK_TWO alone. Nothing hints that a memory server exists. Tests
#            whether an agent reaches for memory unprompted, which is the
#            honest question and may well fail.
#   briefed  The workflow the server instructions actually prescribe. Tests
#            the intended path rather than a cold open.
#
# An earlier version appended "you do not have the codebase to hand: answer
# from what you already know". That was my mistake: the model read it as an
# instruction to answer from its own knowledge, did exactly that, and stopped
# in one turn without ever querying the store. The prompt discouraged the tool
# use it was meant to measure.
TASK_TWO_BRIEFED = (
    "Start by calling briefing(project='omnimem-session-bench') to pick up "
    "what earlier sessions recorded, then answer this:\n\n" + TASK_TWO
)
SESSION2_VARIANTS = ("session2_cold", "session2_briefed")
TASK_TWO_GOLD = (
    "pellham. kestrel-rs installs a process-wide governor via "
    "install_global, and limiters constructed on a different runtime block on "
    "that global handle, which breaks Harrier's per-shard isolation. pellham "
    "constructs an independent limiter per partition with no global state."
)


# Only the treatment arm is told to write down what it learned. That is the
# workflow OmniMem exists to support, and the control has no equivalent.
RECORD_SUFFIX = (
    "\n\nThen record this for future sessions. Call remember() describing what "
    "you investigated (project 'omnimem-session-bench'), take the key it "
    "returns, and call record_experience() on that key with effort_score 4, "
    "outcome 'abandoned', abandoned_approaches naming the crate you rejected "
    "(type 'library') with the technical reason, and breakthrough naming the "
    "crate that actually works."
)


@dataclass
class Turn:
    """One `claude -p` invocation, with everything it consumed."""

    arm: str
    label: str
    text: str = ""
    turns: int = 0
    input_tokens: int = 0
    output_tokens: int = 0
    cache_read_tokens: int = 0
    cache_creation_tokens: int = 0
    cost_usd: float = 0.0
    duration_ms: float = 0.0
    prompt_chars: int = 0
    # num_turns counts only the parent loop. An agent that delegates to
    # sub-agents can report turns=1 while running for three minutes and
    # spending real money, which is precisely the "went and looked it up"
    # behaviour this benchmark exists to measure. Captured verbatim rather
    # than parsed, so the shape can change without silently zeroing the work.
    subagent_stats: dict = field(default_factory=dict)
    error: str = ""

    @property
    def total_input(self) -> int:
        return self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens


def run_claude(arm: str, label: str, prompt: str, attach: bool, timeout: float = 300.0,
               files: bool = True, cwd: str | None = None) -> Turn:
    """One CLI call, either with OmniMem attached or with no MCP servers at all.

    ``--strict-mcp-config`` is not optional. Without it the CLI loads whatever
    servers are configured for this machine, which on a developer box includes
    a real OmniMem holding real memories. The control arm would stop being a
    control, and the treatment arm would write benchmark junk into production.
    """
    if attach:
        mcp_args = ["--strict-mcp-config", "--mcp-config", ".mcp.json"]
    else:
        mcp_args = ["--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}']
    # Both arms get the same tool configuration, so neither is advantaged.
    # This is NOT an enforced sandbox: --allowed-tools was measured to be a
    # permit list rather than an exclusive one, so it does not by itself stop
    # an agent reaching the filesystem. Enforcement, where the scenario needs
    # it, has to come from a mechanism that has been tested to deny.
    cmd = [
        "claude", "-p", prompt,
        "--output-format", "json",
        "--model", MODEL,
        "--allowed-tools", ",".join(ALLOWED_TOOLS),
        # Session 2 withholds the codebase from BOTH arms. With the fixture
        # readable, the treatment arm re-derived the answer from files and paid
        # the MCP overhead on top, so it lost on tokens while being right. This
        # models the realistic case the memory is for: you are away from the
        # repository and the only route to the answer is what was recorded.
        # It measures memory against nothing rather than against re-reading,
        # which is the kinder comparison, and the report must say so.
        "--disallowed-tools", ",".join(
            DISALLOWED_TOOLS if files else DISALLOWED_TOOLS + ("Read", "Glob", "Grep")),
        *mcp_args,
    ]
    started = time.monotonic()
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, timeout=timeout,
            stdin=subprocess.DEVNULL, cwd=cwd or BENCH_DIR,
        )
    except subprocess.TimeoutExpired:
        return Turn(arm, label, error="timeout", prompt_chars=len(prompt),
                    duration_ms=(time.monotonic() - started) * 1000)
    duration_ms = (time.monotonic() - started) * 1000
    if proc.returncode != 0:
        return Turn(arm, label, error=proc.stderr[:300], prompt_chars=len(prompt),
                    duration_ms=duration_ms)
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return Turn(arm, label, error=proc.stdout[:300], prompt_chars=len(prompt),
                    duration_ms=duration_ms)
    usage = payload.get("usage", {}) or {}
    return Turn(
        arm=arm,
        label=label,
        text=str(payload.get("result", "")).strip(),
        turns=int(payload.get("num_turns") or 1),
        input_tokens=int(usage.get("input_tokens", 0)),
        output_tokens=int(usage.get("output_tokens", 0)),
        cache_read_tokens=int(usage.get("cache_read_input_tokens", 0)),
        cache_creation_tokens=int(usage.get("cache_creation_input_tokens", 0)),
        cost_usd=float(payload.get("total_cost_usd", 0.0)),
        duration_ms=duration_ms,
        prompt_chars=len(prompt),
        subagent_stats=payload.get("subagent_stats") or {},
    )


def measure_fixed_overhead() -> dict:
    """What attaching OmniMem costs before any work happens.

    The same trivial prompt is sent twice, once with the server attached and
    once without, so the only difference is the tool definitions. Matched
    conditions matter: comparing two different prompts would attribute their
    difference to the server.
    """
    prompt = "Reply with exactly: OK"
    without = run_claude("control", "overhead-probe", prompt, attach=False)
    with_mcp = run_claude("omnimem", "overhead-probe", prompt, attach=True)
    per_turn_without = without.total_input / max(1, without.turns)
    per_turn_with = with_mcp.total_input / max(1, with_mcp.turns)
    return {
        "without_mcp_tokens_per_turn": per_turn_without,
        "with_mcp_tokens_per_turn": per_turn_with,
        "fixed_overhead_tokens": per_turn_with - per_turn_without,
        "without": asdict(without),
        "with": asdict(with_mcp),
    }


def _clean_room() -> str:
    """An empty directory, for probes that must see nothing.

    The guessability gate has to run somewhere with no fixture and no project
    context, or it measures working-directory leakage rather than whether the
    answer is genuinely recoverable. Run from the bench directory it reported a
    compromised fixture twice; run from here the same prompt returns nothing.
    """
    path = Path(os.environ.get("OMNIMEM_BENCH_TMP", tempfile.gettempdir())) / "clean-room"
    path.mkdir(parents=True, exist_ok=True)
    return str(path)


def assert_answer_not_guessable(samples: int = 3) -> dict:
    """Refuse to run if the answer can be produced with no tools and no memory.

    This check should have existed from the first run and did not. An earlier
    fixture named its crates `tollgate-rs` and `shardgate`, and `shardgate`
    telegraphed "the shard-aware one" so plainly that the model named it 3 of 3
    times with no files, no memory and no tools, reconstructing the project's
    constraints from priors alone. Every accuracy verdict from four benchmark
    runs was worthless, and nothing in the harness noticed.

    A scenario only measures memory if the answer is unreachable without it.
    """
    answer = TASK_TWO_GOLD.split(".")[0].strip().lower()
    hits = 0
    for _ in range(samples):
        turn = run_claude("guessprobe", "guess", TASK_TWO, attach=False,
                          files=False, cwd=_clean_room())
        if answer and answer in turn.text.lower():
            hits += 1
    verdict = {"samples": samples, "guessed": hits, "answer": answer,
               "guessable": hits > 0}
    if hits:
        raise SystemExit(
            f"refusing to run: the answer {answer!r} was produced {hits}/{samples} "
            "times with no tools, no files and no memory.\n"
            "The fixture is compromised: a correct answer proves nothing about "
            "memory. Rename so the answer carries no semantic hint."
        )
    return verdict


def verify_tools_callable() -> dict:
    """Prove the treatment arm can actually use OmniMem, before measuring it.

    Without this the benchmark's most likely failure is silent: the server
    connects, exposes nothing, and the treatment arm quietly measures a model
    with no memory at all while looking like a valid result.
    """
    nonce = f"SESSIONCHECK{uuid.uuid4().hex[:8].upper()}"
    prompt = (f"Use the omnimem-bench 'remember' MCP tool to store exactly this text: "
              f"{nonce}\nReply DONE once the tool call has succeeded.")
    turn = run_claude("omnimem", "tool-check", prompt, attach=True)
    removed = 0
    try:
        with McpClient(BENCH_URL) as client:
            hits = client.call("recall", {"query": nonce, "top_k": 5})
            found = nonce in hits.text
            tools = len(client.list_tools())
            # This check writes a memory to prove the tool works. Remove it, or
            # the measured run starts on a store that is not the empty one the
            # assertion just demanded, and the treatment arm recalls a stray
            # SESSIONCHECK record alongside the scenario.
            if found:
                try:
                    for item in hits.json():
                        key = item.get("key")
                        if key and nonce in str(item.get("content", "")):
                            client.call("forget", {"key_or_query": key, "confirm": True})
                            removed += 1
                except (McpError, ValueError, AttributeError, TypeError):
                    pass
    except McpError as exc:
        return {"callable": False, "error": str(exc)[:200], "tools": 0}
    return {"callable": found, "tools": tools, "cleaned": removed,
            "model_said": turn.text[:120]}


def assert_clean_store() -> dict:
    """Refuse to run unless the store is genuinely empty.

    Deleting the benchmark project is not enough, and this bit me. Recording an
    abandonment creates a store-wide suppression that outlives the project that
    created it, and a suppressed name hides every later memory mentioning it.
    Since the treatment arm's whole job is to record an abandonment, a second
    run would inherit the first run's suppression: session 2 could then look
    like a success on a suppression session 1 never made, or session 1's write
    could be hidden outright. Both failures still produce plausible numbers,
    which is the worst kind.
    """
    with McpClient(BENCH_URL) as client:
        records = client.call_ok("health").json().get("records", {})
        total = (sum(int(v) for v in records.values())
                 if isinstance(records, dict) else int(records))
        supp = client.call_ok("list_suppressions").json().get("suppressed_topics", [])
    if total or supp:
        raise SystemExit(
            f"refusing to run on a dirty store: {total} record(s), suppressions={supp}.\n"
            "Wipe the database and restart the server, then run again."
        )
    return {"records": total, "suppressions": supp}


def run_arm(arm: str) -> list[Turn]:
    """Both sessions of one arm.

    Session 1 does the investigation. Session 2 is a genuinely fresh session
    asking the same question again, which is where a memory layer either saves
    the re-derivation or does not.
    """
    attach = arm == "omnimem"
    one = TASK_ONE + (RECORD_SUFFIX if attach else "")
    first = run_claude(arm, "session1", one, attach)
    # Identical prompt for both arms. The control must re-read the fixture and
    # re-derive the incompatibility; the treatment has the option of recalling
    # it. Nothing in the prompt tells it to.
    # Both variants withhold the codebase. Prompts are identical across arms;
    # only tool availability differs.
    # Session 2 has the codebase available to BOTH arms. Withholding it made the
    # control unable to work rather than obliged to redo it, which measured the
    # wrong thing: the control simply stopped and asked for permissions. The
    # question that matters is whether memory stops an agent redoing a lookup it
    # could perfectly well do again, so the lookup has to be possible.
    cold = run_claude(arm, "session2_cold", TASK_TWO, attach, files=True)
    if not attach:
        # The control is NOT run against the briefed prompt. That prompt tells
        # the agent to call briefing(), a tool the control does not have, so the
        # cell is nonsense: in one run the control read TODO.md, took the crate
        # it names as "the obvious candidate", and answered wrong, having
        # answered correctly on the cold prompt minutes earlier. Using it as the
        # baseline flattered OmniMem by comparing against a control that had
        # been handed an impossible instruction.
        #
        # The baseline is control/session2_cold: the memoryless agent doing its
        # natural best work with the codebase in front of it.
        return [first, cold]
    briefed = run_claude(arm, "session2_briefed", TASK_TWO_BRIEFED, attach, files=True)
    return [first, cold, briefed]


def summarise(rows: list[Turn], overhead: dict, verdicts: dict) -> dict:
    by_arm: dict[str, dict] = {}
    for arm in ("control", "omnimem"):
        arm_rows = [r for r in rows if r.arm == arm]
        by_arm[arm] = {
            "sessions": {
                r.label: {
                    "turns": r.turns,
                    "input_tokens": r.total_input,
                    "output_tokens": r.output_tokens,
                    "cost_usd": r.cost_usd,
                    "prompt_chars": r.prompt_chars,
                    "duration_s": round(r.duration_ms / 1000, 1),
                    "subagent_stats": r.subagent_stats,
                    "correct": (verdicts.get(f"{arm}:{r.label}") or {}).get("label"),
                }
                for r in arm_rows
            },
            # The driver of the whole comparison: every extra agent turn
            # re-sends the entire context, so turns cost far more than the
            # per-call tool-definition overhead does.
            "agent_turns": sum(r.turns for r in arm_rows),
            "total_input_tokens": sum(r.total_input for r in arm_rows),
            "total_output_tokens": sum(r.output_tokens for r in arm_rows),
            "cache_creation": sum(r.cache_creation_tokens for r in arm_rows),
            "cache_read": sum(r.cache_read_tokens for r in arm_rows),
            "cost_usd": sum(r.cost_usd for r in arm_rows),
            "wall_ms": sum(r.duration_ms for r in arm_rows),
        }
    control, omnimem = by_arm["control"], by_arm["omnimem"]
    return {
        "arms": by_arm,
        "fixed_overhead": overhead,
        "token_delta_omnimem_minus_control": omnimem["total_input_tokens"] - control["total_input_tokens"],
        "cost_delta_usd": omnimem["cost_usd"] - control["cost_usd"],
        "baseline": "control/session2_cold",
        "metric_warning": (
            "Read cost_usd, not turns or input_tokens. Both undercount badly when "
            "an agent delegates: one run showed turns=1 and 26,211 parent tokens "
            "while spawning 2 sub-agents, running 42s and costing $0.0914 against "
            "a control that did the work directly in 7 turns for $0.0187. Parent "
            "token counts exclude sub-agent work; cost_usd includes it."
        ),
        "note": (
            "Totals conflate two different things and should not be read as the "
            "headline. Session 1 is an investment: only the treatment arm records "
            "what it learned, so it is asymmetric by construction. Session 2 is the "
            "payoff, and is the only like-for-like comparison. Compare them "
            "separately."
        ),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=Path("results") / "session.jsonl")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--skip-guess-check", action="store_true",
                        help="skip the guessability gate (not recommended: a guessable "
                             "answer makes every accuracy verdict meaningless)")
    parser.add_argument("--skip-verify", action="store_true",
                        help="skip the tool-callability check (not recommended)")
    args = parser.parse_args()

    if not (BENCH_DIR / ".mcp.json").exists():
        print(f"no .mcp.json in {BENCH_DIR}: see the README on approving the server")
        return 2

    if args.dry_run:
        # Counted rather than guessed, because this figure is what a person
        # reads before agreeing to spend money on a run.
        calls = {
            "guessability_gate": 0 if args.skip_guess_check else 3,
            "tool_callability_check": 0 if args.skip_verify else 1,
            "fixed_overhead_probes": 2,
            "arm_sessions": 2 + 2 + 1,  # both arms: session 1 and cold; omnimem also briefed
            "judge_calls": 3,       # control cold, omnimem cold, omnimem briefed
        }
        total = sum(calls.values())
        print(json.dumps({
            "bench_dir": str(BENCH_DIR), "url": BENCH_URL,
            "calls": calls, "claude_calls": total,
            # ~$0.033 to ~$0.047 measured per CLI call on Haiku, dominated by
            # the CLI's own system prompt rather than by the prompt sent.
            "estimated_usd": round(total * 0.04, 2),
            "fixture": FIXTURE,
            "scenario": "dead-end re-evaluation",
        }, indent=2))
        return 0

    print(f"store check (before): {json.dumps(assert_clean_store())}")
    if not args.skip_guess_check:
        print(f"guessability gate: {json.dumps(assert_answer_not_guessable())}")

    if not args.skip_verify:
        check = verify_tools_callable()
        print(f"tool callability: {json.dumps(check)}")
        if not check.get("callable"):
            print("REFUSING TO RUN: OmniMem's tools are not callable from the CLI.\n"
                  "The treatment arm would measure a model with no memory and still\n"
                  "produce numbers. Check the server is approved and the binary is\n"
                  "patched for protocol 2026-07-28.")
            return 1

    overhead = measure_fixed_overhead()
    print(f"fixed overhead: {overhead['fixed_overhead_tokens']:,.0f} tokens/turn")

    # Again, after the callability check removed its own nonce: if this trips,
    # the cleanup failed and the run would be measuring a polluted store.
    print(f"store check (after cleanup): {json.dumps(assert_clean_store())}")
    rows: list[Turn] = []
    # Control first: it attaches no server and writes nothing, so the store is
    # still pristine when the treatment arm runs.
    for arm in ("control", "omnimem"):
        print(f"running arm: {arm}")
        rows.extend(run_arm(arm))

    panel = judge_mod.build_panel()
    verdicts = {}
    for arm in ("control", "omnimem"):
        for label in SESSION2_VARIANTS:
            answer = next(
                (r.text for r in rows if r.arm == arm and r.label == label), ""
            )
            # Judged against the plain question in both variants, so the gold
            # standard does not shift between them.
            results = panel.judge(TASK_TWO, TASK_TWO_GOLD, answer, "strict")
            verdicts[f"{arm}:{label}"] = results[0].to_dict() if results else {}

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w") as sink:
        for row in rows:
            sink.write(json.dumps(asdict(row)) + "\n")
    report = {
        "metadata": {
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "model": MODEL,
            "bench_url": BENCH_URL,
            "fixture": FIXTURE,
            "scenario": "dead-end re-evaluation",
            "task_two": TASK_TWO,
            "gold": TASK_TWO_GOLD,
        },
        "summary": summarise(rows, overhead, verdicts),
        "verdicts": verdicts,
    }
    summary_path = args.out.with_suffix(".summary.json")
    summary_path.write_text(json.dumps(report, indent=2))
    print(json.dumps(report["summary"], indent=2))
    print(f"\nper-call records: {args.out}\nsummary: {summary_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
