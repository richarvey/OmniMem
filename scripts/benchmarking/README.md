# OmniMem benchmarking

A benchmark harness that drives OmniMem 6.x and 7.x through the same MCP
client, scores them on published memory benchmarks, and produces a PDF report
intended to survive a sceptical reader.

## Why this exists, and why it is built this way

The obvious competitor benchmark is mem0's. Reading their published material
against their published code turned up several problems worth designing around:

* Their **current harness contains no competitor adapters at all**. It can only
  benchmark mem0, so their comparative claims cannot be reproduced from it.
* It **emits no token counts and computes no latency percentiles**, while the
  marketing quotes both. Their own committed result file gives a search latency
  p50 of about 3,054ms against a published claim of "+1ms median".
* Their README claims **92.5% (1425/1540)** on LoCoMo while the committed
  results for that run give **91.56% (1410/1540)**, and the result metadata
  lists roughly 160 question IDs merged in from a separate run.
* The jump from about 67% (paper) to about 92% (current) is substantially
  **rubric, not system**: the newer rubric awards full credit for one correct
  item out of several, tolerates dates 14 days out, and truncates open-domain
  gold answers at the first semicolon.

So this harness does the opposite of each: it publishes per-question records,
measures tokens and latency including ingest, carries both rubrics and reports
them side by side, and prints its answer prompt in full.

> **Provenance of the claims above.** They were established on 2026-09-17 from
> mem0's public material: arXiv 2504.19413, `github.com/mem0ai/memory-benchmarks`
> (README, `benchmarks/locomo/prompts.py`, `benchmarks/common/metrics.py`, and
> the committed `results/platform/locomo_results.json`), and mem0.ai/research.
> They have **not** been independently re-verified since, and they describe a
> moving target: a repository can change the day after it is read. Treat them as
> the design rationale for this harness, which is all they are needed for. Do
> **not** put any of them into published marketing until they have been checked
> again against those primary sources, with the retrieval date stated.

## Licensing, which constrains the dataset choice

`LOCOMO` is **not used here.** Its licence file states CC BY-NC 4.0, which is
NonCommercial, and the output of this harness is intended for commercial use.
Anyone publishing LOCOMO-derived benchmark marketing carries that exposure.

| Dataset | Licence | Used |
|---|---|---|
| LongMemEval | MIT | yes |
| BEAM | CC BY-SA 4.0 | planned |
| LOCOMO | CC BY-NC 4.0 | deliberately excluded |

## Safety rails

**A production OmniMem stack runs on this host.** Containers occupy ports 8765
and 8080, and a production OmniMem MCP server is reachable over Tailscale.
Benchmarking must never touch it, so:

* Instances bind **8766** (v7), **8767** (v6) and **6399** (throwaway Valkey),
  clear of production.
* `assert_empty()` refuses to write unless the store reports **zero records**,
  so a misdirected connection aborts rather than polluting real memories.
* Every `claude` invocation passes **`--strict-mcp-config`**, so only the MCP
  servers this harness specifies are loaded. Without it, Claude would attach
  the real OmniMem and the benchmark would read and write production data.
* `ANTHROPIC_API_KEY` is stripped from server environments, because fact
  extraction and query expansion call out to an LLM when it is present and
  would make timings and results non-deterministic.
* Spawned `claude` agents never get blanket permissions. Note that
  `--allowed-tools` is **not** an exclusive allowlist: an agent given
  `--allowed-tools Read,Glob,Grep` was measured executing Bash with zero
  permission denials. Do not describe an arm as unable to reach the filesystem
  unless the enforcement has been tested.
* Judge and answerer calls run from an **empty scratch directory**, so they
  cannot inherit the project's CLAUDE.md or its file-based memory
  instructions. One early call decided its task was to record a memory and
  wrote a file into the operator's real memory directory.

## Setup

```bash
# Python environment. Do NOT install torch: since v6.7 the default embedding
# backend is ONNX and neither torch nor sentence-transformers is required.
uv venv .venv-bench --python 3.13
VIRTUAL_ENV=.venv-bench uv pip install -r mcp_server/requirements.txt httpx

# The v7 binary under test (needs Rust 1.94 or newer)
cargo build --release -p omnimem        # -p omnimem skips the GTK desktop crate

# The dataset, roughly 265MB, into a git-ignored directory
mkdir -p data/benchmarks
curl -L -o data/benchmarks/longmemeval_s_cleaned.json \
  https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_s_cleaned.json
```

| Variable | Purpose |
|---|---|
| `OMNIMEM_V7_BIN` | path to the built v7 binary, required for v7 runs |
| `OMNIMEM_V6_PYTHON` | interpreter with the v6 requirements installed |
| `OMNIMEM_BENCH_TMP` | scratch directory for databases and server logs |
| `OMNIMEM_BENCH_DATA` | dataset directory, defaults to `data/benchmarks` |
| `PW_EXEC` | Chromium path for PDF rendering, if not on the default |
| `OPENAI_API_KEY` | enables the second judge; without it the panel runs single-judge |

## Modules

| File | Role |
|---|---|
| `mcpclient.py` | minimal streamable-HTTP MCP client, drives both versions identically |
| `instances.py` | start, reset and stop throwaway v6 and v7 instances |
| `corpus.py` | dataset loading, stratified sampling, conversion to memories |
| `answerer.py` | the neutral answer prompt and the answer baselines |
| `judge.py` | both rubrics, the judges, and their agreement rate |
| `metrics.py` | set-based F1, BLEU-1, percentiles, aggregation |
| `run_memory_bench.py` | the benchmark itself |
| `run_session_bench.py` | the with and without OmniMem token comparison |
| `report.py` | HTML to PDF report |

## Running it

```bash
cd scripts/benchmarking

# Always dry run first: prints the plan and the cost, spends nothing
python run_memory_bench.py --version v7 --size 60 --dry-run

python run_memory_bench.py --version v7 --size 60 --out results/v7-lme.jsonl
python run_memory_bench.py --version v6 --size 60 --out results/v6-lme.jsonl

python report.py --summary results/v7-lme.summary.json \
                 --summary results/v6-lme.summary.json \
                 --out reports/omnimem-benchmark.pdf
```

Per-question records stream to JSONL as they are produced, so a run that dies
part way loses nothing and `--resume` continues it. Those records are meant to
be committed: a benchmark nobody can audit is marketing.

### Sampling is not optional

Every LongMemEval question carries its own haystack of roughly 48 sessions and
122,000 tokens, and those haystacks must not contaminate each other, so each
question gets a **fresh empty store**. A full 500-question run therefore means
ingesting about **61 million tokens across 500 separate stores**, which is an
overnight job. The default is a stratified sample across all six categories
with a recorded seed, and the report states both.

### Cost

Answer and judge calls go through the `claude` CLI, which carries roughly
18,000 tokens of system prompt on every invocation regardless of prompt size,
so cost tracks the **number of calls** far more than their size. Measured:
about **$0.033** per judge call and **$0.047** per answer call on Haiku, so a
60-question run with two rubrics lands near **$30**. An API key would cut this
substantially by removing the CLI preamble.

## Measured so far

From validation runs on this host, v7 built from `v7.0.x`:

| | v7 (7.0.0-dev) | v6 (6.7.1) |
|---|---|---|
| remember | 43ms | 17 to 26ms |
| recall | 44ms | 14 to 26ms |
| cold start to ready | 0.3s (median of 3) | 2 to 3s including model load |

Both versions expose the **same 48 tool names**, which is what lets one client
drive both. On identical content and query they scored a recall 0.86239034
against 0.86239010, differing only in float accumulation order.

### Session comparison: results withdrawn

The dead-end scenario has been rebuilt and its earlier results are **void**.

The first fixture named its crates `tollgate-rs` and `shardgate`. Asked the
scenario question with **no files, no memory and no tools**, the model named
`shardgate` **3 times out of 3**, and reconstructed the per-shard isolation
constraint, the 40 production sites and TimescaleDB from priors alone. The
name telegraphed the answer: "shardgate" reads as "the shard-aware one".

So every accuracy verdict across four benchmark runs was meaningless. A
correct answer proved nothing, because the answer was reachable without
consulting anything. That includes the runs where OmniMem looked good.

Two things changed as a result:

* The fixture now uses semantically neutral names (`kestrel-rs` for the dead
  end, `pellham` for the answer), so neither hints at partitioning.
* `assert_answer_not_guessable()` runs **before any measurement** and aborts
  the run if the answer can be produced without tools. It costs 3 calls. It
  should have existed before the first run; it did not, and nothing else in
  the harness noticed for four runs.

What survives from those runs is the mechanical measurement, which does not
depend on the answer being unguessable: per-turn overhead, turn counts and
token totals. What does not survive is every statement about correctness.

## Dead-end interception (`run_deadend_bench.py`)

The scenario the other benchmarks could not measure: does memory stop an agent
repeating work that has already failed?

The fixture (`~/.omnimem-fixtures/harrier-rs`) is a small Rust workspace with
one open task: implement `make_limiter` so the suite passes. `TODO.md`
recommends `kestrel-rs`. Despite its name, that crate's `install_global` keeps
its handle in a thread-local; Harrier builds each shard's limiter on that
shard's own thread, so every shard but the first fails. The vendored `pellham`
works. Nothing in the docs or the error names gives this away: the only way to
find out is to run the tests.

Both arms get the same prompt, the same tools, and a fresh copy of the code.
The control has no MCP server and no hooks. The treated arm has the server plus
`omnimem hook` as a PreToolUse hook, with the earlier failure in the store.

There is **no LLM judge in this benchmark**. `cargo test` decides whether the
task was done, and the tests and vendored crates are hashed before and after,
so a run that made the suite pass by editing the tests is reported as tampered.
Before any money is spent the scenario is proven: untouched code must fail, the
kestrel-rs route must fail, the pellham route must pass, or the run aborts.

### Results: 10 pairs, Haiku 4.5, alternating arms

| | control | omnimem |
|---|---|---|
| passed honestly | 10/10 | 10/10 |
| mean cost | $0.1583 | $0.1153 (-27%) |
| mean tokens | 677,634 | 523,371 (-23%) |
| mean wall time | 94s | 61s (-35%) |
| runs that reached a failing suite | 8/10 | **0/10** |
| dead-end proposals blocked | 0 of 8 | **7 of 7** |

Cheaper in **all ten pairs** (sign test p = 0.002), from -4.6% to -54%. Within
the control arm alone, the eight runs that walked into the dead end cost 27%
more than the two that did not, which is the same figure reached from the other
direction.

### What these numbers are not

* **One scenario, one model, a shallow dead end.** The control escaped in one
  or two failed test runs. A dead end that takes longer to find is worth more,
  and this data does not show that.
* **This measures the hook, not recall.** Across all twenty runs the agent made
  **zero** memory calls. The server's instruction to call `briefing()` at
  session start was ignored every time. Without the hook the treated arm would
  have behaved like the control.
* **An earlier fixture measured nothing.** It documented the trap in an error
  variant name and a README line that echoed the rollout notes, so both arms
  read it and went straight to the answer: the control never hit the dead end
  and the treated arm had nothing to prevent. A dead end the code explains is
  not one memory is needed for. That version's numbers (-3%) are void.

## A v7 bug this harness found

While wiring up the session benchmark, the harness turned up a shipping blocker
that has nothing to do with benchmarking.

**OmniMem v7 is invisible to Claude Code.** The server connects, the tool list
is rejected, and the user sees a connected server with zero tools. The only
symptom is a small `tools fetch failed` note, which reads like the user's own
misconfiguration. v6 is unaffected, which is why it went unnoticed.

Established from the wire, using a raw TCP tee between the client and the
server, after the model's own account of the failure proved wrong twice:

1. rmcp advertises protocol `2026-07-28` although its `LATEST` is `2025-11-25`,
   so Claude Code negotiates the newer revision.
2. That revision requires `ttlMs` and `cacheScope` on paginated results
   (SEP-2549). rmcp declares them `Option` with `skip_serializing_if`, so unset
   means omitted from the wire.
3. `ListToolsResult::with_all_items()` sets both to `None`, and v7's
   `list_tools` calls exactly that.
4. The client validates strictly and rejects the entire response.

**rmcp 3.4.0 does not fix this**, so a version bump is not the remedy. The fix
is one expression in `crates/omnimem-mcp/src/handler.rs`, gated on the
negotiated version exactly as rmcp's own `#[tool_handler]` macro does it:

```rust
let supports_cache_hints = context.protocol_version()
    .is_some_and(|v| v >= ProtocolVersion::V_2026_07_28);
// when true: .with_ttl_ms(0).with_cache_scope(CacheScope::Public)
```

Note `list_tools` currently discards its context as `_context`. Verified in a
throwaway worktree: Claude Code then reports connected, lists all 48 tools, and
a `remember` call it makes lands in the store. **`v7.0.x` still has the bug.**

## Known gaps

* **The session comparison is one scenario, n=1.** Eight facts, two sessions,
  one model. The crossover it implies is an extrapolation from a single run and
  should not be quoted as a measurement until several scenarios of differing
  size have been run.
* **Two prerequisites, both strict.** The `.mcp.json` must have been approved
  once interactively, because Claude Code silently refuses unapproved MCP
  servers. And the v7 binary must carry the fix below, or the treatment arm
  measures a model with no memory at all while still producing plausible
  numbers. The harness verifies tool callability by side effect before it
  measures anything, and refuses to run otherwise.
* **The second judge is inactive** without `OPENAI_API_KEY`, so the dual-judge
  design currently runs single-judge. The panel reports this honestly rather
  than fabricating an agreement rate.
* **F1 and BLEU are not accuracy on LongMemEval.** Gold answers are terse
  ("$65") while correct answers are verbose, so a fully correct answer
  routinely scores below 0.25. Validation measured F1 0.198 and BLEU-1 0.125
  against judge accuracy of 1.0. They are retained only as a divergence check
  and the report labels them as such.
* **BEAM is not downloaded and has no loader yet.** Only its file sizes have been
  checked: 5.4MB, 34MB and 66MB for the 100K, 500K and 1M splits. The 10M split
  lives in a separate repository.
