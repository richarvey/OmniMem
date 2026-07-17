# MCP Tool Reference

Every tool the OmniMem MCP server exposes, grouped by area. The server delivers usage instructions to connecting agents automatically via the MCP `instructions` field, so agents learn most of this on connect.

## Core memory

| Tool | What it does |
|---|---|
| `remember(content, project?, tags?, force?, mode?)` | Store a memory. In `full` mode (default) extracts atomic facts via Claude Haiku and routes preferences to the preference namespace; `raw` stores verbatim. Auto-checks for duplicates and contradictions |
| `remember_document(content, chunk_strategy, project?, tags?, namespace?, chunk_size?, mode?)` | Index a long-form document by splitting it into chunks (`turn_pairs`, `sentences`, `paragraphs`, or `fixed_tokens`) and storing each as a memory linked by a shared `doc_id` |
| `recall(query, top_k?, project_filter?, expand_queries?)` | Semantic search across all namespaces. With `expand_queries=true`, generates alternative phrasings via Claude Haiku and unions the results to improve recall coverage when query vocabulary doesn't match stored content |
| `deprioritise(key_or_query, reason, reinstate_hints?)` | Soft-suppress without deleting |
| `archive(key_or_query)` | Remove from recall but keep for history |
| `reinstate(key_or_query)` | Bring a deprioritised memory back |
| `retag(key, tags?, add?, remove?)` | Replace or adjust a memory's tags without re-embedding. Pass `tags` for a full replacement (`[]` clears), or `add`/`remove` to tweak the existing set |
| `forget(key_or_query, confirm=True)` | Hard delete, requires explicit confirmation |
| `suppress_topic(topic)` | Filter a topic from all future recalls |
| `unsuppress_topic(topic)` | Remove a topic from the suppression list |
| `list_suppressions()` | Show all currently suppressed topics |
| `find_duplicates(namespace?, threshold?, project_filter?)` | Scan for clusters of near-identical memories |
| `check_contradictions(query?, namespace?, use_api?)` | Detect memories that contradict each other |
| `briefing(project?, include_knowledge?)` | Single-call session start with full context |

## Project context

| Tool | What it does |
|---|---|
| `set_project_context(name, description, stack, goals, current_state)` | Create or update project memory |
| `get_project_context(name)` | Retrieve it, called at every session start |
| `update_project_state(name, current_state, notes?)` | Update state without re-embedding |
| `compile_project_context(name, auto_save?)` | Auto-produce or refresh a project context from its episodic memories, tags, experience data, and abandoned approaches |
| `list_projects()` | See all stored projects |
| `delete_project(name, confirm?, include_context?)` | Bulk delete every memory belonging to a project by direct key scan (no semantic search, so nothing gets missed). Preview by default; `confirm=True` deletes in pipelined batches; `include_context=True` also removes the project context entry |

## Experience scoring

| Tool | What it does |
|---|---|
| `record_experience(key, effort_score, outcome, abandoned_approaches?, breakthrough?, gotchas?)` | Log how hard it was and what failed |
| `log_abandoned(key, name, type, reason)` | Add dead ends incrementally mid-session |
| `warn_if_abandoned(query)` | Check the graveyard before proceeding |
| `experience_summary(project?)` | Graveyard, breakthroughs, and effort stats |
| `get_experience(key)` | Full experience data for one memory |

## Skill compiler

| Tool | What it does |
|---|---|
| `compile_skill(domain, mode?, min_reinforcement?, include_graveyard?, export_path?, description?)` | Compile a domain's experience and graveyard into a `SKILL.md`. `propose` (default) returns a reviewable diff and change summary; `write` commits only a previously proposed and accepted draft — no silent writes. `export_path` mirrors the file under `SKILL_EXPORT_DIR` |
| `find_skills(query_or_domain)` | Ranked skill discovery over indexed metadata. Exact domain matches lead; a hand-authored skill outranks a generated one on the same domain |
| `get_skill(skill_id)` | Load the whole skill body intact, by key (`mem:skill:gen:python-ric`), name (`python-ric`), or bare domain (`python`) |
| `bless(memory_key)` | Promote one strong lesson to skill-eligible now, bypassing the reinforcement threshold at the next compile |

See [skill-compiler.md](skill-compiler.md) for how compilation and the propose-and-accept gate work.

## Knowledge

| Tool | What it does |
|---|---|
| `recent_knowledge(days?, feed_name?, topics?, limit?)` | Query recent RSS articles with optional filters, sorted newest first |
| `promote_knowledge(key, domain?, demote?, rules?)` | Mark an article as permanently useful by clearing its expiry. With `domain`, also mark it skill-eligible: the next `compile_skill()` for that domain renders it in the skill's Reference section. Pass `rules=[{kind, text}, ...]` to extract an article's discrete guidance into individual stance-prefixed Reference rules (reviewed at promotion, so compiles stay deterministic). `demote=True` removes a domain again |

## Audit and backup

| Tool | What it does |
|---|---|
| `memory_audit(project?, namespace?, limit?, offset?)` | All memories by state; full state-count summary plus a paginated `entries` list (default 100, max 500) |
| `explain_memory(key)` | Full history for a single memory |
| `why_did_you_mention(query)` | Debug why something surfaced |
| `dump_to_file(filename?)` | Export everything to a timestamped JSON file |
| `restore_from_file(filename, dry_run?)` | Restore from backup, merges rather than overwrites, re-embeds for immediate recall |
| `list_backups()` | See available backup files |
| `health()` | Server, Valkey, index, and model status |
| `queue_status()` | Enrichment queue depth — poll until `pending` reaches 0 after batch ingest before running recall/scoring |
| `reindex(namespace?)` | Drop and recreate Valkey search indexes to clear orphaned vector entries. Data-safe |
| `version()` | Return the current OmniMem version |
