# Embedding benchmark

`scripts/embedding_bench.py` measures ingest and recall end to end against a real valkey-search, using whatever embedding backend `memory/embedder.py` is wired to. It exists so a backend change — the v6.7 swap from sentence-transformers on PyTorch to ONNX Runtime — can be judged on numbers: run it before, run it after, compare.

## Running it

Point it at a throwaway valkey-search, never at production. It refuses a store that already holds memories.

```bash
docker run -d --name omnimem-bench -p 6399:6379 valkey/valkey-extension:latest \
  valkey-server --loadmodule /usr/lib/valkey/libsearch.so \
  --requirepass testpw --notify-keyspace-events AKE

# before the swap
VALKEY_HOST=127.0.0.1 VALKEY_PORT=6399 VALKEY_PASSWORD=testpw \
  python scripts/embedding_bench.py run --out bench/before.json --label torch

# ... swap the backend ...

VALKEY_HOST=127.0.0.1 VALKEY_PORT=6399 VALKEY_PASSWORD=testpw \
  python scripts/embedding_bench.py run --out bench/after.json --label onnx

python scripts/embedding_bench.py compare bench/before.json bench/after.json
docker rm -f omnimem-bench
```

`compare` prints a Markdown table and exits non-zero if it raises a warning, so it can gate a CI job. Add `--out bench/comparison.json` to keep the numbers.

Run both sides in the **same environment** — same machine, same Python, same container image if that is where the swap lives. The local `.venv-test` on a laptop and the Docker image are different backends in all but name.

## What it measures

| Metric | How |
|---|---|
| model load | `Embedder().load()` cold, in milliseconds |
| peak RSS | resident memory of the process after the run (model, corpus, index client) |
| `embed()` p50 / p95 | single-text latency over the first 200 corpus texts, after a warm-up |
| `embed_batch(n)` | texts per second at batch sizes 1, 8 and 32 |
| `remember()` p50 / p95 | the tool end to end in raw mode — embed, dedup check, contradiction heuristic, write |
| `remember_document()` | milliseconds per chunk for a 40-paragraph document |
| `recall()` p50 / p95 | `RecallPipeline.recall()` with a project filter, query expansion off |
| `recall_index()` p50 | the lightweight recall tool |

And the material `compare` uses to decide whether the two backends are actually the same model:

| Check | What it tells you |
|---|---|
| vector cosine | the same 48 texts embedded by both backends; mean and minimum cosine, with counts below 0.999 and 0.99. A faithful ONNX export of the same model sits at 0.999+; anything under 0.99 means the tokeniser, pooling or normalisation differs |
| top-k overlap | the top 10 results per query, matched by content (every run writes fresh keys); mean and minimum Jaccard, how often the top-1 is identical, and score drift |
| vector dimension | flagged if it changes — every stored vector would need re-embedding |

## The corpus

By default a seeded synthetic corpus of 400 memory-shaped texts across the episodic, knowledge and preference namespaces, so a run is reproducible on any machine. `--corpus backups/omnimem-….json` uses the `content` of every memory in a `dump_to_file` backup instead — realistic text, and only the text: nothing in the backup's original store is touched. `--size` caps either, `--queries` sets how many corpus-derived queries to run (four generic ones are always added), `--seed` varies the synthetic corpus.

Everything the run writes goes under the project `embedding-bench` and is deleted afterwards, recall logs included.

## Reading the numbers

Latency and throughput are what the swap is for; equivalence is what it must not break. A result like "3x faster, cosine min 0.9998, top-1 identical 95%" is the swap working. "3x faster, cosine min 0.91" is a different model wearing the same name, and every vector in the store would rank differently against it — that is a re-embed, not a swap.

The suite runs the same functions against the in-memory fakes (`mcp_server/tests/test_embedding_bench.py`), so the harness cannot rot between the before and after runs.
