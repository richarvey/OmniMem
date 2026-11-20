# OmniMem 7.2.x spec: daydream and the scrub gate

Nov 20, 2026 · @Ric Harvey · Decisions recorded 2026-11-20 (#43) · Branched from v7.1.x (7.1.7-dev, c296558) as v7.2.x at 7.2.0-dev

The work is tracked in the [7.2.0 milestone](https://github.com/richarvey/OmniMem/milestone/6) (#43 to #60) and the [7.2.1 milestone](https://github.com/richarvey/OmniMem/milestone/7) (#61, #62).

## Summary and scope

**Daydream** is the name for this feature. Other agent-memory tools "dream": they consolidate and rewrite memory unattended, in the background, and that has drawn fair criticism. OmniMem daydreams instead. It happens while you're there (when you import a log, when a hook fires, or when you press **Daydream now**), it runs every transcript through a deterministic secrets gate first, and everything it finds is only a `candidate` until a person accepts it. Nothing runs on a timer.

7.2.x lets OmniMem learn from the sessions people actually had, not only from what an agent chose to `remember()`. It reads session logs from all 13 clients in `guides/`, takes secrets out with a deterministic gate before anything else sees the text, finds failure and recovery patterns, and proposes them as `candidate` memories for a person to accept.

In scope for 7.2.0:

- **The scrub gate** (`omnimem-scrub`): a git-secrets-style scanner with no LLM. It runs before storage, embedding, any LLM call and any log line. It replaces today's `redact()` in `hook.rs`.
- **Adapters for every guide client.** Clients that keep local transcripts are read from disk. Clients that only have an account export get an upload path. Clients with neither can submit through an MCP tool.
- **Three ways to bring logs in**: the desktop app (tray and local discovery), the settings panel (browser upload, scanned in the browser by a WASM build of the gate before it leaves the machine, which also works for headless and hosted instances), and the CLI. Hooks and the MCP tool feed the same queue.
- **Deterministic detectors** that turn sessions into signals, and a reinforcement gate that turns repeated signals into proposals.

Out of scope: scheduled or background dreaming (decided 2026-11-20), an OTLP receiver, and LLM-written syntheses beyond rewording a proposal. A daydream runs when a person imports, when a hook fires, or when a person presses **Daydream now**. Nothing runs on a timer.

## Decisions and interpretations

Ric decided (2026-11-20):

- Daydream ships as **7.2.x**, branched from **v7.1.x**.
- Cover **as many of the guide clients as possible from day one**.
- Secrets are caught by a **deterministic scan before any text reaches an LLM**, in the spirit of git-secrets.
- Log uploads are allowed **through the client and through the web settings**.
- No scheduled dreaming yet.
- Thin slicing and model routing, once pencilled in as 7.2.0, move to **7.3.x**. Daydream's per-client tallies land first, and routing needs them.
- The feature is called **daydream** everywhere: the panel, the tray, the docs, the settings (`DAYDREAM_*`) and the code (`daydream.rs`, `daydream_signals`). "Session logs" stays the name for the ingest side (the Logs page, `omnimem logs`), since that's what people handle.

Interpretations taken here, to confirm:

- "The client" means the OmniMem desktop app (tray, local discovery, local scan before anything is sent) plus the `omnimem` CLI. "The web settings" means the settings panel in a browser, which is the only route for headless and hosted instances. The MCP tool is a third route for chat-only clients.
- The gate runs before **everything** that persists or transmits text, not just before the LLM: the store, the embedder, the audit log, `tracing` output and the queue. A secret in an embedding or a debug log is still a leaked secret.
- Daydreamed memories keep `provenance` as `concluded`, because the system concluded them. The transcript origin goes in new fields (`origin`, `origin_client`, `origin_model`, `source_refs`). Adding a fourth provenance value would blur a field the docs define as "who is speaking".

## Architecture

```
routes (5)                       gate (1)                stages (3)
 desktop app discovery ─┐
 panel upload ──────────┤                          ┌─ detectors (deterministic signals)
 CLI ───────────────────┼─▶ adapters ─▶ omnimem-scrub ─▶ queue ─┼─ reinforcement gate (≥ N sessions)
 hooks ─────────────────┤    RawText     CleanText             └─ proposals ─▶ Pending (candidate)
 submit_session_log ────┘                                            └▶ optional LLM rewording
```

Every route ends in the same adapters and the same gate, so a log uploaded in a browser is treated exactly like one read from disk. The optional LLM only ever sees proposals made from `CleanText`.

## The scrub gate

Every byte of a transcript passes through `omnimem-scrub` before anything else touches it. The crate is pure Rust with no network and no LLM, so the same input always gives the same output. It fails closed: if a scan errors, nothing from that session goes further.

### Enforced by the types, not by convention

Adapters produce `RawText`, which has no `Display`, no `Serialize` and a redacting `Debug`. The only way to get a `CleanText` is `Scrubber::scan()`. The store's transcript writes, the embedder, `omnimem-llm`, the audit log and the daydream queue accept only `CleanText`. A missed call becomes a compile error, not a leak. A clippy lint in CI also bans `tracing` macros on `RawText`.

### Rule layers, in order

1. **Known formats.** Provider and platform tokens: Anthropic `sk-ant-`, OpenAI `sk-proj-` / `sk-`, AWS `AKIA` / `ASIA` keys and secret-key assignments, GitHub `ghp_` / `gho_` / `github_pat_`, GitLab `glpat-`, Slack `xox[abprs]-`, Stripe `sk_live_` / `rk_live_`, Google `AIza`, Tailscale `tskey-`, Cloudflare tokens, Hugging Face `hf_`, npm `npm_`, PyPI `pypi-`, PEM private keys, JWTs, and OmniMem's own bearer tokens and OAuth client secrets.
2. **Credentials in structure.** Passwords in URLs (`postgres://user:pass@`), `Authorization:` and `Cookie:` headers, `.env`-style `KEY=value` blocks whose key names a secret, and `--password` / `-p` command-line flags.
3. **Keyword plus entropy.** A value next to `secret`, `token`, `password`, `api_key` and similar is redacted when its Shannon entropy passes a threshold (gitleaks' approach). That catches unknown formats without flagging every hex string.
4. **Known secrets by fingerprint.** The user can register their own secrets: OmniMem's `MCP_AUTH_TOKEN`, values from `omnimem.env`, or any file they point at. Only a salted SHA-256 and the length are stored. The scanner hashes candidate tokens of the same length and redacts exact matches. This is the git-secrets `--add --literal` idea, without keeping the secret.
5. **Personal data (opt-in).** Email addresses, phone numbers, private IPs and tailnet hostnames. Off by default, one switch per class.

The built-in pack is written in **gitleaks' TOML rule format**, so people can drop in an existing `gitleaks.toml` or a custom rule file. git-secrets' allowed-patterns idea becomes an allowlist, checked per rule and stored in the database. Rules compile once at startup into a `RegexSet`. Today's `redact()` compiles its seven patterns on every call.

### What a hit does

- **Redact** (default): the value becomes `[redacted:aws-access-key#3f2a]`. The suffix is a short keyed hash, so the same secret gets the same placeholder throughout a session and "the key that failed" still reads sensibly to a detector. The value itself is never recoverable.
- **Drop segment**: for rules marked `block` (private keys, `.env` dumps longer than N lines), the whole turn or tool output is replaced with a placeholder.
- **Quarantine session**: if more than a set number of `block` hits land in one session, it is held for review on the Logs page and not daydreamed over.

Each scan writes a findings report: counts per rule, plus turn index and character offset. It never holds the value or any surrounding text. The audit log and the Logs page show only that report.

### Dry run and reuse

`omnimem logs scan <path>` and the panel's **Preview** run the gate and show the findings without importing anything. The same crate replaces `redact()` in `hook.rs`, so the existing SessionEnd record gets the full rule pack too. The crate also builds for `wasm32-unknown-unknown`, and in 7.2.0 the settings panel scans an upload in the browser before the file leaves the machine. The server scans again on arrival, so the browser's scan is a courtesy to the person and never the thing the store relies on. Native and WASM builds must give byte-identical output on the same input.

## Client coverage on day one

All 13 clients in `guides/` get a route in 7.2.0. Seven keep transcripts on disk that OmniMem can read directly. Three have an account export to upload. Three have neither, so they rely on the MCP submit tool. Every path below needs checking against a real install before its adapter is merged.

| Client | Where sessions live | Format | Route in 7.2.0 | Caveats |
| --- | --- | --- | --- | --- |
| Claude Code | `~/.claude/projects/<encoded-cwd>/<session>.jsonl`, plus `subagents/` ([ref](https://brycewatson.com/blog/28-claude-code-deletes-old-logs)) | JSONL | Disk + SessionEnd hook (exists) | Deleted at startup after `cleanupPeriodDays`, default 30 ([ref](https://www.theregister.com/a/5264673)). The hook gets them before cleanup does |
| Codex CLI | `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` ([ref](https://pkg.go.dev/github.com/MoonCaves/rawclaw@v0.6.1/internal/source/codex)) | JSONL, `{type,timestamp,payload}` envelope | Disk | Not keyed by project: use `session_meta.cwd`. Some builds write `.jsonl.zst` ([ref](https://docs.rs/crate/codexusage/0.4.0/source/src/app/session_files.rs)). The documented and real formats differ ([ref](https://dev.to/milkoor/reverse-engineering-codex-cli-rollout-traces-3b9b)) |
| Cursor | `~/.cursor/projects/<slug>/agent-transcripts/<conv>/<conv>.jsonl` ([ref](https://docs.rs/crate/agentdiff/latest/source/scripts/capture-cursor.py)) | JSONL, plus legacy `.txt` | Disk + `stop` hook (`transcript_path`) | The transcript is thinner than Claude Code's ([ref](https://forum.cursor.com/t/richer-agent-transcripts-lifecycle-data-for-observability-langfuse-stop-hooks/166592)) and holds tool inputs but not outputs ([ref](https://forum.cursor.com/t/accessing-the-full-agent-transcript-in-cursor/157311)). `stop` can fire before the last lines are flushed. The multi-GB `state.vscdb` is skipped in 7.2.0; an opt-in, read-only adapter for it follows in 7.2.1 (#62) |
| GitHub Copilot (VS Code) | `<Code>/User/workspaceStorage/<hash>/chatSessions/<uuid>.json(l)`; `workspace.json` maps hash to folder ([ref](https://gist.github.com/cdeil/93ceacbdea17a7e744fb8c6ec95b3d9f)) | JSON before VS Code 1.109, JSONL mutation log after | Disk + upload of **Chat: Export Chat…** JSON ([ref](https://code.visualstudio.com/docs/agents/run/sessions/session-history)) | Read Stable and Insiders. Orphaned hashes are common, so match on `workspace.json`, not on the current hash |
| Kiro | IDE: `<Kiro>/User/globalStorage/kiro.kiroagent/` ([ref](https://github.com/kirodotdev/Kiro/issues/5469)); CLI: `kiro-cli/data.sqlite3` ([ref](https://www.pkgstats.com/pkg:kiro-history)) | IDE `.chat` JSON; CLI SQLite | Disk + upload of **Export Chat** zip ([ref](https://kiro.dev/docs/chat/chat-export/)) | Folders grow to many GB, so read incrementally. Kiro's SessionEnd is skipped in `hook.rs` today |
| Mistral Vibe | `~/.vibe/logs/session/` (or `session_logging.save_dir`, `VIBE_HOME`) ([ref](https://www.typingmind.com/skills/mistralai-vibe)) | To confirm on a real install | Disk | No session-end hook today (`hook.rs` skips it), so it's read from disk on import |
| OpenCode | `~/.local/share/opencode/opencode.db` (newer) ([ref](https://www.skills.sh/dzianisv/opencode-plugins/opencode-session-db)) or `storage/{session,message,part}/` JSON (older) ([ref](https://docs.rs/lore-cli/0.1.9/lore_cli/capture/watchers/opencode)) | SQLite (WAL) or JSON tree | Disk + existing plugin | Open read-only. Respect `OPENCODE_DATA_DIR` / `XDG_DATA_HOME`. Support both layouts, as the plugin does for both APIs |
| Claude Desktop | No local transcript store to rely on | n/a | Upload of the claude.ai export + MCP submit | Claude Code sessions started from the desktop app land with the other Claude Code transcripts and are caught there |
| claude.ai | Account export: Settings → Privacy → Export data | Zip with `conversations.json`, `projects.json` ([ref](https://univik.com/help/export-claude-chats.html)) | Upload + MCP submit | Chat only, with no tool exit codes, so mostly correction signals |
| Mistral Le Chat | Account export at `admin.mistral.ai/account/export` ([ref](https://help.mistral.ai/en/articles/347623-how-do-i-export-my-data-from-le-chat)) | Zip, format to confirm | Upload + MCP submit | Build the adapter from a real export |
| GitLab Duo | Server-side only. Conversations expire after 30 days of inactivity ([ref](https://docs.gitlab.com/user/gitlab_duo_chat/agentic_chat)) | n/a | MCP submit | No export exists ([ref](https://gitlab.com/gitlab-org/gitlab/-/issues/454171)) |
| Lumo | Server-side, encrypted. No native export ([ref](https://addons.mozilla.org/de/firefox/addon/lumo-chat-export/)) | n/a | MCP submit | Fits Lumo's privacy model: only what the agent chooses to submit ever leaves |
| Open Design | Unknown | Unknown | MCP submit | It's open source: read its storage code before 7.2.1 |

## Ingest routes

Five routes, one queue. Each one hands bytes to the same adapter registry, then the gate, then the queue, so the rules apply the same way whichever route a log arrives by.

1. **Desktop app (the client).** The tray's existing client detection (7.1.7's installed-clients menu) gains a **Session logs** submenu per detected client: *Import now*, *Import automatically after each session* (only where a hook exists) and *Stop*. Discovery reads the paths in the coverage table. The scan runs **on the user's machine**. When OmniMem's server is remote (a tailnet host, or OmniMem Cloud), only `CleanText` sessions are sent, never raw files.
2. **Settings panel upload (the web settings).** A new **Logs** page takes `.jsonl`, `.json`, `.zip` and `.jsonl.zst` by multipart upload. The browser runs the WASM gate over the file first and shows the findings, then the server runs the gate again on what arrives. It reuses the skill-bundle import's protections: declared-size reads against zip bombs, an entry allowlist, `MAX_UPLOAD_MB` (default 100) and per-entry caps. The adapter is detected from content, with a dropdown to override. **Preview** shows sessions found and findings per rule before **Import**. Raw upload bytes stay in memory and are dropped when the request ends.
3. **CLI.** `omnimem logs import <path>|--client <name>|--all`, `omnimem logs scan <path>` (dry run, findings only) and `omnimem logs sources` (what discovery finds and its state). `--db` works as it does for `install-hooks`.
4. **Hooks.** Claude Code's SessionEnd already reads the transcript. It now enqueues the path, and the daydream reads it after the hook returns, which keeps the hook inside its 30-second budget. Cursor gets a `stop` hook through `install-hooks --cursor`. Its payload carries `transcript_path`, but Cursor can fire `stop` before the file is flushed, so the job waits for the file size to stop changing. The OpenCode plugin adds a `session.idle` handler. Vibe and Kiro stay import-only until they gain a session-end hook.
5. **MCP: `submit_session_log`.** For clients with no files: claude.ai, Claude Desktop, Le Chat, Lumo, GitLab Duo and Open Design. The agent sends its own session as turns (role, text, tool name, outcome) at the end of a session or when asked. The server runs the gate on arrival. Records are marked `origin=agent_reported` and weigh half as much as disk transcripts in reinforcement, because the model chose what to report. This is the one new tool, taking the count from 49 to 50.

Every route de-duplicates on SHA-256 of the source file plus the session id. Append-only JSONL sources resume from their last byte offset, so re-importing a busy Claude Code folder costs only the new lines.

## Session model and storage

Every adapter maps its format into one normalised `Session`, defined in a new crate, `omnimem-transcripts`. By default **no transcript text is kept**: OmniMem stores an index of what it read and the signals it found, never the conversation.

`Session`: `id`, `client`, `client_version`, `models[]`, `project` (from the cwd, using the same rule as the hooks: `--project`, then `OMNIMEM_PROJECT`, then the folder name), `started_at`, `ended_at`, `origin` (`disk` | `upload` | `hook` | `agent_reported`), `source_ref` {path, sha256, byte range}, `turns[]`.

`Turn`: `index`, `role`, `text: CleanText`, `ts`, `tool_calls[]` {`name`, `input_summary: CleanText`, `status`, `exit_code`, `error_class`}. Field names follow the OpenTelemetry GenAI conventions where one exists, so an OTLP receiver can map onto the same model later without a schema change.

Schema 7 (from 6) adds four tables:

| Table | Holds | Never holds |
| --- | --- | --- |
| `transcript_sources` | Client, path, discovery state, last byte offset, enabled flag | File contents |
| `transcript_sessions` | Session metadata, `source_ref`, findings report (counts per rule), quarantine flag, daydreamed-at | Turn text, unless retention is on |
| `daydream_signals` | Detector, session, turn index, tool name, error class, a short `CleanText` excerpt | Raw text |
| `scrub_rules` / `scrub_fingerprints` | Custom rules, allowlist entries, salted hashes and lengths of known secrets | Secret values |

`TRANSCRIPT_RETENTION` is `none` by default on every kind of install, desktop and hosted alike. `scrubbed:<days>` is opt-in and keeps scrubbed turns, so the detectors can be re-run after a rule or detector improves. Backups, `export`, `restore_from_file` and Litestream carry the new tables, with no retained text unless retention is on. Older readers stop at schema 7, as they do for schema 6 today.

## Daydream: detectors and proposals

Detection is deterministic, like the skill compiler, so the same sessions always give the same signals and proposal diffs stay meaningful. An LLM is used only to reword a proposal that's already been made, only on `CleanText`, and only when `DAYDREAM_LLM=on`.

### Detectors (`omnimem-engine/src/daydream.rs`)

| Detector | Fires on | Works for |
| --- | --- | --- |
| `tool_error` | A tool result marked as an error, a non-zero exit code, or a known error class (compile error, test failure, 4xx/5xx, permission denied) | Disk clients with tool results; Cursor only partly (inputs, no outputs) |
| `retry_loop` | The same tool with near-identical input 3 or more times within 8 turns | All tool-using clients |
| `user_correction` | A user turn right after an assistant turn that matches the correction lexicon ("no,", "that's wrong", "not what I asked", "undo", "revert", "stop") | Every client, including chat-only ones. The lexicon is configurable |
| `revert` | A file edited, then restored (`git checkout`/`restore`, or an edit back to the earlier content) | Disk clients |
| `abandoned` | The session ends within 2 turns of a failure, with no later success | All |
| `recovery` | A failure followed by success on the same goal (tests pass, the build goes green, the user says thanks) | All |
| `guard_hit` | OmniMem's own PreToolUse refusal appears in the transcript | Hook clients. Counts as evidence the graveyard is working |

### From signals to proposals

- A **failure followed by recovery** is the main product. It becomes a `record_experience` proposal: the failed approach goes into `abandoned_approaches` and the fix into `breakthrough`. This is "gains experience" taken from real sessions.
- A **repeated failure with no recovery** becomes a `log_abandoned` proposal, which in time feeds the graveyard and the skill compiler's Don't rules.
- **Repeated corrections on one topic** become a `preference` candidate, worded as the user's own correction, quoted from the scrubbed text.

### Reinforcement gate

A signal becomes a proposal only once it recurs in at least `DAYDREAM_MIN_SESSIONS` (default 2) distinct sessions, counted over the **whole** history, not the latest batch, to avoid recency drift. A pattern seen in only one client is scoped to that client, with `origin_client` set and a `client:<name>` tag. One seen in 2 or more clients is project-wide. `agent_reported` sessions count half. That per-client tally is also the evidence the 7.x model-routing work needs.

### Landing

Proposals are written with `trust=candidate`, `provenance=concluded`, `origin=daydream`, `origin_client`, `origin_model` and `source_refs` (session id plus turn range). They go to the settings panel's existing **Pending** list under `HUMAN_APPROVAL`. Nothing daydreamed is ever `accepted` without a person. A rejected proposal stops that signal key from being proposed again, and `rejected` memories never feed the graveyard, as 7.0.11 already enforces.

## Settings panel and desktop UX

One new sidebar page, **Logs**, with four tabs. It follows the 7.1 panel rules: no inline scripts, `script-src 'self'` CSP, the `compact` number filter, and the WCAG 2.2 AA checks the panel already meets. The one exception is that the Logs page's own CSP adds `'wasm-unsafe-eval'`, which the in-browser scan needs. No other page gets it.

- **Sources.** One row per guide client: detected (yes/no), path, enabled toggle, last import, sessions, quarantined. Chat-only clients show "MCP submit" with a copy-able snippet for the agent's instructions, and a link to that client's export page.
- **Upload.** File picker, adapter override, an in-browser scan by the WASM gate, **Preview** (sessions found and findings per rule, never the values), then **Import**. The server never persists raw upload bytes. The same flow backs the desktop app's *Import file…*.
- **Scrub rules.** Built-in rules (read-only, hit counts over the last 30 days), custom rules (gitleaks TOML, with a test box that shows match or no match on pasted sample text that's never stored), allowlist, and known secrets (add a value or a file: shown only as label, length and date).
- **History.** Imports newest first: date, route, client, sessions, findings, quarantines, proposals made. Quarantined sessions can be released or deleted from here.

Proposals appear in the existing **Pending** list with an origin badge (client icon plus "from 3 sessions"). The dashboard gets one tile: "Daydreamed this week: N proposals waiting".

On the desktop, the tray shows a **Session logs** submenu only for clients that are installed, matching 7.1.7's installed-only menu. The Flatpak asks for read-only access to each client's own folder and nothing wider: `~/.claude:ro`, `~/.codex:ro`, `~/.cursor:ro`, `~/.vibe:ro`, `xdg-data/opencode:ro`, `xdg-config/Code:ro` and `xdg-config/Kiro:ro`. It never asks for the home folder. Discovery then works inside the sandbox as it does outside, *Import file…* (through the file-chooser portal) covers anything kept elsewhere, and the Flatpak's software centre page says what the access is for.

Claude Code deletes transcripts after `cleanupPeriodDays` (30 by default). When daydreaming for Claude Code is switched on, the tray offers once to raise it in `~/.claude/settings.json`, changing it only on a yes and after a backup, as `install-hooks` does. It never touches `~/.claude.json`. If the offer is declined, the Logs page says how much history Claude Code keeps.

## Configuration

All settings go through `omnimem_core::env`, so `omnimem.env`, the Configuration page and hooks read them the same way. The defaults are chosen so that upgrading to 7.2.0 changes nothing until someone imports a log.

| Variable | Default | Values |
| --- | --- | --- |
| `DAYDREAM` | `manual` | `off` · `manual` (import and Daydream now only) · `hooks` (also after each hooked session) |
| `DAYDREAM_SOURCES` | every detected client | Comma-separated client ids |
| `DAYDREAM_MIN_SESSIONS` | `2` | Integer, at least 1 |
| `DAYDREAM_LLM` | `off` | `off` · `on` (rewords proposals; needs the existing LLM config) |
| `DAYDREAM_CORRECTION_LEXICON` | built-in English list | Path to a file, one phrase per line |
| `TRANSCRIPT_RETENTION` | `none` | `none` · `scrubbed:<days>` |
| `SCRUB_RULES_FILE` | none | Path to a gitleaks-format TOML, merged with the built-in rules |
| `SCRUB_PII` | `off` | `off` · comma list of `email`, `phone`, `ip`, `tailnet` |
| `SCRUB_QUARANTINE_BLOCKS` | `3` | Block-severity hits per session before it's held |
| `MAX_UPLOAD_MB` | `100` | Integer |

## Testing and acceptance criteria

7.2.0 ships when every box below is ticked, on top of the usual suite, clippy and `cargo audit` run before the release push (the 7.0.6 lesson).

- [ ] **Canary test.** Fixtures for every adapter carry planted fake secrets of every built-in rule type. After a full import and daydream, a byte search of `omnimem.db`, the WAL, the audit log, captured `tracing` output, the embedder's inputs and a mocked LLM's request log finds none of them.
- [ ] **Gate property tests** (proptest): for any input, no span matched by any rule survives `scan()`; `scan()` is idempotent; output length is bounded.
- [ ] **Fuzzing** (`cargo-fuzz`) on every adapter's parser and on the gate, 1 hour each in CI nightly. No panics, no unbounded allocation.
- [ ] **False-positive corpus**: real-looking non-secrets (git SHAs, UUIDs, base64 images, lockfile hashes) stay under a set redaction rate, so transcripts stay readable.
- [ ] **gitleaks compatibility**: the upstream gitleaks rule file loads, and its own test cases pass for the rules OmniMem supports.
- [ ] **Golden tests per adapter**, from real sessions scrubbed by hand: Claude Code, Codex (plain and `.zst`), Cursor (JSONL and legacy `.txt`), Copilot (JSON and JSONL), Kiro (IDE and CLI), Vibe, OpenCode (SQLite and JSON), claude.ai export, Le Chat export.
- [ ] **Detector golden tests**: a fixed corpus gives byte-identical `daydream_signals` across runs and platforms.
- [ ] **Upload hardening**: zip bomb, path traversal in zip entries, oversize, wrong content type and a truncated `.zst` are all refused cleanly. The 7.0 security review's method (split by attack surface) runs over the new routes.
- [ ] **End to end on Ric's machine**: one week of real sessions from at least 4 clients gives proposals that Ric accepts at a rate he's happy with. Record the number in the release notes.
- [ ] **Docs**: `docs/daydream.md`, a "Session logs" section in each of the 13 guides, `docs/configuration.md`, `docs/mcp-tools.md` (50 tools) and a CHANGELOG security section for the gate.

## Delivery plan on v7.2.x

The gate lands first and can ship alone. It improves the existing SessionEnd record even before any daydream exists. After that, the adapters are independent of each other and can be built in parallel.

1. **Branch.** Done: `v7.2.x` cut from `v7.1.x` at c296558 and set to `7.2.0-dev`, with the 7.2.0 milestone and an issue per step. This spec's decisions are #43.
2. **`omnimem-scrub`** (#44). Crate, `RawText` / `CleanText` types, built-in rule pack, gitleaks loader, fingerprints, findings report. Replace `redact()` in `hook.rs`. Property tests, a fuzz target and the WASM build.
3. **Schema 7** (#45). The four tables, with backup, export and restore support, and the older-reader refusal.
4. **`omnimem-transcripts` and the first adapter** (#46). `Session` / `Turn`, the adapter trait, content-based detection. Claude Code first, built from the existing `read_transcript()`.
5. **Remaining disk adapters**, in parallel: Codex (#47), Cursor (#48), Copilot (#49), Kiro (#50), Vibe (#51), OpenCode (#52). Gotcha: agent worktrees in this repo branch from `main`, not from the checked-out branch. Rebase each onto `v7.2.x` before merging.
6. **Export adapters**: claude.ai (#53) and Le Chat (#54), each built from a real export.
7. **`submit_session_log`** MCP tool (#55), with a snippet in each chat-client guide.
8. **Routes**: CLI subcommands, queue, Cursor `install-hooks --cursor` and the OpenCode `session.idle` handler (#56); the Logs page with its in-browser scan (#57); the tray submenu, the Flatpak's read-only folder access and the Claude Code retention offer (#58).
9. **Daydream** (#59): detectors, reinforcement gate, proposals into Pending, the dashboard tile.
10. **Hardening and release** (#60): security review by attack surface, canary test, docs for all 13 guides, CHANGELOG, then tag `v7.2.0`.

Patch releases after that (7.2.1 and on): the Open Design adapter (#61), the opt-in read of Cursor's `state.vscdb` (#62), and fixes as client formats change. Thin slicing and model routing follow as 7.3.x.

## Decisions on the open questions

The first draft of this spec ended with seven open questions. Ric settled them on 2026-11-20, and the sections above now say what was decided.

| Question | Decision |
| --- | --- |
| Version clash with model routing | Thin slicing and model routing move to **7.3.x**. Daydream's per-client tallies land first |
| One new MCP tool, or a mode | A new tool, `submit_session_log`, taking the count from 49 to 50. Its input is structured turns, not a document, so a mode of `remember_document` would have been the wrong shape |
| Browser-side scanning | **In 7.2.0.** `omnimem-scrub` builds for WASM and the Logs page scans before upload. `'wasm-unsafe-eval'` goes in the CSP of the Logs page only |
| Flatpak discovery | Ask for filesystem access: read-only, one path per client, never broad home access |
| Retention default | `TRANSCRIPT_RETENTION=none` everywhere. `scrubbed:<days>` stays opt-in |
| Cursor's `state.vscdb` | An opt-in, read-only adapter in a 7.2.x patch (#62), off by default, with a size guard |
| Claude Code retention | The tray offers once to raise `cleanupPeriodDays` when Claude Code daydreaming is switched on, with consent and a backup |
