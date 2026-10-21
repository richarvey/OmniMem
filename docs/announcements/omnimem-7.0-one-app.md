# OmniMem 7: one app, no Docker

OmniMem 7 is out, and it's a different kind of program. 6.x was four containers: Valkey with its search module, the Python MCP server, the web UI and the RSS worker, held together by a Docker Compose file. 7.0 is one Rust binary. It holds its own store, serves MCP, runs the RSS scheduler and the enrichment worker, and on a desktop it sits in your tray or menu bar with a settings window behind it.

Nothing else to install. No Docker, no database server, no Python. Download it, open it, and the embedding model fetches itself on the first start (about 90 MB, once).

Your memories come with you via a simple backup and import process, and they behave the way they did before. The 48 MCP tools have the same names, parameters and defaults, so an agent that knew OmniMem 6 knows OmniMem 7.

The first release is 7.0.3, on the [releases page](https://code.squarecows.com/ric/omnimem/releases/latest).

## Why rewrite it

OmniMem's job is small: embed one memory, embed one query, search, rank. 6.x did that job well, but the machinery around it kept growing. Four images to pull and keep in step. A search index that lived beside the data and could drift from it, with phantom entries, silent index migration failures and tag-query quirks to work around. Each Python service loaded its own copy of the embedding model. And the only way to see your memories was a web UI on a port, which is one more thing listening on your machine. It was great for developing and testing the theories behind how memory and experience should live and work together, but I wanted to make this more accessible for everyone.

## What 7 is

**One process.** The MCP server, the memory engine, the embedding model, RSS ingestion and the database all live in one binary. One embedder serves every caller. Written in Rust so memory safe and super fast.

**One file.** Memories live in SQLite. On start, every vector is loaded into one matrix per namespace and searched exactly, not approximately. At OmniMem's scale (tens of thousands of memories) a full scan takes a few milliseconds and gives the same answer every time. Filters are plain SQL, so there's no separate index left to drift from the data. A real 3,500-memory store comes to a 12.8 MB database.

**Nothing leaves your machine.** The embedding model (all-MiniLM-L6-v2 on ONNX Runtime) runs locally. The Anthropic API is only called if you give OmniMem a key for the optional Claude Haiku features: fact extraction, query expansion, deeper contradiction checks and RSS summaries.

**Less on the network.** Over HTTP, 7.0 serves `/mcp`, the OAuth 2.1 routes that protect it and `/healthz`. That's all. There is no web UI on a port and no `/metrics`.

## On your desktop

There's an installer for each platform: a DMG for macOS (Apple silicon, 13.4 or later), an MSI for Windows 10 and 11, and a Flatpak for Linux desktops on amd64 and arm64.

OmniMem lives in the tray, or the menu bar on a Mac. The menu shows whether it's running and how many memories it holds, and it does the setup for you:

- **Add to Claude Code** and **Add to Claude Desktop** checks the client's settings first and never adds OmniMem twice. An old `omnimem` entry pointing at a 6.x install is offered as **Switch Claude Desktop here**, which repoints it rather than adding a second.
- **Copy MCP URL** covers every other agent with a bit of manual work.

**Settings…** opens a window with every page the 6.x web UI had (the dashboard, memories, search, projects, experience and the graveyard, skills with export and import, duplicates, contradictions, telemetry, token overhead, feeds and backups) plus a configuration page for everything the server sets in its environment (such as the oAuth settings or the Anthropic API key). The window's pages come from the app itself over a custom URL scheme. Nothing about them is reachable over a network, from your machine or anyone else's.

Secrets (your Anthropic key, the MCP access token, a Hugging Face token) go to the OS keychain: Keychain on macOS, Credential Manager on Windows, the Secret Service on Linux. Once saved they're never shown again.

## On a server

`omnimem serve` is the same binary without the desktop parts, configured with the same environment variables as before.

- **`.deb` and `.rpm`** for amd64 and arm64 install it as the `omnimem` systemd service, running as its own user, with one settings file (`/etc/omnimem/omnimem.env`) and its data in `/var/lib/omnimem`. There's a tarball with an `install.sh` for other distributions.
- **Docker**: `richarvey/omnimem:7.0.3`, amd64 and arm64, with everything in one `/data` volume. A token is all it needs.
- **OAuth 2.1** is rebuilt by hand on axum, and every route, discovery document, challenge and error was checked against a capture of the 6.x server, so claude.ai and other OAuth clients connect as before. Tokens are stored only as SHA-256 hashes and survive restarts.

Linux builds need glibc 2.38 or newer (Debian 13, Ubuntu 24.04, Fedora 39 and later), a floor set by ONNX Runtime's library.

## The same memory

This is the part that matters most. A rewrite that ranks your memories differently is a migration dressed up as an upgrade, so 7.0 was checked against 6.x at every step:

- **The vectors are the 6.7 vectors.** The Rust engine was checked against full reference vectors the Python engine wrote, covering accents, CJK, emoji, empty text, truncation past 256 tokens and mixed-length batches. Deduplication, contradiction and skill clustering thresholds don't move.
- **Recall returns the same results.** A real production backup of 3,500 memories was imported, and all 32 query and namespace searches tested returned an identical ordered top 10 to the Python engine's search over the same data. Every field survives an export.
- **Skills compile the same.** Skill rendering matches the Python compiler byte for byte. 
- **RSS reads the same articles.** On the project's own reading list, the Rust parser picks out exactly the 59 articles 6.x's feedparser does, with the same keys, titles, dates and text.

Recall scoring has three deliberate changes, all in the [changelog](../../CHANGELOG.md):

1. **Recency declines from the day a memory is written**, reaching 0.9 at 90 days and then falling 5% every 30 days to a 0.3 floor. 6.x held it flat at 1.0 for 90 days, so yesterday's memory and one from three months ago scored alike. The relevance floor reads raw similarity, so nothing extra is dropped.
2. **An abandoned outcome scores 1.0, not 0.1.** The old weight buried the write-up of a failure along with the approach, so the one memory explaining why something didn't work ranked below almost everything else.
3. **Recording an abandonment no longer suppresses anything.** Suppression hid the memory that explained the failure, and it was store-wide, so a dead end on one project silenced unrelated memories on another.

## Memory your agent doesn't have to ask for

One finding shaped this release more than any other. Across twenty runs with OmniMem attached and the answer already in the store, the agent made zero memory calls of its own. The memory was there. Nothing asked it.

So 7.0 ships [Claude Code hooks](../claude-code-hook.md). `omnimem hook` puts the project's briefing into every session's opening context, read-only, in about 6 ms. Before a tool call that changes something, it checks the graveyard, and if the call proposes an approach this project already abandoned, it stops it and hands back what was abandoned, why, and what worked instead. It never guards a read, and it fails open on any error. At the end of a session it records what the session did, skips sessions that changed nothing and redacts credentials before writing.

Measured over ten pairs of runs on one task: every attempt at the abandoned approach was stopped before it ran, no run with the hooks reached a failing test suite (against 8 of 10 without them), and the runs cost 27% less.

## Hardened before release

The whole Rust workspace was security reviewed before 7.0.0, with fixes in every crate. The headlines: the OAuth routes are size-capped, rate limited and strict about PKCE and redirect URIs; the settings panel carries no inline JavaScript and sends a Content Security Policy; skill bundles are read against their declared sizes, so a zip bomb is refused rather than inflated; RSS fetches refuse private and loopback addresses and re-check every redirect; and the model download verifies a sha256 from the hub. CI now runs `cargo audit` and gitleaks on every push. The full list is in the [changelog](../../CHANGELOG.md).

## Coming from 6.x

Your 6.x install keeps working, and nothing upgrades itself. When you're ready:

1. On 6.x, take a backup: ask your agent to call `dump_to_file()`, or use the old web UI's Backups page.
2. Stop 6.x, or at least make sure it isn't on port 8765.
3. Install 7, then restore the backup on the settings window's **Backups** page, or run `omnimem import omnimem_backup.json`.

Backups don't carry vectors, so every memory is embedded again from its text. That's about 8 ms a memory, so a few thousand take well under a minute and 25,000 take roughly three and a half minutes. It also has a side benefit: any memory 6.x had stopped finding, because its stored vector had gone missing or stale, is found again.

If you aren't ready, the 6.x Docker stack carries on from the `v6.7.x` branch, and the old one-line installer still sets up 6.7.

A few things are gone on purpose: Valkey and every `VALKEY_*` setting, the Compose stack, the web UI on port 8080, `/metrics` and the SSE transport. If an agent's config still says `"type": "sse"` or ends in `/sse`, point it at `http://127.0.0.1:8765/mcp` instead. The [configuration reference](../configuration.md#gone-since-6x) lists every setting that went.

## Rough edges

- **No Intel Mac build.** ONNX Runtime ships no prebuilt library for Intel macOS, so the DMG is Apple silicon only.
- **The Windows installer isn't signed yet**, so SmartScreen asks first: **More info**, then **Run anyway**.
- **In the Flatpak, Add to Claude Code can't work**, because the sandbox can't see a `claude` installed outside it. The menu says so, and `claude mcp add` does the job.
- **Stock GNOME doesn't show tray icons** without the AppIndicator extension. Where there's no tray, OmniMem opens its settings window instead and says which extension to install.

## Get it Now

- [OmniMem 7.0.3 on the releases page](https://code.squarecows.com/ric/omnimem/releases/latest), with a `.sha256` beside every file
- [Quick start](../quick-start.md)
- [omnimem.org](https://omnimem.org)

OmniMem is open source, MIT licensed, and free. Issues and ideas are welcome on [GitHub](https://github.com/richarvey/OmniMem/issues).
