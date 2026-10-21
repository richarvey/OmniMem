#!/usr/bin/env python3
"""Dead-end benchmark: does OmniMem stop an agent repeating a known failure?

The scenario
------------
Harrier (``~/.omnimem-fixtures/harrier-rs``) is a small Rust workspace with one
open task: implement ``make_limiter`` so the test suite passes. ``TODO.md``
points at ``kestrel-rs`` as the obvious crate. It is a dead end, and only at
runtime: despite the name, ``install_global`` keeps its handle in a
thread-local, Harrier builds each shard's limiter on that shard's own thread,
and every shard but the first fails with ``HandleUnavailable``. Nothing in the
docs or the error names says so. The vendored ``pellham`` works.

The fixture was rewritten after a pilot in which the old version documented the
trap (an error variant named for it, a README line matching the rollout notes):
both arms read the source, went straight to pellham, and memory had nothing to
prevent. A dead end the code already explains is not one memory is needed for.

Both arms get the same prompt, the same tools and a fresh copy of the code.

* **control**: no MCP servers, no hooks. It is expected to try kestrel-rs, run
  the tests, watch them fail, go looking, find pellham and implement that.
* **omnimem**: the OmniMem server plus the PreToolUse hook in
  ``deadend_hook.py``. The store holds the experience of having already been
  down that road. When the agent proposes kestrel-rs, the hook stops the edit
  before it executes and hands back what was abandoned, why, and what worked.

What is measured
----------------
* ``cost_usd`` is the headline. Turns and parent-loop tokens both undercount
  when an agent delegates; cost includes everything. Sub-agents are disabled
  in both arms anyway, so the loops stay comparable.
* Whether the task was actually done: ``cargo test --offline`` is run on the
  agent's workspace afterwards. No LLM judge is involved.
* Whether the pass is honest: the tests and the vendored crates are hashed
  before and after, and a run that edited them is reported as tampered.
* What happened on the way: whether kestrel-rs was proposed, whether the hook
  blocked it, and how many red test runs the agent sat through.

Fairness
--------
* Arms are interleaved (control, omnimem, control, ...) so drift in API
  latency or load lands on both.
* The store is restored from a snapshot before every omnimem run, so nothing a
  run writes can help the run after it.
* ``--strict-mcp-config`` on both arms, so no machine-level MCP server leaks in.
  User-level Claude Code settings still apply, equally, to both arms.
* Before any money is spent the fixture is proven: untouched it fails, the
  kestrel-rs route fails, the pellham route passes. If any of that stops being
  true the benchmark refuses to run, because it would be measuring nothing.

Seeding
-------
By default the store is seeded with a fixed record of the earlier failure, so
runs are reproducible. ``--earn`` instead runs one OmniMem session against an
empty store, lets the agent hit the dead end for real and record it, and uses
whatever it recorded. That is the more honest story and the less reproducible
one; the results file says which was used.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from mcpclient import McpClient  # noqa: E402
from deadend_hook import GUARDED, proposal_text  # noqa: E402

HERE = Path(__file__).resolve().parent
BENCH_DIR = Path(os.environ.get("OMNIMEM_BENCH_DIR", Path.home() / ".omnimem-bench"))
WORK_DIR = BENCH_DIR / "deadend"
FIXTURE = Path(os.environ.get("OMNIMEM_FIXTURE_DIR",
                              Path.home() / ".omnimem-fixtures" / "harrier-rs"))
PORT = int(os.environ.get("OMNIMEM_BENCH_PORT", "8766"))
BENCH_URL = f"http://127.0.0.1:{PORT}"
PYTHON = BENCH_DIR / "venv" / "bin" / "python"
MODEL = os.environ.get("OMNIMEM_BENCH_MODEL", "claude-haiku-4-5-20251001")
PROJECT = "harrier"

# Identical for both arms. It names the task and the definition of done and
# says nothing about memory, tools or earlier attempts.
TASK = (
    "You are working in the Harrier repository (the current directory). "
    "Complete the rate limiting work described in TODO.md: implement "
    "`make_limiter` in `ingestd/src/shard.rs` so that `cargo test --offline` "
    "passes. Do not modify the tests or anything under `vendor/`. When you are "
    "done, reply with the crate you used and one sentence on why."
)

# Appended only for the --earn session, which has to leave a record behind.
EARN_SUFFIX = (
    " When the tests pass, record what you learned in OmniMem for project "
    "'harrier': call remember() with a short account, then record_experience() "
    "on that key, listing any approach you abandoned in abandoned_approaches "
    "with the reason it failed, and what worked as the breakthrough."
)

TOOLS_COMMON = ["Read", "Glob", "Grep", "Edit", "MultiEdit", "Write", "Bash(cargo:*)"]
DISALLOWED = ["WebFetch", "WebSearch", "Task", "Agent"]

# Files an honest pass must leave alone.
PROTECTED = ["ingestd/tests", "vendor", "Cargo.toml"]

# The fixed record used when not earning it. Written as the agent would have.
SEED_CONTENT = (
    "Implemented rate limiting on the Harrier ingest path. Tried kestrel-rs first "
    "because TODO.md names it as the obvious candidate. Despite its name, "
    "Governor::install_global stores the handle thread-locally, so only the shard "
    "thread that installed it can construct a limiter; every other shard failed "
    "with HandleUnavailable and the isolation tests went red. Harrier builds each "
    "shard's limiter on that shard's own thread, so kestrel-rs cannot work here. "
    "Switched to pellham, Limiter::for_partition per shard with no shared "
    "handle, and the suite passed."
)
SEED_EXPERIENCE = {
    "effort_score": 4,
    "outcome": "pivoted",
    "iterations": 6,
    "abandoned_approaches": [{
        "name": "kestrel-rs",
        "type": "library",
        "reason": ("install_global keeps its handle thread-locally, so every "
                   "shard thread except the installing one fails with "
                   "HandleUnavailable"),
    }],
    "breakthrough": "pellham: Limiter::for_partition(shard_id, capacity), one per shard, no shared handle",
    "lesson": "a limiter whose handle lives on one thread cannot serve shards that each run on their own thread",
}

# Reference implementations, used only to prove the fixture before a run.
IMPL_KESTREL = '''pub fn make_limiter(_shard_id: u64, capacity: usize) -> Box<dyn RateLimiter> {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        kestrel_rs::Governor::install_global(kestrel_rs::Config::default())
            .expect("governor installs once");
    });
    struct K(kestrel_rs::Limiter);
    impl RateLimiter for K {
        fn try_acquire(&self) -> bool { self.0.try_acquire() }
        fn refill(&self) { self.0.refill() }
    }
    Box::new(K(kestrel_rs::Limiter::new(capacity).expect("limiter")))
}
'''
IMPL_PELLHAM = '''pub fn make_limiter(shard_id: u64, capacity: usize) -> Box<dyn RateLimiter> {
    struct P(pellham::Limiter);
    impl RateLimiter for P {
        fn try_acquire(&self) -> bool { self.0.try_acquire() }
        fn refill(&self) { self.0.refill() }
    }
    Box::new(P(pellham::Limiter::for_partition(shard_id, capacity)))
}
'''

DENY_MARKER = "Stopped by project memory"
DEADEND_TOKENS = ("kestrel_rs", "kestrel-rs")
FIXED_TOKENS = ("pellham",)


# --------------------------------------------------------------------------- #
# Server control
# --------------------------------------------------------------------------- #

class Server:
    """The bench OmniMem instance, started and stopped around store snapshots."""

    def __init__(self, binary: Path, data_dir: Path) -> None:
        self.binary = binary
        self.data_dir = data_dir
        self.db = data_dir / "omnimem.db"
        self.log = WORK_DIR / "server.log"
        self.proc: subprocess.Popen | None = None

    def _port_open(self) -> bool:
        with socket.socket() as s:
            s.settimeout(0.3)
            return s.connect_ex(("127.0.0.1", PORT)) == 0

    def _kill_listener(self) -> None:
        out = subprocess.run(["ss", "-ltnp"], capture_output=True, text=True).stdout
        for line in out.splitlines():
            if f":{PORT} " in line and "pid=" in line:
                pid = int(line.split("pid=")[1].split(",")[0])
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.proc = None
        self._kill_listener()
        for _ in range(50):
            if not self._port_open():
                return
            time.sleep(0.1)
        raise RuntimeError(f"port {PORT} still in use after stopping the server")

    def start(self) -> None:
        env = dict(os.environ, OMNIMEM_DB=str(self.db), MCP_HOST="127.0.0.1",
                   MCP_PORT=str(PORT), RSS_SCHEDULE_HOURS="0",
                   FEEDS_CONFIG_PATH=str(self.data_dir / "no-feeds.yml"))
        self.proc = subprocess.Popen(
            [str(self.binary), "serve"], cwd=BENCH_DIR, env=env,
            stdin=subprocess.DEVNULL, stdout=self.log.open("a"), stderr=subprocess.STDOUT,
            start_new_session=True)
        for _ in range(150):
            if self._port_open():
                return
            if self.proc.poll() is not None:
                break
            time.sleep(0.2)
        raise RuntimeError(f"server did not start; see {self.log}")

    def wipe(self) -> None:
        self.stop()
        for suffix in ("", "-wal", "-shm"):
            Path(f"{self.db}{suffix}").unlink(missing_ok=True)
        self.start()

    def snapshot(self, dest: Path) -> None:
        self.stop()  # a clean shutdown checkpoints the WAL into the main file
        dest.mkdir(parents=True, exist_ok=True)
        for suffix in ("", "-wal", "-shm"):
            src = Path(f"{self.db}{suffix}")
            if src.exists():
                shutil.copy2(src, dest / f"omnimem.db{suffix}")
        self.start()

    def restore(self, src: Path) -> None:
        self.stop()
        for suffix in ("", "-wal", "-shm"):
            Path(f"{self.db}{suffix}").unlink(missing_ok=True)
            snap = src / f"omnimem.db{suffix}"
            if snap.exists():
                shutil.copy2(snap, Path(f"{self.db}{suffix}"))
        self.start()


def wait_for_queue(client: McpClient, seconds: float = 30.0) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if int(client.call_ok("queue_status").json().get("pending", 0) or 0) <= 0:
            return
        time.sleep(0.25)


def graveyard_names(client: McpClient) -> list[str]:
    """The abandoned approaches the store would warn about for this scenario."""
    result = client.call_ok("warn_if_abandoned", {"query": "kestrel-rs"}).json()
    if not isinstance(result, dict) or result.get("status") != "warning":
        return []
    return [str(m.get("abandoned_name")) for m in result.get("matches") or []]


# --------------------------------------------------------------------------- #
# Workspace and oracle
# --------------------------------------------------------------------------- #

def fresh_workspace(label: str, root: Path) -> Path:
    path = root / label
    shutil.copytree(FIXTURE, path, ignore=shutil.ignore_patterns("target"))
    return path


def tree_hash(workspace: Path) -> str:
    digest = hashlib.sha256()
    for rel in PROTECTED:
        target = workspace / rel
        files = [target] if target.is_file() else sorted(
            p for p in target.rglob("*") if p.is_file() and "target" not in p.parts)
        for f in files:
            digest.update(str(f.relative_to(workspace)).encode())
            digest.update(f.read_bytes())
    return digest.hexdigest()


def cargo_test(workspace: Path, timeout: float = 300.0) -> tuple[bool, str]:
    try:
        proc = subprocess.run(["cargo", "test", "--offline"], cwd=workspace,
                              capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return False, "timeout"
    tail = (proc.stdout + proc.stderr)[-1500:]
    return proc.returncode == 0, tail


def crate_used(workspace: Path) -> str:
    """Which limiter make_limiter ended up built on, ignoring comments."""
    src = (workspace / "ingestd/src/shard.rs").read_text()
    code = "\n".join(line.split("//")[0] for line in src.splitlines())
    start = code.find("fn make_limiter")
    end = code.find("\n}", start)
    body = code[start:end] if start >= 0 else ""
    uses_k = any(t in body for t in DEADEND_TOKENS)
    uses_p = any(t in body for t in FIXED_TOKENS)
    if "todo!" in body:
        return "unimplemented"
    return {(True, False): "kestrel-rs", (False, True): "pellham",
            (True, True): "both"}.get((uses_k, uses_p), "other")


def with_impl(workspace: Path, impl: str, crate: str) -> None:
    manifest = workspace / "ingestd/Cargo.toml"
    manifest.write_text(manifest.read_text().rstrip("\n")
                        + f'\n{crate} = {{ path = "../vendor/{crate}" }}\n')
    path = workspace / "ingestd/src/shard.rs"
    src = path.read_text()
    start = src.index("/// Build the limiter for one shard.")
    end = src.index("/// Mark this thread as driving")
    path.write_text(src[:start] + impl + "\n" + src[end:])


def prove_fixture(root: Path) -> dict:
    """Refuse to benchmark a scenario that has stopped being true."""
    results = {}
    for label, impl, want_pass in (("untouched", None, False),
                                   ("kestrel-rs", IMPL_KESTREL, False),
                                   ("pellham", IMPL_PELLHAM, True)):
        ws = fresh_workspace(f"proof-{label}", root)
        if impl:
            with_impl(ws, impl, label)
        passed, tail = cargo_test(ws)
        results[label] = passed
        if passed != want_pass:
            raise SystemExit(
                f"FIXTURE PROOF FAILED: the {label} route "
                f"{'passed' if passed else 'failed'} but should have "
                f"{'passed' if want_pass else 'failed'}.\n{tail}")
    return results


# --------------------------------------------------------------------------- #
# One agent run
# --------------------------------------------------------------------------- #

@dataclass
class Run:
    arm: str
    index: int
    workspace: str
    cost_usd: float = 0.0
    duration_s: float = 0.0
    turns: int = 0
    input_tokens: int = 0
    output_tokens: int = 0
    cache_read_tokens: int = 0
    cache_creation_tokens: int = 0
    tool_calls: dict = field(default_factory=dict)
    deadend_proposed: int = 0     # edits/commands that reached for kestrel-rs
    deadend_blocked: int = 0      # of those, stopped by the hook
    red_test_runs: int = 0        # cargo test runs that came back failing
    compile_errors: int = 0       # cargo runs that did not compile
    memory_calls: int = 0
    passed: bool = False
    tampered: bool = False
    crate: str = ""
    answer: str = ""
    error: str = ""

    @property
    def total_tokens(self) -> int:
        return (self.input_tokens + self.output_tokens
                + self.cache_read_tokens + self.cache_creation_tokens)


def _text_of(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(_text_of(c.get("text", c.get("content", ""))) if isinstance(c, dict)
                         else str(c) for c in content)
    return str(content or "")


def parse_stream(lines: list[str], run: Run) -> None:
    """Read what the agent did from the stream-json transcript."""
    uses: dict[str, dict] = {}
    for raw in lines:
        try:
            event = json.loads(raw)
        except json.JSONDecodeError:
            continue
        kind = event.get("type")
        if kind == "assistant":
            for block in event.get("message", {}).get("content", []) or []:
                if block.get("type") != "tool_use":
                    continue
                name = block.get("name", "")
                run.tool_calls[name] = run.tool_calls.get(name, 0) + 1
                if name.startswith("mcp__"):
                    run.memory_calls += 1
                uses[block.get("id", "")] = block
                # The hook's own extraction, so the count and the interception
                # cannot disagree about what a proposal is. Reading about
                # kestrel-rs is investigation; writing or adding it is proposing.
                proposed = proposal_text(name, block.get("input") or {}) if name in GUARDED else ""
                if any(t in proposed for t in DEADEND_TOKENS):
                    run.deadend_proposed += 1
        elif kind == "user":
            for block in event.get("message", {}).get("content", []) or []:
                if block.get("type") != "tool_result":
                    continue
                text = _text_of(block.get("content"))
                use = uses.get(block.get("tool_use_id", ""), {})
                if DENY_MARKER in text:
                    run.deadend_blocked += 1
                if use.get("name") == "Bash" and "cargo" in json.dumps(use.get("input", {})):
                    if "error[E" in text or "could not compile" in text:
                        run.compile_errors += 1
                    elif ("test result: FAILED" in text or "panicked" in text) and \
                            "rate limiting is the blocker" not in text:
                        # The untouched todo!() panics too; that is a baseline
                        # check, not a failed attempt.
                        run.red_test_runs += 1
        elif kind == "result":
            usage = event.get("usage", {}) or {}
            run.cost_usd = float(event.get("total_cost_usd") or 0.0)
            run.turns = int(event.get("num_turns") or 0)
            run.input_tokens = int(usage.get("input_tokens", 0))
            run.output_tokens = int(usage.get("output_tokens", 0))
            run.cache_read_tokens = int(usage.get("cache_read_input_tokens", 0))
            run.cache_creation_tokens = int(usage.get("cache_creation_input_tokens", 0))
            run.answer = str(event.get("result", "")).strip()[:600]
            if event.get("is_error"):
                run.error = run.error or str(event.get("subtype", "error"))


def run_agent(arm: str, index: int, root: Path, prompt: str, timeout: float) -> Run:
    ws = fresh_workspace(f"{arm}-{index}", root)
    run = Run(arm=arm, index=index, workspace=str(ws))
    before = tree_hash(ws)

    cmd = ["claude", "-p", prompt, "--model", MODEL,
           "--output-format", "stream-json", "--verbose",
           "--permission-mode", "acceptEdits",
           "--disallowed-tools", ",".join(DISALLOWED),
           "--strict-mcp-config"]
    if arm == "control":
        cmd += ["--mcp-config", '{"mcpServers":{}}',
                "--allowed-tools", ",".join(TOOLS_COMMON)]
    else:
        cmd += ["--mcp-config", str(WORK_DIR / "mcp-omnimem.json"),
                "--settings", str(WORK_DIR / "settings-omnimem.json"),
                "--allowed-tools", ",".join(TOOLS_COMMON + ["mcp__omnimem-bench"])]

    transcript = root / f"{arm}-{index}.jsonl"
    started = time.monotonic()
    try:
        with transcript.open("w") as out:
            proc = subprocess.run(cmd, cwd=ws, stdin=subprocess.DEVNULL, stdout=out,
                                  stderr=subprocess.PIPE, text=True, timeout=timeout)
        if proc.returncode != 0:
            run.error = proc.stderr[-300:] or f"exit {proc.returncode}"
    except subprocess.TimeoutExpired:
        run.error = f"timeout after {timeout:.0f}s"
    run.duration_s = time.monotonic() - started
    parse_stream(transcript.read_text().splitlines(), run)

    run.tampered = tree_hash(ws) != before
    run.passed, _ = cargo_test(ws)
    run.crate = crate_used(ws)
    return run


# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #

def write_configs(binary: Path, db: Path) -> None:
    """Point the arm at the server and at the shipped `omnimem hook`.

    The hook is the binary's own subcommand, not the Python reference in
    deadend_hook.py, so a run exercises what actually ships. The two were
    checked against the same payloads and agree; the Python one is kept
    because it is what produced the recorded runs, and as a second opinion.
    """
    WORK_DIR.mkdir(parents=True, exist_ok=True)
    (WORK_DIR / "mcp-omnimem.json").write_text(json.dumps(
        {"mcpServers": {"omnimem-bench": {"type": "http", "url": f"{BENCH_URL}/mcp"}}}))
    hook = f"{binary} --db {db} hook"
    (WORK_DIR / "settings-omnimem.json").write_text(json.dumps({"hooks": {"PreToolUse": [{
        "matcher": "Edit|MultiEdit|Write|Bash",
        "hooks": [{"type": "command", "command": hook, "timeout": 15}]}]}}))


def summarise(runs: list[Run]) -> dict:
    def med(values):
        return round(statistics.median(values), 4) if values else None

    arms = {}
    for arm in ("control", "omnimem"):
        rs = [r for r in runs if r.arm == arm]
        honest = [r for r in rs if r.passed and not r.tampered and not r.error]
        arms[arm] = {
            "runs": len(rs),
            "passed_honestly": len(honest),
            "tampered": sum(r.tampered for r in rs),
            "errors": sum(bool(r.error) for r in rs),
            "median_cost_usd": med([r.cost_usd for r in rs]),
            "total_cost_usd": round(sum(r.cost_usd for r in rs), 4),
            "median_duration_s": med([r.duration_s for r in rs]),
            "median_turns": med([r.turns for r in rs]),
            "median_total_tokens": med([r.total_tokens for r in rs]),
            "runs_that_proposed_deadend": sum(r.deadend_proposed > 0 for r in rs),
            "runs_where_deadend_was_blocked": sum(r.deadend_blocked > 0 for r in rs),
            "runs_with_red_tests": sum(r.red_test_runs > 0 for r in rs),
            "median_red_test_runs": med([r.red_test_runs for r in rs]),
            "crates": {c: sum(r.crate == c for r in rs) for c in {r.crate for r in rs}},
        }
    c, o = arms["control"], arms["omnimem"]
    delta = None
    if c["median_cost_usd"] and o["median_cost_usd"] is not None:
        delta = round(100 * (o["median_cost_usd"] - c["median_cost_usd"]) / c["median_cost_usd"], 1)
    return {"arms": arms, "median_cost_change_pct": delta}


def print_table(runs: list[Run], summary: dict) -> None:
    print(f"\n{'run':<11}{'cost':>8}{'secs':>7}{'turns':>6}{'tokens':>9}"
          f"{'proposed':>9}{'blocked':>8}{'red':>5}  {'crate':<13}result")
    for r in runs:
        verdict = ("ERROR " + r.error[:40]) if r.error else (
            "TAMPERED" if r.tampered else ("pass" if r.passed else "FAIL"))
        print(f"{r.arm + '-' + str(r.index):<11}{r.cost_usd:>8.4f}{r.duration_s:>7.0f}"
              f"{r.turns:>6}{r.total_tokens:>9}{r.deadend_proposed:>9}"
              f"{r.deadend_blocked:>8}{r.red_test_runs:>5}  {r.crate:<13}{verdict}")
    print()
    for arm, s in summary["arms"].items():
        print(f"{arm:>8}: {s['passed_honestly']}/{s['runs']} honest passes, "
              f"median ${s['median_cost_usd']}, {s['median_duration_s']}s, "
              f"{s['median_turns']} turns; proposed dead end in "
              f"{s['runs_that_proposed_deadend']}, blocked in "
              f"{s['runs_where_deadend_was_blocked']}, red tests in {s['runs_with_red_tests']}")
    if summary["median_cost_change_pct"] is not None:
        print(f"\nmedian cost, omnimem vs control: {summary['median_cost_change_pct']:+}%")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--runs", type=int, default=3, help="runs per arm (default 3)")
    ap.add_argument("--binary", type=Path, default=BENCH_DIR / "omnimem")
    ap.add_argument("--earn", action="store_true",
                    help="earn the memory in a real session instead of seeding it")
    ap.add_argument("--timeout", type=float, default=900.0, help="seconds per agent run")
    ap.add_argument("--dry-run", action="store_true",
                    help="prove the fixture and prepare the store, spend nothing")
    args = ap.parse_args()

    write_configs(args.binary, BENCH_DIR / "data" / "omnimem.db")
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    root = WORK_DIR / f"run-{stamp}"
    root.mkdir(parents=True)
    print(f"workspaces and transcripts: {root}")

    print("proving the fixture ...")
    proof = prove_fixture(root)
    print(f"  untouched fails, kestrel-rs fails, pellham passes: {proof}")

    server = Server(args.binary, BENCH_DIR / "data")
    server.wipe()
    seed: dict = {"mode": "earned" if args.earn else "recorded"}
    earn_run = None
    if args.earn:
        print("earning the memory in a real session (empty store) ...")
        earn_run = run_agent("earn", 0, root, TASK + EARN_SUFFIX, args.timeout)
        with McpClient(BENCH_URL) as client:
            wait_for_queue(client)
            names = graveyard_names(client)
        seed["earn_run"] = asdict(earn_run)
        if not any(n.lower().replace("_", "-") == "kestrel-rs" for n in names):
            print(f"  the earning session recorded no kestrel-rs dead end "
                  f"(graveyard: {names}; crate {earn_run.crate}, "
                  f"proposed {earn_run.deadend_proposed}). Nothing to benchmark.")
            (root / "results.json").write_text(json.dumps(
                {"aborted": "earn session left no dead end", "seed": seed}, indent=2))
            server.stop()
            return 2
    else:
        with McpClient(BENCH_URL) as client:
            key = client.call_ok("remember", {"content": SEED_CONTENT, "project": PROJECT,
                                              "namespace": "episodic"}).json()["key"]
            client.call_ok("record_experience", {"key": key, **SEED_EXPERIENCE})
            wait_for_queue(client)
            names = graveyard_names(client)
    seed["graveyard"] = names
    print(f"  store holds dead ends: {names}")
    snapshot = root / "store-snapshot"
    server.snapshot(snapshot)

    if args.dry_run:
        server.stop()
        print("dry run: fixture proven and store prepared; no agent runs")
        return 0

    runs: list[Run] = []
    for i in range(1, args.runs + 1):
        for arm in ("control", "omnimem"):
            if arm == "omnimem":
                server.restore(snapshot)
            print(f"running {arm} {i}/{args.runs} ...", flush=True)
            run = run_agent(arm, i, root, TASK, args.timeout)
            runs.append(run)
            print(f"  ${run.cost_usd:.4f}, {run.duration_s:.0f}s, crate={run.crate}, "
                  f"passed={run.passed}, proposed={run.deadend_proposed}, "
                  f"blocked={run.deadend_blocked}, red={run.red_test_runs}"
                  + (f", error={run.error[:80]}" if run.error else ""), flush=True)
    server.stop()

    summary = summarise(runs)
    results = {
        "benchmark": "dead-end interception",
        "started": stamp,
        "model": MODEL,
        "binary": str(args.binary),
        "fixture": str(FIXTURE),
        "task": TASK,
        "fixture_proof": proof,
        "seed": seed,
        "summary": summary,
        "runs": [asdict(r) | {"total_tokens": r.total_tokens} for r in runs],
    }
    out = root / "results.json"
    out.write_text(json.dumps(results, indent=2))
    print_table(runs, summary)
    print(f"\nresults: {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
