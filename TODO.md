# OmniMem: Future Ideas

Ideas worth exploring, gathered from research and from using the thing. Not commitments. What's actually planned for 7.0 lives in [docs/rust-port-plan.md](docs/rust-port-plan.md).

## Hybrid keyword and vector search

Recall is purely semantic: exact cosine similarity over the embeddings. That's great for "things like this" and not so great for exact tokens like library names, error codes or version numbers.

**Why it matters:** if you stored a memory about `onnxruntime` crashing, a query for "onnx" should always find it, even when the embedding doesn't put the two close enough together.

**Approach to explore:**
- [ ] An FTS5 index over memory content in the SQLite store (SQLite ships with it, so there's nothing new to depend on)
- [ ] Run the keyword search alongside the vector search and merge the candidates before scoring
- [ ] Decide how a keyword-only hit is scored, since it has no similarity to gate on
- [ ] Benchmark recall quality with and without it on a real store before turning it on

## An HNSW index behind the same search

Exact search over an in-memory matrix is under 10 ms at OmniMem's scale (tens of thousands of memories) and it's deterministic, which the v7 clustering rules need. A store that grows well past that might not stay fast enough.

**Approach to explore:**
- [ ] Measure where exact search actually starts to hurt, on real hardware including a Pi
- [ ] If it does, add an approximate index behind the existing search interface, keeping exact search as the default
