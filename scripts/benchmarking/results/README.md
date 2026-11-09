# Benchmark results

The outputs behind the figures OmniMem quotes. Each run has its own folder, named for the OmniMem version it measured and the date. The harness that produced it, the model, the OmniMem build and its checksum are recorded in the folder's `manifest.json` and `report.md`. A figure in the CHANGELOG or the docs that comes from a benchmark should link to the folder it came from.

| Run | OmniMem | Date | Harness | Agent model | Suites | Headline |
|---|---|---|---|---|---|---|
| [7.0.10-harness-2026-11-09](7.0.10-harness-2026-11-09/report.md) | 7.0.10, the released `omnimem-7.0.10-linux-amd64.tar.gz` (sha256 `0fe80a00…1532e85`) | 9 November 2026 | [omnimem-test-harness](https://code.squarecows.com/ric/omnimem-test-harness) at `de1e6713`, run `test-7.0.10-pairs10` | `claude-haiku-4-5-20251001` (S9 only) | S2, S4, S5, S6, S7, S8, S9 | Dead ends (S9, 10 pairs): the control walked into the dead end in 8 of 10 runs and OmniMem in 0 of 10; median tokens -30.6% (fewer in 8 of 10 pairs, one-sided sign test p = 0.055), median cost -55.2%. Every gate held |

## What each folder holds

- `report.md`: the run as the harness wrote it, with every gate and the notes that qualify it
- `manifest.json`: the OmniMem build (release asset, version, checksum), the harness commit and the host
- `gates.json`: the pass or fail of each gate
- `<suite>.summary.json`: counts and the suite's own figures
- `<suite>.jsonl`: one record per case, so a figure can be checked case by case
- `s9/results.json`: the per-run figures behind the S9 table (tokens, cost, duration, outcome)

Some files stay on the harness's [`results` branch](https://code.squarecows.com/ric/omnimem-test-harness/src/branch/results/results/test-7.0.10-pairs10/2026-11-09/105604), linked rather than copied:

- the agent transcripts for S9 (`s9/*.jsonl.gz`, 1.4 MB);
- the comparison with 7.0.9 (`compare-test-7.0.9.pdf` and `.html`);
- `s4.jsonl`: ForgetEval's case data includes made-up passwords ("The WiFi password is pineapple42!secure"), which the repository's secret scan rightly can't tell from real ones.

## Not here yet

- **LongMemEval.** The earlier LongMemEval runs were made with `run_memory_bench.py` on the development host, and their output wasn't kept in the tree. They need running again (against 7.0.11) before any LongMemEval figure is quoted.
- **The first dead-end runs** (`run_deadend_bench.py`, before the harness had S9). The hook guide quoted them; it now quotes the 7.0.10 run above instead.
