# OmniMem Development Guide

## What is this?

Self-hosted semantic memory for AI agents, served over MCP. Memories live in five namespaces: episodic (decisions, bugs, patterns), project context (stack, goals, state, work-type domains), knowledge (RSS articles and extracted facts), preferences (prescriptive rules pulled from conversation) and compiled skills (SKILL.md documents distilled from experience, written only through a propose-and-accept gate). Every memory carries a `licence` (own, open, restricted, unknown) and a `provenance` (retrieved, concluded, asserted).

**Version**: 7.0.0-dev, on branch `v7.0.x`. OmniMem 7 is a Rust rewrite of the 6.x Python stack (Valkey, FastMCP server, Starlette web UI, RSS worker) as **one binary**: SQLite with exact in-memory vector search, the MCP server, the enrichment worker and RSS scheduler in one process, and a desktop app whose settings panel replaces the web UI. The 6.x Python lives on the `v6.x` branches only.

**Stack**: Rust (edition 2024, see `rust-version` in `Cargo.toml`), rusqlite (bundled SQLite), ONNX Runtime via `ort` plus `tokenizers` running all-MiniLM-L6-v2 (384-dim), rmcp over streamable HTTP on axum, minijinja for the panel, tao + wry + tray-icon for the desktop app, reqwest (rustls) for the Anthropic API and feeds, feed-rs, keyring.

The working plan and the evidence for each phase is `docs/rust-port-plan.md`; the Mycelium adapter spec is `docs/v7-change-spec.md`. Update the plan's progress table as phases land.

## Workspace

```
crates/
  omnimem-core/      Namespaces, keys, record types, validation, licence and provenance vocabularies,
                     content hash, env (settings read environment first, then the desktop overlay)
  omnimem-embed/     ONNX embedder: model resolution (local dir, HF cache, pinned download), pooling
  omnimem-store/     SQLite schema and migrations (PRAGMA user_version), vector matrix, filtered exact k-NN,
                     kv (what were meta:* keys), enrichment queue, OAuth tables, 6.x backup import/export
  omnimem-engine/    Recall pipeline, lifecycle, dedup, contradictions, experience, projects and domains,
                     lineage, chunking, temporal, briefing, maintenance, skills compiler/scan/transfer, enrichment
  omnimem-llm/       Blocking Anthropic Messages client behind the core LanguageModel trait
  omnimem-mcp/       The 48 tools (descriptions copied verbatim from 6.x), instructions, streamable HTTP,
                     bearer auth, the OAuth 2.1 authorisation server (src/oauth/), Host/Origin guards
  omnimem-rss/       Feed fetch and parse, summary and digest modes, licence gate, scheduler, feed influence
  omnimem-settings/  The settings panel: axum routes never bound to a socket, minijinja templates and static
                     files embedded by build.rs, the Configuration page and the SecretStore trait
  omnimem-app/       What server and desktop share: open the engine, run the services, data folder, instance lock
  omnimem-desktop/   Tray or menu bar icon, settings window on the omnimem:// scheme with IPC, keychain, start at login
  omnimem/           The binary: serve, import, export, stats, search, embed, rss, and desktop (default with the feature)
ci/desktop/          Container with GTK/WebKitGTK/AppIndicator for building and smoke-testing the desktop crate
claude_config/       CLAUDE.md template and MCP config for end users
examples/feeds.yml   Sample reading list
docs/, guides/       User documentation
```

`omnimem-desktop` is outside the workspace's `default-members`, so plain `cargo build` and `cargo test` need no GUI libraries.

## Running locally

```bash
cargo run -p omnimem -- serve                   # MCP at http://127.0.0.1:8765/mcp, data in ./data/omnimem.db
cargo run -p omnimem -- import backup.json      # a 6.x dump_to_file backup, re-embedded
OMNIMEM_DB=/tmp/x.db cargo run -p omnimem -- stats
```

Configuration is the 6.x environment variables (see `crates/omnimem-settings/src/configuration.rs` for the catalogue the panel shows). The desktop app reads `omnimem.env` from its data folder and secrets from the OS keychain into `omnimem_core::env`'s overlay at start; `serve` never does. The production 6.x stack on this host holds ports 8080 and 8765, so use another `MCP_PORT` for live checks.

## Tests and checks

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
scripts/desktop-check.sh     # desktop crate: clippy, tests and a smoke test under Xvfb, in ci/desktop
```

- The dev host runs out of memory on parallel builds: set `CARGO_BUILD_JOBS=2` and run the desktop check on its own, never alongside another cargo job
- SQLite runs in memory in tests; there are no store fakes, so a fake can't diverge from the real thing
- Golden fixtures pin byte-level contracts with 6.x: reference vectors (`omnimem-embed/tests/fixtures`), skill bodies (`omnimem-engine/tests/fixtures/skills_golden.json`), the OAuth HTTP surface (`omnimem-mcp/tests/fixtures/oauth_golden.json`). The capture scripts beside them need a `v6.7.x` checkout
- Tests that need the real embedding model run only when the Hugging Face cache has it

## Compatibility contract with 6.x

Kept exactly: the 48 tool names, parameters, defaults, descriptions and result JSON (including `_compact()` dropping empty values); vectors; recall scoring (floor, weak band, recency, experience weight, temporal boost, reinstate 0.6, fact collapse, ordering); rendered skill bodies byte for byte; backup JSON, skill bundle zips (format v2, reads v1) and `feeds.yml`; every 6.x environment variable except the Valkey, web UI and PyTorch ones.

Deliberately gone: Valkey, the web UI and `/metrics` over HTTP, SSE transport, `EMBEDDING_BACKEND=torch`. Over HTTP the binary serves `/mcp`, the OAuth routes and `/healthz`, nothing else.

## Design rules that still bind

- **Skill compilation is deterministic**: the same sources render a byte-identical body except the `compiled_at` line. No randomness, map-order dependence or extra timestamps in rendering, or every recompile proposes a noise diff. The skill banner still says "(Valkey)" until cut-over for the same reason
- **Skill writes are gated**: `compile_skill(mode="propose")` stashes the draft; `mode="write"` commits that stash verbatim and refuses if the stored skill changed. Experience and graveyard writes stay ungated; that asymmetry is the design
- **Domains route, they never label memories**: a project's domains narrow which projects a recall searches; memories don't inherit them. An unmatched domain is always reported, and an empty domain and project intersection returns nothing rather than searching everything
- **Licence and provenance are decided at write time and never scored on**. A fact inherits its source's licence and provenance. Reclassifying goes through the lineage stamp, cascades to chunks and extracted facts, and never bumps `updated_at` (the skill compiler reads that as "source changed")
- **Promotion and feed influence feed skills; ordinary knowledge doesn't.** Refs and feed rules bypass the reinforcement gate but never count towards it or bootstrap a skill
- **Settings are read through `omnimem_core::env`**, never `std::env::var` directly, and nothing writes to the process environment (not sound once the keychain has started threads)
- **The fail-closed rule**: a non-loopback `MCP_HOST` needs `MCP_AUTH_TOKEN` or OAuth; OAuth switched on but incomplete refuses to start
- **OAuth**: refresh tokens rotate with a grace window (`OAUTH_REFRESH_GRACE_SECONDS`) in which the old token replays the same successor pair, which stops claude.ai's concurrent refreshes being signed out. Codes and tokens are stored as SHA-256 hashes; a code is consumed and a token rotated inside one `Store::with_oauth` transaction

## Gotchas

- **WebKitGTK needs wry's `linux-body` feature** (WebKitGTK 2.40+) or the custom scheme gets empty POST bodies, and it doesn't follow redirects from a custom scheme, so the panel answers a form with a page that replaces itself (`see_other`) rather than a 303
- **Webview downloads are unreliable** (WebKitGTK saves silently, macOS won't report), so the panel writes exports and backups into the Downloads folder itself and says where
- **The panel is never bound to a socket**: `Panel::handle` runs axum routes in-process for the window's `omnimem://` handler. Don't add HTTP routes for it
- **GNOME shows no tray icon without the AppIndicator extension**; the app opens its window instead
- **Numbers rendered where 6.x output is compared** are formatted to match Python (`str(float)` and friends), not Rust's defaults

## Validation constraints

- Project names: alphanumeric, hyphens, underscores, dots, spaces
- Content: max 50 KB per memory; tags: max 20, each 100 characters or fewer
- Project domains: max 20, same charset as skill domains, aliases resolve
- Skill domains: lowercase kebab-case, 1 to 64 characters of `[a-z0-9._-]`
- `set_licence` and `set_provenance` take at most 200 keys per call
- Key prefixes: `mem:episodic:`, `mem:project:`, `mem:knowledge:`, `mem:preference:`, `mem:skill:`

## Committing

Commit after each meaningful piece of work. The repo lives on Forgejo at `code.squarecows.com` (owner `ric`), pushed over SSH on port 222; the Forgejo MCP points at it for PRs, releases and CI jobs.

**Branch policy**: version lines are developed and released from their own branch (`v7.0.x` here). Cut tags and releases on the version branch. Never merge a version branch into `main` unless Ric asks.

## Writing style

- British English (colour, summarised, centre)
- Conversational and human, no em dashes, no marketing fluff
- Docs and guides in Ric's own voice (his published articles are the reference), lighter on reference pages
- Technical but accessible, with concrete numbers where possible
