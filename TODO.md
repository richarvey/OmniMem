# OmniMem - Future Ideas

Ideas and potential features gathered from research and usage. Not commitments, just things worth exploring.

## Hybrid keyword + vector search

Right now recall is purely vector-based (cosine similarity via HNSW). This works well for semantic matches but can miss exact keywords like library names, error codes, or specific tool versions.

Adding a keyword search pass (either a simple text match or something like Valkey's full-text indexing) alongside the vector results could improve recall accuracy. The two result sets would be merged and re-ranked through the existing scoring pipeline.

**Why it matters:** If you stored a memory about `onnxruntime` crashing, a query for "onnx" should always find it, even if the embedding doesn't place them close enough.

**Approach to explore:**
- [ ] Investigate Valkey's TAG and TEXT field indexing for keyword matching
- [ ] Run keyword search in parallel with vector search
- [ ] Merge and deduplicate results before the scoring pipeline
- [ ] Benchmark recall quality with and without hybrid search

## Progressive disclosure (token budgeting)

As memory stores grow, returning full content for the top 5 results can use a lot of context. A two-step recall mode could help: first return a lightweight index (titles, timestamps, types, approximate token count), then let Claude fetch the full content for just the ones it actually needs.

**Why it matters:** Keeps recall efficient as the memory store scales into hundreds or thousands of entries. Also makes the agent smarter about which memories are worth the context budget.

**Approach to explore:**
- [ ] Add a `recall_index()` tool that returns compact summaries with keys
- [ ] Add a `recall_detail(keys)` tool that fetches full content for specific entries
- [ ] Keep the existing `recall()` as the default for smaller stores
- [ ] Consider auto-switching based on result count or total token estimate

## Web viewer for memory management

A simple browser-based UI for browsing, searching, and managing memories. Currently the only way to interact with memories is through MCP tool calls, which makes bulk review and cleanup harder than it needs to be.

**Why it matters:** Being able to visually scan your memory store, spot duplicates, review stale entries, and manage lifecycle states in a dashboard would be a much better experience than doing it all through the CLI.

**Approach to explore:**
- [ ] Serve a lightweight single-page app from the MCP server on a separate route
- [ ] Browse memories by namespace, project, state, and date
- [ ] Search with the same recall pipeline used by the MCP tools
- [ ] Manage lifecycle (deprioritise, archive, reinstate, delete) from the UI
- [ ] Show contradiction warnings and duplicate clusters visually
