# Changelog
All notable changes to omnimem are documented here.
Format: [version] - date - description

## [Unreleased]

## [0.2.2] - 2026-03-17
### Fixed
- **RSS summariser retry on transient errors**: Haiku API calls now retry up to 2 times on timeouts, rate limits, server errors, and connection errors with backoff, instead of immediately falling back to truncation.
- **Better fallback summaries**: Fallback truncation limit increased from 300 to 800 characters so articles that miss summarisation are still useful.
- **Summariser error logging**: Log the full error message on API failures, not just the exception type.

## [0.2.1] - 2026-03-17
### Fixed
- **RSS scheduler misses on sleeping machines**: APScheduler's default `misfire_grace_time` of 1 second meant every scheduled ingestion was silently skipped when the host machine was asleep at fire time. Set `misfire_grace_time` to the full interval window and `coalesce=True` so missed jobs execute on wake (once, not per missed cycle).

## [0.2.0] - 2026-03-10
### Added
- **Semantic deduplication**: `remember()` now checks for near-identical memories before storing (threshold configurable via `DEDUP_SIMILARITY_THRESHOLD`, default 0.92). Use `force=True` to override. New `find_duplicates` tool scans a namespace and returns clusters of duplicate memories using pairwise cosine similarity with union-find clustering.
- **Contradiction detection**: Two-tier system — Tier 1 (fast keyword heuristic with negation pattern matching) runs automatically on every `remember()` call and warns about potential contradictions. Tier 2 (Claude API analysis) available on-demand via `check_contradictions` tool. Contradictions are cross-linked between memories and surfaced in `recall()` results.
- **Briefing tool**: Single-call `briefing(project="...")` replaces the previous 3-step session start. Aggregates project context, experience summary, stale memories (configurable via `STALE_MEMORY_DAYS`), new knowledge articles, contradiction warnings, reinstate candidates, and suppressed topics.
- New env vars: `DEDUP_SIMILARITY_THRESHOLD`, `CONTRADICTION_SIMILARITY_THRESHOLD`, `STALE_MEMORY_DAYS`
- `contradictions` field added to episodic memory return fields, RecallResult, and explain_memory output
- Updated CLAUDE.md to use briefing tool for session start workflow

## [0.1.0] - 2026-03-09
### Added
- Valkey + valkey-search vector store with HNSW indexing
- Three memory namespaces: episodic, project, knowledge
- Memory lifecycle state machine: active -> deprioritised -> archived -> deleted
- Surface score weighting with topic suppression
- Reinstate hints for graceful memory retirement
- Recall pipeline: semantic search + surface score + recency decay + experience weight
- Experience scoring: effort_score (1-5), outcome, iterations, abandoned_approaches, breakthrough, gotchas
- Abandoned approach fast-path — warnings bypass normal scoring
- High-effort lifecycle guard — warns before deprioritising battle-hardened memories
- Core MCP tools: remember, recall, deprioritise, archive, reinstate, forget, suppress_topic
- Project context tools: set_project_context, get_project_context, list_projects, update_project_state
- Experience tools: record_experience, log_abandoned, get_experience, experience_summary, warn_if_abandoned
- Audit tools: memory_audit, why_did_you_mention, explain_memory
- Backup tools: dump_to_file, restore_from_file, list_backups
- RSS ingestion worker with dedup, scheduling, and Claude-powered summarisation
- Slim Debian-based Docker images (torch CPU-only for embedding support)
- CLAUDE.md integration file for drop-in project wiring
