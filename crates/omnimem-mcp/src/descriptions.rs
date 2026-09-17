//! Tool descriptions, from the 6.x docstrings.
//!
//! FastMCP sent the whole docstring, `Args:` block included. Those blocks
//! restated the JSON schema the client already has, in prose, in every
//! session, so they were stripped: 11,623 bytes of the 19,610 went, and the
//! longer guidance moved to `docs/agent-guide.md`. Two `Args:` blocks
//! survive where they say something the schema does not.

pub const VERSION: &str = "Return the current OmniMem version.";

pub const REMEMBER: &str = r"Store a memory with automatic dedup. Returns duplicate info if near-match exists; use force=True to override.";

pub const REMEMBER_DOCUMENT: &str = r"Index a long-form document by splitting it into chunks and storing each chunk as a memory.

Use this instead of remember() for conversation transcripts, articles, meeting notes,
or any content longer than a focused fact. Returns the list of inserted keys plus a
shared doc_id so callers can clean up or group them later.";

pub const RECALL: &str = r"Search memories by semantic similarity. Returns ranked results; abandoned-approach warnings appear first.

Every result carries its `licence` and `provenance`. When any result's
licence is still unknown, a trailing entry with result_type
'licence_notice' (no key) lists them so the human can classify.

top_k is a ceiling, not a quota. Results below the relevance floor
(RECALL_MIN_SCORE, default 0.15) are dropped instead of padding the list,
so fewer results than you asked for — including none — is a normal answer.

A result flagged `weak_match` scored inside the band where relevant and
irrelevant results genuinely overlap on this model. Read it, but do not
build on it and do not go looking for a connection to the query: if it
is not obviously about what you asked, it isn't.";

pub const RECALL_INDEX: &str = r"Lightweight recall: returns ranked summaries without full content. Use recall_detail() to fetch full content for selected keys.

Shares recall()'s relevance floor (RECALL_MIN_SCORE, default 0.15) and
its `weak_match` flag, so top_k is a ceiling and a short or empty result
set is a real answer.";

pub const RECALL_DETAIL: &str = r"Fetch full content for specific memory keys. Use after recall_index() to expand only the entries you need.";

pub const DEPRIORITISE: &str = r"Reduce a memory's visibility without deleting. Accepts a key or natural language query.";

pub const ARCHIVE: &str = r"Archive a memory — excluded from recall but still stored.";

pub const REINSTATE: &str = r"Reinstate a deprioritised/archived memory to active state.";

pub const RETAG: &str = r"Replace or adjust the tags on a memory without re-embedding it.";

pub const FORGET: &str = r"Permanently delete a memory. Requires confirm=True; returns preview otherwise.";

pub const SUPPRESS_TOPIC: &str = r"Suppress a topic — matching memories filtered from recall.";

pub const UNSUPPRESS_TOPIC: &str = r"Remove a topic from the suppression list.";

pub const LIST_SUPPRESSIONS: &str = "List all suppressed topics.";

pub const FIND_DUPLICATES: &str = r"Scan a namespace for clusters of near-identical memories.";

pub const HEALTH: &str =
    "Server health: database status, record and vector counts, model status, uptime.";

pub const QUEUE_STATUS: &str = r"Check the enrichment queue. Returns the number of pending jobs waiting for background fact extraction.

Use this to know when enrichment has finished after a batch ingest —
poll until pending reaches 0 before running recall/scoring.";

pub const DUMP_TO_FILE: &str = r"Export all memories to a JSON backup file.";

pub const RESTORE_FROM_FILE: &str = r"Restore memories from backup. Default dry_run=True previews without writing. Merges with existing data.";

pub const LIST_BACKUPS: &str = "List available backup files, newest first.";

// Phase 3: experience, projects, audit, classification, contradictions,
// briefing and knowledge.

pub const RECORD_EXPERIENCE: &str = r#"Record effort, outcome, dead ends, and breakthroughs for a memory. High-effort successes surface more; abandoned approaches come back as a warning when a later session proposes one of them.

    Args:
        key: Memory key to attach experience to.
        effort_score: 1-5 (1=trivial, 3=moderate, 5=battle-hardened).
        outcome: 'succeeded', 'pivoted', or 'abandoned'.
        iterations: Number of attempts.
        abandoned_approaches: List of dicts with 'name', 'type', 'reason'.
        breakthrough: What finally worked, on this occasion.
        gotchas: Caveats to watch for.
        lesson: The generalisable claim this work taught, if there is one:
            a rule that holds beyond this incident, e.g. "a test fake must
            reproduce the real server's divergent behaviour, not its docs".
            Leave it out when nothing transfers. Skill compilation prefers
            it over the breakthrough."#;

pub const LOG_ABANDONED: &str = r"Append a dead-end approach to a memory's abandoned list.";

pub const GET_EXPERIENCE: &str = r"Return experience data for a memory key.";

pub const EXPERIENCE_SUMMARY: &str = r"Aggregate experience stats: effort, outcomes, graveyard of abandoned approaches, breakthroughs.";

pub const WARN_IF_ABANDONED: &str = r"Check if an approach was previously abandoned. Call before suggesting libraries or tools.";

pub const SET_PROJECT_CONTEXT: &str = r"Create or update a project's context (description, stack, goals, state).";

pub const GET_PROJECT_CONTEXT: &str = r"Retrieve full context for a project by name.";

pub const LIST_PROJECTS: &str = r"List all stored project contexts, deduplicated by project name.";

pub const COMPILE_PROJECT_DOMAINS: &str = r"Suggest work-type domains for a project from its stack and its own memories, with the evidence behind each one. Returns a draft by default; pass auto_save=True to store it.

    Domains are what makes cross-project recall work: once projects declare
    them, recall(domain_filter='python') searches every Python project at
    once. They share the compiled-skill vocabulary, so the same names reach
    find_skills() and get_skill().

    Suggestions are merged with any domains the project already declares —
    this never removes one.";

pub const UPDATE_PROJECT_STATE: &str = r"Update a project's current state and notes without re-embedding.";

pub const DELETE_PROJECT: &str = r"Bulk delete every memory belonging to a project. Requires confirm=True; returns a preview otherwise.

    Finds memories by scanning keys directly (no semantic search), so it
    catches everything — including memories that recall can't surface.
    Deletes in pipelined batches rather than one call per key.";

pub const DEPRIORITISE_PROJECT: &str = r"Bulk deprioritise every active memory in a project (0.2x recall visibility, reversible). Requires confirm=True; returns a preview otherwise.

    Like delete_project but non-destructive: memories stay stored and searchable,
    just heavily down-weighted in recall. Undo the whole project with
    reinstate_project(). Only memories currently in the active state are changed;
    already-deprioritised or archived ones are reported under `already_inactive`.";

pub const REINSTATE_PROJECT: &str = r"Bulk reinstate every deprioritised or archived memory in a project back to active. Requires confirm=True; returns a preview otherwise. The inverse of deprioritise_project().";

pub const COMPILE_PROJECT_CONTEXT: &str = r"Gather all stored memories for a project and compile them into a structured context draft. Use this before set_project_context() to auto-produce or refresh a project's context from its episodic memories, experience data, and abandoned approaches.";

pub const MEMORY_AUDIT: &str = r"Summary of all memories grouped by state. Useful for cleanup.

    The state counts always cover the whole store; the per-memory ``entries``
    list is paginated so a large store doesn't return thousands of rows at once.";

pub const WHY_DID_YOU_MENTION: &str = r"Explain why a topic surfaced by searching recall logs.";

pub const EXPLAIN_MEMORY: &str = r"Return full metadata for a memory key.";

pub const REINDEX: &str = r"Rebuild the in-memory vector index from the database.

    Kept for compatibility with 6.x clients. There is no separate search index
    any more, so nothing can drift from the records and no phantom entries
    are ever removed; this reloads the vectors and reports the counts.";

pub const SET_LICENCE: &str = r"Record the redistribution rights of one or more memories.

    Give either specific keys, or a feed_name to classify every article
    ingested from that RSS feed at once. Use it when recall reports results
    with an unknown licence and the human can say where the content stands,
    or to correct a wrong classification. Facts extracted from a classified
    memory take the same licence automatically.";

pub const SET_PROVENANCE: &str = r"Record where one or more memories came from.

    Use it when the human vouches for a memory ('asserted'), when a memory
    turns out to be a copy of an external source ('retrieved'), or when
    something recorded as stated was actually your own inference
    ('concluded'). Facts extracted from a reclassified memory follow it.
    Provenance is reported on recall and never changes ranking.";

pub const CHECK_CONTRADICTIONS: &str = r"Scan for contradictions. Tier 1 (default): fast heuristic. Tier 2 (use_api=True): Claude API verification.";

pub const BRIEFING: &str = r"Session-start briefing: project context, experience summary, stale memories, knowledge, contradictions, reinstate candidates.";

pub const RECENT_KNOWLEDGE: &str = r"Recent knowledge articles ingested by the RSS worker.

    Returns knowledge items created within the given lookback window,
    sorted newest first. Optionally filter by feed name, topics, or licence
    class — licence='unknown' lists what still needs classifying.";

// Phase 4: skills.

pub const COMPILE_SKILL: &str = r"Compile domain procedure from experience and graveyard memories into a loadable SKILL.md. 'propose' (default) returns a reviewable diff; 'write' commits only a previously proposed and accepted diff — there is no silent-commit path.

    The compiled skill is build output: derived from raw memories, never
    hand-edited. To change domain guidance, update the underlying memories
    (record_experience, log_abandoned, bless) and recompile.";

pub const FIND_SKILLS: &str = r"Discover compiled skills: ranked skill IDs and descriptions for a query or domain. Load the winner intact with get_skill().

    Returns only skills that clear the relevance floor (SKILL_MIN_SCORE,
    default 0.25), so an empty list is a real answer — it means nothing
    stored covers this work, not that discovery failed. Each entry carries a
    `confidence` of 'high' or 'low'; don't load a 'low' one without reading
    its description first.";

pub const GET_SKILL: &str = r"Load a skill whole: the complete SKILL.md body with frontmatter and structure intact, by ID, name, or domain.";

pub const BLESS: &str = r"Promote a single strong lesson to skill-eligible now, bypassing the reinforcement threshold at the next compile_skill().

    The human-accept gate at compile time is still the safety net — bless
    only pre-qualifies the lesson, it does not write to any skill.";

pub const PROMOTE_KNOWLEDGE: &str = r#"Mark a knowledge item as permanently useful, and optionally skill-eligible for a domain.

    Without a domain: clears the expires_at field so the item is never
    auto-archived by maintenance. Use this when an RSS-ingested article turns
    out to be genuinely valuable.

    With a domain: additionally marks the article skill-eligible — the next
    compile_skill() for that domain compiles it into the skill's Reference
    section, citing the article. Promotion is the vetting step (an article
    carries no experience signal, so a human marking it eligible substitutes
    for reinforcement); the compile itself still runs the propose-and-accept
    gate. Expiry is cleared too — an article feeding a skill must not
    auto-archive underneath it.

    When the article contains discrete guidance (a "5 things to avoid" list,
    a best-practice post), read it first and pass the items as rules — each
    becomes its own stance-prefixed bullet in the Reference section instead
    of one summary line. Extraction happens here, under human review, never
    at compile time, so compilation stays deterministic. Re-promote with an
    edited list to revise; rules=[] reverts to the single summary rule.

    Args:
        key: The memory key (e.g. mem:knowledge:01ABC...).
        domain: Skill domain to make this article eligible for (e.g.
            'python'). Aliases resolve the same way as compile_skill.
        demote: With domain, remove that domain from the article's
            skill-eligibility instead of adding it.
        rules: With domain, extracted rules from the article, each
            {"kind": "do"|"watch"|"dont"|"note", "text": "..."} (max 20,
            400 chars each). Review them with the human before promoting."#;
