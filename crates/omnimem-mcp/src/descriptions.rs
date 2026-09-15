//! Tool descriptions, verbatim from the 6.x docstrings (FastMCP sent the
//! whole docstring, `Args:` block included, and agents read it).

pub const VERSION: &str = "Return the current OmniMem version.";

pub const REMEMBER: &str = r#"Store a memory with automatic dedup. Returns duplicate info if near-match exists; use force=True to override.

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
        not as independent evidence."#;

pub const REMEMBER_DOCUMENT: &str = r#"Index a long-form document by splitting it into chunks and storing each chunk as a memory.

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
        remember(). Applied to every chunk."#;

pub const RECALL: &str = r#"Search memories by semantic similarity. Returns ranked results; abandoned-approach warnings appear first.

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
        'domain_filter_notice' entry."#;

pub const RECALL_INDEX: &str = r#"Lightweight recall: returns ranked summaries without full content. Use recall_detail() to fetch full content for selected keys.

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
        requested domain matches no project."#;

pub const RECALL_DETAIL: &str = r#"Fetch full content for specific memory keys. Use after recall_index() to expand only the entries you need.

Args:
    keys: List of memory keys to retrieve (e.g. from recall_index results)."#;

pub const DEPRIORITISE: &str = r#"Reduce a memory's visibility without deleting. Accepts a key or natural language query.

Args:
    key_or_query: Memory key (e.g. 'mem:episodic:...') or search query.
    reason: Why this is being deprioritised.
    reinstate_hints: Keywords that flag this as a reinstate candidate in future queries."#;

pub const ARCHIVE: &str = r#"Archive a memory — excluded from recall but still stored.

Args:
    key_or_query: Memory key or search query.
    reason: Why this is being archived."#;

pub const REINSTATE: &str = r#"Reinstate a deprioritised/archived memory to active state.

Args:
    key_or_query: Memory key or search query."#;

pub const RETAG: &str = r#"Replace or adjust the tags on a memory without re-embedding it.

Args:
    key: Memory key (e.g. 'mem:episodic:...').
    tags: Full replacement tag list; pass [] to clear all tags.
        Mutually exclusive with add/remove.
    add: Tags to add to the existing set (duplicates skipped).
    remove: Tags to remove from the existing set (exact match)."#;

pub const FORGET: &str = r#"Permanently delete a memory. Requires confirm=True; returns preview otherwise.

Args:
    key_or_query: Memory key or search query.
    confirm: Must be True to delete."#;

pub const SUPPRESS_TOPIC: &str = r#"Suppress a topic — matching memories filtered from recall.

Args:
    topic: Topic string to suppress (case-insensitive).
    reason: Why."#;

pub const UNSUPPRESS_TOPIC: &str = r#"Remove a topic from the suppression list.

Args:
    topic: Topic to unsuppress."#;

pub const LIST_SUPPRESSIONS: &str = "List all suppressed topics.";

pub const FIND_DUPLICATES: &str = r#"Scan a namespace for clusters of near-identical memories.

Args:
    namespace: 'episodic' (default), 'project', or 'knowledge'.
    threshold: Similarity threshold (0.0-1.0). Default 0.92.
    project_filter: Restrict to a project."#;

pub const HEALTH: &str =
    "Server health: database status, record and vector counts, model status, uptime.";

pub const QUEUE_STATUS: &str = r#"Check the enrichment queue. Returns the number of pending jobs waiting for background fact extraction.

Use this to know when enrichment has finished after a batch ingest —
poll until pending reaches 0 before running recall/scoring."#;

pub const DUMP_TO_FILE: &str = r#"Export all memories to a JSON backup file.

Args:
    filename: Auto-generated if not provided."#;

pub const RESTORE_FROM_FILE: &str = r#"Restore memories from backup. Default dry_run=True previews without writing. Merges with existing data.

Args:
    filename: Backup filename (must be in BACKUP_DIR).
    dry_run: Preview only (default True). Set False to restore."#;

pub const LIST_BACKUPS: &str = "List available backup files, newest first.";
