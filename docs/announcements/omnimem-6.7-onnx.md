# OmniMem 6.7: the same embeddings, a third of the time, and 1.4 GB less image

OmniMem 6.7 swaps the embedding engine from sentence-transformers on PyTorch to ONNX Runtime. Nothing about the model changed. all-MiniLM-L6-v2 still turns every memory and every query into the same 384 numbers it did last week. What changed is how those numbers get computed, and the difference is large enough that it deserves its own post.

The short version: recall is a third faster, storing a memory is twice as fast, the server starts in under a second instead of several, resident memory dropped by two thirds, and the MCP server image went from 2.04 GB to 634 MB. Your stored vectors are the vectors 6.7 produces, to within floating-point noise. Nothing needs re-embedding. There is nothing to migrate.

## The problem we were solving

Every OmniMem image carried PyTorch. Not because OmniMem does any training, or any GPU work, or anything PyTorch is actually for. It carried PyTorch because sentence-transformers is built on it, and sentence-transformers is the easy way to run a sentence embedding model in Python.

That easy way cost about 1.4 GB per image, across three images. It ruled out Alpine as a base (PyTorch publishes no musl wheels, and yes, we tried the gcompat shim). It made a Raspberry Pi build a swap-file exercise. And it meant every container start paid for importing a full autograd framework, then loading a model through it, before a single memory could be stored.

For a tool whose whole job is "embed one memory, embed one query, search", that is a lot of machinery for a fixed 22 million parameter graph.

## What 6.7 does instead

The model's maintainers publish an ONNX export of all-MiniLM-L6-v2 in the same Hugging Face repo as the PyTorch weights. ONNX is just the graph: the layers, the shapes, the weights. No framework. OmniMem now downloads that file, opens it with ONNX Runtime, tokenises with the Rust `tokenizers` library, and does the two remaining steps (mean pooling over the attention mask, then L2 normalisation) in about ten lines of numpy.

That is the entire engine, and the RSS worker now runs the very same module: its image ships the shared memory package rather than a copy, so the server and the worker cannot embed differently. The engine also reads the model's own pooling configuration, refuses a model whose vectors would not fit the index, and pins the default model to a known commit so an upstream re-export can never move vectors under a live store.

## The numbers

Measured with the new benchmark harness (`scripts/embedding_bench.py`, shipped in this release) on an arm64 machine, against a real valkey-search, with a 400-memory corpus and 44 queries. Both runs in the same environment, same corpus, same machine, minutes apart.

| Metric | torch (6.6) | onnx (6.7) | Change |
|---|---:|---:|---:|
| model load | 7768 ms | 776 ms | **−90%** |
| peak resident memory | 924 MB | 301 MB | **−67%** |
| `embed()` p50 / p95 | 22.3 / 40.4 ms | 7.9 / 16.8 ms | −64% / −58% |
| `remember()` p50 / p95 | 25.0 / 47.4 ms | 10.9 / 16.0 ms | −57% / −66% |
| `remember_document()` | 82 ms per chunk | 34 ms per chunk | −59% |
| `recall()` p50 / p95 | 35.0 / 56.1 ms | 22.7 / 42.0 ms | −35% / −25% |
| `recall_index()` p50 | 34.3 ms | 21.2 ms | −38% |
| single-text throughput | 41 texts/s | 119 texts/s | +193% |
| batch of 8 | 95 texts/s | 111 texts/s | +18% |
| batch of 32 | 93 texts/s | 107 texts/s | +15% |
| **MCP server image** | **2.04 GB** | **634 MB** | **−69%** |

One honest note on the load row: PyTorch's load time bounced between 2.6 and 7.8 seconds across runs depending on what was in the disk cache. ONNX Runtime sat at 0.8 seconds every time. Read that row as "seconds versus under a second" rather than exactly ten times.

## Is it the same model, though?

This is the question that matters more than any speed number, because a faster model that ranks your memories differently is a migration wearing a performance improvement's clothes. Every vector in your store would be in the wrong place relative to the new ones.

So the benchmark checks. It embeds the same 48 texts with both backends and compares the vectors, and it runs the same 43 queries against the same corpus and compares what comes back.

| Check | Result |
|---|---|
| vector cosine similarity, same text both backends | mean 1.0, minimum 1.0 over 48 texts |
| top-10 results per query, matched by content | Jaccard 1.0 over 43 queries |
| top-1 result identical | 43 of 43 |
| score drift | 0 |

Identical. Not "close enough". The ONNX graph is the same weights the PyTorch model loads, the pooling is the same arithmetic, and the tokeniser is the same tokeniser file. The vectors you already have are the vectors 6.7 would produce for the same text. Deduplication thresholds, contradiction thresholds, the skill compiler's clustering threshold: none of them move.

## Why ONNX wins

It is worth being precise about where the speed comes from, because the table has a shape to it. Batched throughput improved a modest 15 to 18 percent. Single-text latency improved 64 percent. Load time and memory improved by an order of magnitude and two thirds respectively. That pattern tells you what happened.

**PyTorch was never slow at the maths.** A batch of 32 texts is a handful of big matrix multiplies, and PyTorch's CPU kernels do those about as well as anyone's. That is why the batch numbers only moved a little. If OmniMem's workload were bulk embedding, this swap would have been a nice-to-have.

**PyTorch was slow at everything around the maths.** Every call into a PyTorch model goes through the framework: Python-level module dispatch, autograd bookkeeping it has to check it can skip, tensor allocation through its own allocator, a tokeniser wrapped in Python. For a 32-text batch that overhead is amortised. For one text, it is most of the wall clock. And OmniMem's workload is one text: one memory being stored, one query being answered, one fact being extracted. The 64 percent drop in single-text latency is that overhead going away, and it flows straight through to `remember()` and `recall()` because those are one-text operations with a database round trip attached.

**ONNX Runtime treats the model as what it is: a fixed graph.** It knows the whole computation up front, so it fuses operations (attention blocks collapse into single kernels, layer norms merge with the ops around them), plans memory once, and runs the result with no dynamic dispatch. It has no concept of gradients to check for. It is a C++ inference engine with a thin Python binding, and it loads an 86 MB graph in the time PyTorch takes to import itself.

**The tokeniser matters more than people expect.** sentence-transformers tokenises through the Python `transformers` wrapper. The Rust `tokenizers` library it wraps is fast; the wrapping is not. Calling the Rust library directly, in batches, removes a second layer of per-call overhead on a path that runs once per memory and once per query.

**Memory is the framework, not the model.** The model's weights are 86 MB on disk. The 924 MB resident set was PyTorch: its kernels, its allocator arenas, its dependency tree. ONNX Runtime plus tokenizers plus numpy is a 300 MB process holding the same model, and most of that is Python itself.

The result is a backend that is better at exactly the shape of work OmniMem does, worse at nothing it does, and a third of the size. That is why it won.

## What this means in practice

- **Raspberry Pi and small VMs**: a 300 MB process instead of 900 MB, and a 634 MB image instead of 2 GB. The Pi guide's swap-file advice for the build step was there because of PyTorch; it should now be unnecessary on a 4 GB board, though we have not yet re-run that build to confirm.
- **Cold starts**: the MCP server is serving requests within a second of the container coming up. The first `briefing()` of a session no longer waits on PyTorch.
- **Latency where you feel it**: `recall()` is what every session start and every "have we seen this before?" runs through. A third off its median is a third off the pause before Claude answers.
- **Your store is untouched.** Upgrade, restart, carry on. The startup migrations from 6.6.x still run; there is no new one.

## Upgrading

Pull 6.7.0 and restart. The model's ONNX graph and tokeniser are downloaded on first start into the same Hugging Face cache the PyTorch weights used, and the compose file now mounts that cache as a shared volume so a recreated container does not download again. One note for air-gapped hosts: a cache filled by 6.6 holds the weights but not the ONNX graph, so fetch it once online first, or point `EMBEDDING_MODEL` at a directory holding the files.

If you need the old path back, set `EMBEDDING_BACKEND=torch` and install `mcp_server/requirements-torch.txt`. The images no longer ship PyTorch, so this is a rollback for a local checkout rather than a container flag. We do not expect anyone to need it; it exists because a backend swap without a rollback is a bet, and this one was measured instead.

Four new settings, all optional, all documented in the configuration reference: `EMBEDDING_ONNX_FILE` picks a different graph from the model repo, `EMBEDDING_MODEL_REVISION` pins the repo commit, `EMBEDDING_MAX_SEQ_LENGTH` overrides the token cap, `EMBEDDING_THREADS` pins the runtime's thread count.

## One thing not to do yet

The model repo also ships quantised graphs (`onnx/model_qint8_arm64.onnx`, `onnx/model_qint8_avx512.onnx`). They are faster and smaller again. They are also **not** vector-equivalent to the float model: an int8 graph rounds its weights, and the vectors it produces sit close to the float ones but not on top of them. Switching to one is exactly the migration this release avoided. If you want to try it, run the benchmark first, read the equivalence section, and re-embed the store before you point recall at it. The harness will tell you, in numbers, whether it is worth it for your corpus.

## How we measured

`scripts/embedding_bench.py` is in the repo. It ingests a corpus (a seeded synthetic one, or the text of your own backup), runs a fixed query set through the real recall pipeline against a real valkey-search, and writes a report with the raw vectors and the top-10 for every query. `compare before.json after.json` prints the tables above and exits non-zero if the two backends disagree. It refuses to run against a populated store and cleans up everything it wrote, so it is safe to run on a laptop and honest about what it finds.

The full write-up, including how to run it yourself, is in [docs/embedding-benchmark.md](../embedding-benchmark.md).
