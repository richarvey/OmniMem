//! Tool descriptions, verbatim from the 6.x docstrings (FastMCP sent the
//! whole docstring, `Args:` block included, and agents read it).

pub const VERSION: &str = "Return the current OmniMem version.";

pub const REMEMBER: &str = r"Store a memory with automatic dedup. Returns duplicate info if near-match exists; use force=True to override.

Args:
    content: Text to remember.
    project: Project to scope this memory to.
    tags: Categorisation tags.
    namespace: 'episodic' (default), 'project', 'knowledge', or 'preference'.
    force: Skip duplicate check.
    mode: 'full' (default — extract discrete facts via Claude before storing,
        routing preferences to the preference namespace) or 'raw' (store
        verbatim). Default follows the INGEST_MODE env var.
    licence: Redistribution rights for the content — 'own' (written here:
        a decision, a fix, a preference), 'open' (third-party under a
        redistributable licence), 'restricted' (third-party, not
        redistributable: paywalled, all rights reserved), or 'unknown'.
        A recognised identifier ('cc-by-4.0', 'ogl-3.0',
        'all-rights-reserved') is accepted and kept as a note. Defaults
        to 'own' for episodic/project/preference and 'unknown' for
        knowledge. Pass it explicitly whenever the content came from
        somewhere else — an article, a document, a vendor page.
    provenance: Where the content comes from — 'asserted' (the human
        stated it: a preference, a rule, a fact they gave you),
        'concluded' (your own reasoning or write-up of work done), or
        'retrieved' (an external source: an article, documentation, a
        search result). Defaults to 'concluded' for episodic and project
        memories, 'asserted' for preferences, 'retrieved' for knowledge.
        Pass
        'asserted' when the human dictated the content — a later
        session will treat your own conclusions as your conclusions,
        not as independent evidence.";

pub const REMEMBER_DOCUMENT: &str = r"Index a long-form document by splitting it into chunks and storing each chunk as a memory.

Use this instead of remember() for conversation transcripts, articles, meeting notes,
or any content longer than a focused fact. Returns the list of inserted keys plus a
shared doc_id so callers can clean up or group them later.

Args:
    content: Long-form text to index.
    chunk_strategy: 'turn_pairs' (User:/Assistant: transcripts), 'sentences',
        'paragraphs' (default), or 'fixed_tokens'.
    project: Project to scope these memories to.
    tags: Categorisation tags applied to every chunk.
    namespace: 'episodic' (default), 'project', or 'knowledge'.
    chunk_size: Words per chunk for fixed_tokens strategy (default 200).
    licence: Redistribution rights, applied to every chunk — see remember().
        Documents are the write most likely to be someone else's work, so
        say where it came from: 'restricted' for a paywalled or all-rights-
        reserved source, 'open' (or its identifier, e.g. 'ogl-3.0') for a
        redistributable one. Defaults to 'own' outside the knowledge
        namespace and 'unknown' inside it.
    provenance: 'retrieved' for a document from elsewhere, 'asserted'
        for one the human wrote, 'concluded' for your own output — see
        remember(). Applied to every chunk.";

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
is not obviously about what you asked, it isn't.

Args:
    query: What you're looking for.
    top_k: Max results (default 5).
    namespaces: Namespaces to search ('episodic', 'project', 'knowledge',
        'preference'). All four by default.
    project_filter: Restrict to a project.
    expand_queries: If True, generate alternative phrasings via Claude Haiku and union
        the results. Default follows the RECALL_EXPAND_QUERIES env var.
    domain_filter: Restrict to every project declaring these work-type
        domains (e.g. 'python') — the cross-project search. Combines with
        project_filter as an intersection. If no project declares the
        domain, the search runs unscoped and says so in a leading
        'domain_filter_notice' entry.";

pub const RECALL_INDEX: &str = r"Lightweight recall: returns ranked summaries without full content. Use recall_detail() to fetch full content for selected keys.

Shares recall()'s relevance floor (RECALL_MIN_SCORE, default 0.15) and
its `weak_match` flag, so top_k is a ceiling and a short or empty result
set is a real answer.

Args:
    query: What you're looking for.
    top_k: Max results (default 10).
    namespaces: Namespaces to search. All by default.
    project_filter: Restrict to a project.
    snippet_length: Content preview length in chars (default 150).
    expand_queries: If True, generate alternative phrasings via Claude Haiku
        and union the results.
    domain_filter: Restrict to every project declaring these work-type
        domains (e.g. 'python'). Reports back under 'domain_filter' when a
        requested domain matches no project.";

pub const RECALL_DETAIL: &str = r"Fetch full content for specific memory keys. Use after recall_index() to expand only the entries you need.

Args:
    keys: List of memory keys to retrieve (e.g. from recall_index results).";

pub const DEPRIORITISE: &str = r"Reduce a memory's visibility without deleting. Accepts a key or natural language query.

Args:
    key_or_query: Memory key (e.g. 'mem:episodic:...') or search query.
    reason: Why this is being deprioritised.
    reinstate_hints: Keywords that flag this as a reinstate candidate in future queries.";

pub const ARCHIVE: &str = r"Archive a memory — excluded from recall but still stored.

Args:
    key_or_query: Memory key or search query.
    reason: Why this is being archived.";

pub const REINSTATE: &str = r"Reinstate a deprioritised/archived memory to active state.

Args:
    key_or_query: Memory key or search query.";

pub const RETAG: &str = r"Replace or adjust the tags on a memory without re-embedding it.

Args:
    key: Memory key (e.g. 'mem:episodic:...').
    tags: Full replacement tag list; pass [] to clear all tags.
        Mutually exclusive with add/remove.
    add: Tags to add to the existing set (duplicates skipped).
    remove: Tags to remove from the existing set (exact match).";

pub const FORGET: &str = r"Permanently delete a memory. Requires confirm=True; returns preview otherwise.

Args:
    key_or_query: Memory key or search query.
    confirm: Must be True to delete.";

pub const SUPPRESS_TOPIC: &str = r"Suppress a topic — matching memories filtered from recall.

Args:
    topic: Topic string to suppress (case-insensitive).
    reason: Why.";

pub const UNSUPPRESS_TOPIC: &str = r"Remove a topic from the suppression list.

Args:
    topic: Topic to unsuppress.";

pub const LIST_SUPPRESSIONS: &str = "List all suppressed topics.";

pub const FIND_DUPLICATES: &str = r"Scan a namespace for clusters of near-identical memories.

Args:
    namespace: 'episodic' (default), 'project', or 'knowledge'.
    threshold: Similarity threshold (0.0-1.0). Default 0.92.
    project_filter: Restrict to a project.";

pub const HEALTH: &str =
    "Server health: database status, record and vector counts, model status, uptime.";

pub const QUEUE_STATUS: &str = r"Check the enrichment queue. Returns the number of pending jobs waiting for background fact extraction.

Use this to know when enrichment has finished after a batch ingest —
poll until pending reaches 0 before running recall/scoring.";

pub const DUMP_TO_FILE: &str = r"Export all memories to a JSON backup file.

Args:
    filename: Auto-generated if not provided.";

pub const RESTORE_FROM_FILE: &str = r"Restore memories from backup. Default dry_run=True previews without writing. Merges with existing data.

Args:
    filename: Backup filename (must be in BACKUP_DIR).
    dry_run: Preview only (default True). Set False to restore.";

pub const LIST_BACKUPS: &str = "List available backup files, newest first.";

// Phase 3: experience, projects, audit, classification, contradictions,
// briefing and knowledge.

pub const RECORD_EXPERIENCE: &str = r#"Record effort, outcome, dead ends, and breakthroughs for a memory. High-effort successes surface more; high-effort failures (>=4, abandoned) auto-suppress the approach names.

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

pub const LOG_ABANDONED: &str = r"Append a dead-end approach to a memory's abandoned list.

    Args:
        key: Memory key.
        name: Abandoned approach name.
        type: 'library', 'approach', 'tool', 'pattern', or 'service'.
        reason: Why it was abandoned.";

pub const GET_EXPERIENCE: &str = r"Return experience data for a memory key.

    Args:
        key: Memory key to look up.";

pub const EXPERIENCE_SUMMARY: &str = r"Aggregate experience stats: effort, outcomes, graveyard of abandoned approaches, breakthroughs.

    Args:
        project: Filter to a project.";

pub const WARN_IF_ABANDONED: &str = r"Check if an approach was previously abandoned. Call before suggesting libraries or tools.

    Args:
        query: Library, tool, or approach name to check.";

pub const SET_PROJECT_CONTEXT: &str = r"Create or update a project's context (description, stack, goals, state).

    Args:
        project_name: Unique project identifier.
        description: What the project does.
        stack: Technology stack.
        goals: Current objectives.
        current_state: Current project state.
        notes: Freeform notes for next session.
        domains: Kinds of work in this project ('python', 'docker', 'design'),
            sharing the compiled-skill vocabulary so recall(domain_filter=...)
            and find_skills() speak the same names. Omit to leave any existing
            domains untouched; pass [] to clear them. Use
            compile_project_domains() to have them suggested from the stack
            and the project's own memories.";

pub const GET_PROJECT_CONTEXT: &str = r"Retrieve full context for a project by name.

    Args:
        project_name: Project to look up.";

pub const LIST_PROJECTS: &str = r"List all stored project contexts, deduplicated by project name.

    Args:
        domain: Only list projects declaring this work-type domain
            (e.g. 'python'). Aliases resolve the same way skill domains do.";

pub const COMPILE_PROJECT_DOMAINS: &str = r"Suggest work-type domains for a project from its stack and its own memories, with the evidence behind each one. Returns a draft by default; pass auto_save=True to store it.

    Domains are what makes cross-project recall work: once projects declare
    them, recall(domain_filter='python') searches every Python project at
    once. They share the compiled-skill vocabulary, so the same names reach
    find_skills() and get_skill().

    Suggestions are merged with any domains the project already declares —
    this never removes one.

    Args:
        project_name: Project to suggest domains for.
        auto_save: If True, write the merged domain list to the project context.";

pub const UPDATE_PROJECT_STATE: &str = r"Update a project's current state and notes without re-embedding.

    Args:
        project_name: Project to update.
        current_state: New state description.
        notes: Notes for next session.";

pub const DELETE_PROJECT: &str = r"Bulk delete every memory belonging to a project. Requires confirm=True; returns a preview otherwise.

    Finds memories by scanning keys directly (no semantic search), so it
    catches everything — including memories that recall can't surface.
    Deletes in pipelined batches rather than one call per key.

    Args:
        project_name: Project whose memories should be deleted.
        confirm: Must be True to delete. False returns a preview with counts.
        include_context: Also delete the project's context entry
            (mem:project:<name>). Default False keeps it.";

pub const DEPRIORITISE_PROJECT: &str = r"Bulk deprioritise every active memory in a project (0.2x recall visibility, reversible). Requires confirm=True; returns a preview otherwise.

    Like delete_project but non-destructive: memories stay stored and searchable,
    just heavily down-weighted in recall. Undo the whole project with
    reinstate_project(). Only memories currently in the active state are changed;
    already-deprioritised or archived ones are reported under `already_inactive`.

    Args:
        project_name: Project whose memories should be deprioritised.
        confirm: Must be True to apply. False returns a preview with counts.
        reason: Optional note stored on each memory explaining why.
        include_context: Also deprioritise the project's context entry
            (mem:project:<name>). Default False keeps it active.";

pub const REINSTATE_PROJECT: &str = r"Bulk reinstate every deprioritised or archived memory in a project back to active. Requires confirm=True; returns a preview otherwise. The inverse of deprioritise_project().

    Args:
        project_name: Project whose memories should be reactivated.
        confirm: Must be True to apply. False returns a preview with counts.
        include_context: Also reinstate the project's context entry
            (mem:project:<name>). Default False leaves it as-is.";

pub const COMPILE_PROJECT_CONTEXT: &str = r"Gather all stored memories for a project and compile them into a structured context draft. Use this before set_project_context() to auto-produce or refresh a project's context from its episodic memories, experience data, and abandoned approaches.

    Args:
        project_name: Project to compile context for.
        auto_save: If True, automatically save the compiled context (creates or updates).";

pub const MEMORY_AUDIT: &str = r"Summary of all memories grouped by state. Useful for cleanup.

    The state counts always cover the whole store; the per-memory ``entries``
    list is paginated so a large store doesn't return thousands of rows at once.

    Args:
        project: Filter to a project.
        namespace: Filter to 'episodic', 'project', 'knowledge', or
            'preference'. If omitted, covers all four.
        include_archived: Include archived memories (default False).
        limit: Max entries to return (default 100, max 500).
        offset: Entries to skip for pagination (default 0).";

pub const WHY_DID_YOU_MENTION: &str = r"Explain why a topic surfaced by searching recall logs.

    Args:
        query: Topic or phrase to investigate.";

pub const EXPLAIN_MEMORY: &str = r"Return full metadata for a memory key.

    Args:
        key: Full memory key (e.g. 'mem:episodic:01ARZ3...').";

pub const REINDEX: &str = r"Rebuild the in-memory vector index from the database.

    Kept for compatibility with 6.x clients. There is no separate search index
    any more, so nothing can drift from the records and no phantom entries
    are ever removed; this reloads the vectors and reports the counts.

    Args:
        namespace: 'episodic', 'project', 'knowledge', 'preference', or
                   'skill'. If omitted, reloads all five.";

pub const SET_LICENCE: &str = r"Record the redistribution rights of one or more memories.

    Give either specific keys, or a feed_name to classify every article
    ingested from that RSS feed at once. Use it when recall reports results
    with an unknown licence and the human can say where the content stands,
    or to correct a wrong classification. Facts extracted from a classified
    memory take the same licence automatically.

    Args:
        licence: 'own' (written here), 'open' (third-party, redistributable —
            OGL, CC BY, public domain), 'restricted' (third-party, not
            redistributable — paywalled, all rights reserved, CC BY-NC/ND),
            or 'unknown'. A recognised identifier such as 'cc-by-4.0',
            'ogl-3.0' or 'all-rights-reserved' is accepted and kept as the
            note.
        keys: Memory keys to classify (max 200 per call).
        feed_name: Classify every knowledge article from this RSS feed
            instead. Only affects articles already stored — set `licence:` on
            the feed itself (feeds.yml or the web UI feed editor) so future
            articles arrive classified.
        note: Optional free-text detail — the specific licence or where it
            was checked (max 200 chars). Overrides the identifier-derived
            note.";

pub const SET_PROVENANCE: &str = r"Record where one or more memories came from.

    Use it when the human vouches for a memory ('asserted'), when a memory
    turns out to be a copy of an external source ('retrieved'), or when
    something recorded as stated was actually your own inference
    ('concluded'). Facts extracted from a reclassified memory follow it.
    Provenance is reported on recall and never changes ranking.

    Args:
        provenance: 'retrieved' (external source), 'concluded' (the
            system's own reasoning or write-up), or 'asserted' (stated by
            the human).
        keys: Memory keys to reclassify (max 200 per call).";

pub const CHECK_CONTRADICTIONS: &str = r"Scan for contradictions. Tier 1 (default): fast heuristic. Tier 2 (use_api=True): Claude API verification.

    Args:
        query: Focus the search. If None, scans recent memories.
        namespace: 'episodic' (default), 'project', or 'knowledge'.
        project_filter: Restrict to a project.
        use_api: Use Claude API for deeper analysis.";

pub const BRIEFING: &str = r"Session-start briefing: project context, experience summary, stale memories, knowledge, contradictions, reinstate candidates.

    Args:
        project: Project name to focus on.
        include_knowledge: Include recent knowledge articles (default True).";

pub const RECENT_KNOWLEDGE: &str = r"Recent knowledge articles ingested by the RSS worker.

    Returns knowledge items created within the given lookback window,
    sorted newest first. Optionally filter by feed name, topics, or licence
    class — licence='unknown' lists what still needs classifying.

    Args:
        days: Lookback window in days (default 7, max 365).
        feed_name: Filter to a specific RSS feed name.
        topics: Filter to items tagged with at least one of these topics.
        limit: Maximum results to return (default 20, max 50).
        licence: Filter to one redistribution class: 'own', 'open',
            'restricted', or 'unknown'.";

// Phase 4: skills.

pub const COMPILE_SKILL: &str = r"Compile domain procedure from experience and graveyard memories into a loadable SKILL.md. 'propose' (default) returns a reviewable diff; 'write' commits only a previously proposed and accepted diff — there is no silent-commit path.

    The compiled skill is build output: derived from raw memories, never
    hand-edited. To change domain guidance, update the underlying memories
    (record_experience, log_abandoned, bless) and recompile.

    Args:
        domain: Free-form domain tag, e.g. 'python', 'rust', 'technical-blogging'.
        mode: 'propose' returns a diff (or full draft if the skill is new) and
            stashes it; 'write' commits the stashed proposal after human review.
        min_reinforcement: Lessons must recur across this many memories to
            become rules (default 2). bless() promotes a single strong lesson
            past the gate.
        include_graveyard: Compile abandoned approaches into Don't rules
            (default True).
        export_path: On write, also mirror the SKILL.md to this relative path
            under SKILL_EXPORT_DIR. The OmniMem store remains canonical.
        description: Explicitly set the skill description (the load trigger).
            The description is human-owned: the compiler drafts one at
            creation, and recompiles keep the stored one unless this is passed.";

pub const FIND_SKILLS: &str = r"Discover compiled skills: ranked skill IDs and descriptions for a query or domain. Load the winner intact with get_skill().

    Returns only skills that clear the relevance floor (SKILL_MIN_SCORE,
    default 0.25), so an empty list is a real answer — it means nothing
    stored covers this work, not that discovery failed. Each entry carries a
    `confidence` of 'high' or 'low'; don't load a 'low' one without reading
    its description first.

    Args:
        query_or_domain: A domain tag ('python') or a free-text description
            of the work at hand.";

pub const GET_SKILL: &str = r"Load a skill whole: the complete SKILL.md body with frontmatter and structure intact, by ID, name, or domain.

    Args:
        skill_id: Full key ('mem:skill:gen:python-ric'), name
            ('python-ric'), or bare domain ('python').";

pub const BLESS: &str = r"Promote a single strong lesson to skill-eligible now, bypassing the reinforcement threshold at the next compile_skill().

    The human-accept gate at compile time is still the safety net — bless
    only pre-qualifies the lesson, it does not write to any skill.

    Args:
        memory_key: Episodic memory key carrying the lesson
            (lesson, breakthrough, gotchas, or graveyard entry).";

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
