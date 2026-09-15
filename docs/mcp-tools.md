# MCP Tool Reference

All 48 tools, grouped by what they're for. The names, parameters, defaults and descriptions are exactly what 6.x sent, so an agent that knew OmniMem 6 knows OmniMem 7.

You don't have to teach your agent any of this. The server sends its operating instructions in the MCP `instructions` field on connect, and each tool carries its own description, so agents pick most of it up by themselves. This page is for you.

The server lives at `http://127.0.0.1:8765/mcp` (streamable HTTP; SSE has gone).

## Core memory

| Tool | What it does |
|---|---|
| `remember(content, project?, tags?, namespace?, force?, mode?, licence?, provenance?)` | Store a memory with automatic dedup. `namespace` is `episodic` (default), `project`, `knowledge` or `preference`. In `full` mode (the default, from `INGEST_MODE`) facts are extracted in the background and preferences routed to the preference namespace; `raw` stores it verbatim. Returns the near-duplicate instead of storing when one exists, unless `force=True`. `licence` defaults to `own` (or `unknown` for knowledge) and `provenance` to `concluded` (`asserted` for preferences, `retrieved` for knowledge) |
| `remember_document(content, chunk_strategy?, project?, tags?, namespace?, chunk_size?, mode?, licence?, provenance?)` | Index a long document as linked chunks sharing a `doc_id`. Strategies: `turn_pairs`, `sentences`, `paragraphs` (default) or `fixed_tokens` (`chunk_size` words, default 200). `licence` and `provenance` apply to every chunk |
| `recall(query, top_k?, namespaces?, project_filter?, expand_queries?, domain_filter?)` | Semantic search across the namespaces, abandoned-approach warnings first. `top_k` (default 5) is a ceiling: results under `RECALL_MIN_SCORE` are dropped and ones in the weak band are flagged `weak_match`. `domain_filter` searches every project declaring a work-type domain. `expand_queries` rephrases the query with Claude Haiku. A trailing `licence_notice` lists results nobody has classified yet |
| `recall_index(query, top_k?, namespaces?, project_filter?, snippet_length?, expand_queries?, domain_filter?)` | The lightweight version: ranked snippets (150 characters by default) and keys, no full content. `top_k` defaults to 10 |
| `recall_detail(keys)` | Full content for chosen keys, usually after `recall_index()` |
| `deprioritise(key_or_query, reason, reinstate_hints?)` | Turn a memory down to 0.2x without deleting it. Hints bring it back when a later query matches them |
| `archive(key_or_query, reason?)` | Keep it stored but out of recall |
| `reinstate(key_or_query)` | Bring a deprioritised or archived memory back to active |
| `retag(key, tags?, add?, remove?)` | Replace (`tags`, `[]` clears) or adjust (`add`, `remove`) a memory's tags without re-embedding |
| `forget(key_or_query, confirm?)` | Permanent delete. Without `confirm=True` it only previews |
| `suppress_topic(topic, reason?)` | Filter a topic out of every recall until you lift it |
| `unsuppress_topic(topic)` | Lift a suppression |
| `list_suppressions()` | What's currently suppressed |
| `find_duplicates(namespace?, threshold?, project_filter?)` | Clusters of near-identical memories (threshold 0.92 by default) |
| `check_contradictions(query?, namespace?, project_filter?, use_api?)` | Tier 1 heuristic scan, or tier 2 verification with Claude when `use_api=True` |
| `briefing(project?, include_knowledge?)` | The session-start call: project context, experience summary, stale memories, new articles, contradictions, reinstate candidates and the skill sections. Runs auto-maintenance and the auto skill scan on their intervals |

## Projects

| Tool | What it does |
|---|---|
| `set_project_context(project_name, description, stack, goals, current_state, notes?, domains?)` | Create or update a project. `domains` declares its kinds of work (`["python", "docker"]`); leave it out to keep the existing ones, pass `[]` to clear them |
| `get_project_context(project_name)` | The whole context for one project |
| `list_projects(domain?)` | Every project, or only those declaring a domain |
| `update_project_state(project_name, current_state, notes?)` | Update the state and notes without re-embedding |
| `compile_project_context(project_name, auto_save?)` | Draft a context from the project's stored memories, experience and dead ends, with suggested domains |
| `compile_project_domains(project_name, auto_save?)` | Suggest domains from the stack and the project's own recurring tags, with the evidence for each. Never removes one |
| `delete_project(project_name, confirm?, include_context?)` | Delete every memory in a project by key, not by search, so nothing gets missed. Previews without `confirm=True` |
| `deprioritise_project(project_name, confirm?, reason?, include_context?)` | Turn a whole project down to 0.2x, reversibly |
| `reinstate_project(project_name, confirm?, include_context?)` | Undo that |

## Experience and the graveyard

| Tool | What it does |
|---|---|
| `record_experience(key, effort_score, outcome, iterations?, abandoned_approaches?, breakthrough?, gotchas?, lesson?)` | How hard it was (effort 1 to 5), how it ended (`succeeded`, `pivoted`, `abandoned`), what failed, what worked, and the `lesson` that transfers. Abandoned high-effort work auto-suppresses the approach names |
| `log_abandoned(key, name, type, reason)` | Add one dead end mid-session. `type` is `library`, `approach`, `tool`, `pattern` or `service` |
| `warn_if_abandoned(query)` | Check the graveyard before suggesting something |
| `experience_summary(project?)` | Effort stats, outcomes, the graveyard and breakthroughs |
| `get_experience(key)` | Experience data for one memory |

## Skills

| Tool | What it does |
|---|---|
| `compile_skill(domain, mode?, min_reinforcement?, include_graveyard?, export_path?, description?)` | Compile a domain's experience into a `SKILL.md`. `propose` (default) returns a reviewable diff and stashes it; `write` commits only that stashed, reviewed draft. `export_path` mirrors the file under `SKILL_EXPORT_DIR` |
| `find_skills(query_or_domain)` | Ranked skills for a domain or a description of the work, above `SKILL_MIN_SCORE` (0.25) |
| `get_skill(skill_id)` | The whole skill body, by key, name or bare domain |
| `bless(memory_key)` | Let one strong lesson past the reinforcement gate at the next compile |
| `promote_knowledge(key, domain?, demote?, rules?)` | Keep an article forever, and with `domain` feed it into that skill's Reference section, optionally as extracted `rules` |

See [the skill compiler](skill-compiler.md) for how the gate works.

## Knowledge and classification

| Tool | What it does |
|---|---|
| `recent_knowledge(days?, feed_name?, topics?, limit?, licence?)` | Recent RSS articles, newest first. `days` defaults to 7 (max 365), `limit` to 20 (max 50). `licence="unknown"` is the classify queue |
| `set_licence(licence, keys?, feed_name?, note?)` | Record redistribution rights on stored memories, by key (up to 200) or for every article from one feed. Accepts a class or a recognised identifier such as `cc-by-4.0` |
| `set_provenance(provenance, keys)` | Reclassify where memories came from: `asserted`, `concluded` or `retrieved` (up to 200 keys) |

Extracted facts follow their source in both cases, and neither touches `updated_at`, so the skill compiler doesn't think the source changed.

## Audit, backup and housekeeping

| Tool | What it does |
|---|---|
| `memory_audit(project?, namespace?, include_archived?, limit?, offset?)` | Counts by state for the whole store, plus a paginated list (100 by default, 500 at most) |
| `explain_memory(key)` | Full metadata for one memory, licence and provenance included |
| `why_did_you_mention(query)` | Search the recall logs to see why something surfaced |
| `dump_to_file(filename?)` | Export everything to a JSON backup in `BACKUP_DIR`, in the 6.x format |
| `restore_from_file(filename, dry_run?)` | Restore a backup from `BACKUP_DIR`, merging rather than overwriting and re-embedding as it goes. Previews unless `dry_run=False` |
| `list_backups()` | Backup files, newest first |
| `health()` | Database status, record and vector counts, model status, uptime |
| `queue_status()` | How many fact-extraction jobs are waiting. Poll it after a batch ingest |
| `reindex(namespace?)` | Reload the in-memory vectors from the database and report the counts. Kept for 6.x clients: there's no separate index to drift any more |
| `version()` | The OmniMem version |
