# OmniMem: Future Ideas

Ideas worth exploring, gathered from research and from using the thing. Not commitments. What's actually planned for 7.0 lives in [docs/rust-port-plan.md](docs/rust-port-plan.md).

## Planned for 7.0.5: the desktop app sets up the tools you have

The tray menu can connect Claude Desktop and Claude Code to the server today (`crates/omnimem-desktop/src/clients.rs`), and 7.0.4's `omnimem install-hooks` adds the Claude Code hooks from the command line. 7.0.5 brings both together in the desktop app, offering only what fits the tools actually installed.

**Why it matters:** a menu listing every client OmniMem could talk to is noise for someone who uses one of them, and an option for a tool that isn't there can only fail. Showing what's installed, and whether it's already set up, turns the menu into a checklist.

**To do:**
- [ ] Detect each supported tool: whether it's installed (its command on the PATH or the usual install locations, as `find_claude()` does for `claude`, or its config folder), and whether OmniMem is already configured in it
- [ ] Claude Code: connect the MCP server (there already) and install the hooks, reusing `install-hooks`' merge so the CLI and the menu behave the same
- [ ] Claude Desktop: connect the MCP server (there already)
- [ ] Codex: connect the MCP server in its config, offered only when Codex is installed
- [ ] Decide which other clients are worth detecting (Cursor, VS Code, Gemini CLI, ...) and how each is configured
- [ ] Show only installed tools in the menu, each marked as set up or offering to set it up; nothing at all for a tool that isn't there
- [ ] Back up any config file before changing it and leave everything else in it alone, as the existing writers do
- [ ] The Flatpak can't reach other tools' config from its sandbox: decide whether it asks for that access or explains what to add by hand

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
