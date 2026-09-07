"""One-time startup migrations, run by server.py before serving requests.

Each migration is idempotent and cheap on an already-migrated store: it
scans, finds nothing to fix, and returns. They live here rather than in
server.py so they can be unit-tested against the in-memory fakes without
importing the FastMCP app.
"""

import logging

logger = logging.getLogger(__name__)

# Articles are labelled with this project unless the feed sets its own
# `project:` in feeds.yml. Must match _DEFAULT_PROJECT in rss_worker/ingester.py.
RSS_PROJECT_LABEL = "RSS"


def migrate_missing_state(store) -> None:
    """Backfill state=active on memories that pre-date the state field.

    Recall pushes a state tag filter into FT.SEARCH; a doc with no state
    field would silently drop out of every filtered search even though the
    Python-side default treats missing state as active. One-time backfill
    keeps the two behaviours identical.
    """
    fixed = 0
    for ns in ("episodic", "project", "knowledge", "preference"):
        keys = store.scan_prefix(f"mem:{ns}:")
        if not keys:
            continue
        rows = store.get_fields_multi(keys, ("state",))
        for key, row in zip(keys, rows):
            if row is None or not row.get("state"):
                store.set_field(key, "state", "active")
                fixed += 1
    if fixed:
        logger.info("Migration: backfilled state=active on %d memories", fixed)


def migrate_project_names(store) -> None:
    """Set project_name from project field on ULID-keyed project memories missing it."""
    keys = store.scan_prefix("mem:project:")
    if not keys:
        return

    all_data = store.get_multi(keys)
    fixed = 0
    for key, data in zip(keys, all_data):
        if not data:
            continue
        # Skip entries that already have project_name set
        if data.get("project_name"):
            continue
        # Use the project field if available
        project = data.get("project")
        if project:
            store.set_field(key, "project_name", project)
            fixed += 1

    if fixed:
        logger.info("Migration: set project_name on %d project memories", fixed)


def migrate_rss_article_projects(store) -> None:
    """Label pre-existing RSS articles with project="RSS".

    The ingester stamps a project on every article (default "RSS", per-feed
    override in feeds.yml) so ingested articles can be separated from
    conversation-sourced knowledge. Articles are identified by their
    feed_name field — extracted facts and remember() writes never carry one.
    Articles that already have a project (a per-feed label) are left alone.
    """
    keys = store.scan_prefix("mem:knowledge:")
    if not keys:
        return

    rows = store.get_fields_multi(keys, ("feed_name", "project"))
    fixed = 0
    for key, row in zip(keys, rows):
        if not row:
            continue
        if row.get("feed_name") and not row.get("project"):
            store.set_field(key, "project", RSS_PROJECT_LABEL)
            fixed += 1

    if fixed:
        logger.info(
            "Migration: labelled %d RSS articles with project=%s",
            fixed, RSS_PROJECT_LABEL,
        )


def migrate_project_domains(store) -> None:
    """Seed work-type domains (v6.6) from each project's existing stack string.

    Without this, every project upgrading to v6.6 has no domains, so
    recall(domain_filter=...) matches nothing on day one — the failure mode
    that made issue #20 so hard to spot, where a filter silently returns
    nothing rather than reporting that it has nothing to filter on.

    Deliberately conservative and never destructive: it only writes to
    projects with no `domains` field at all, and it only reads a field a human
    already wrote by hand. A project whose stack yields nothing usable gets an
    empty marker so a later run doesn't rescan it, and so the web UI can tell
    "not set up yet" from "considered and empty". Anything derived here is
    editable in the web UI or replaceable with compile_project_domains().
    """
    from .project_domains import (
        invalidate_domain_cache,
        normalise_domains,
        parse_domains,
        serialise_domains,
        _STACK_STOPWORDS,
    )

    keys = store.scan_prefix("mem:project:")
    if not keys:
        return

    rows = store.get_fields_multi(keys, ("stack", "domains", "goals"))
    seeded = 0
    marked = 0
    for key, row in zip(keys, rows):
        if not row:
            continue
        # `domains` present at all (even empty) means this project has been
        # through the migration or been edited since.
        if row.get("domains") is not None:
            continue
        # Only real context entries carry a stack or goals; ULID-keyed project
        # memories are not projects and must not grow a domains field.
        if not (row.get("stack") or row.get("goals")):
            continue

        candidates = [
            item for item in parse_domains(row.get("stack") or "")
            if item.strip().lower() not in _STACK_STOPWORDS
        ]
        domains, _, _ = normalise_domains(candidates)
        store.set_field(key, "domains", serialise_domains(domains))
        if domains:
            seeded += 1
        else:
            marked += 1

    if seeded or marked:
        invalidate_domain_cache()
        logger.info(
            "Migration: seeded domains on %d projects from their stack "
            "(%d had nothing usable to derive)",
            seeded, marked,
        )


def migrate_licence(store) -> None:
    """Backfill the licence / redistribution field (v6.6.1) with honest defaults.

    Conversation-sourced namespaces (episodic, project, preference) are our
    own work and get ``own``. Knowledge is the namespace that holds
    third-party material, and it gets no optimistic guess: RSS articles
    (identified by ``feed_name``) become ``unknown`` so recall can point
    them out for a human to classify, and so do plain knowledge writes with
    no traceable origin. The one derivation the migration does make is for
    extracted facts — a fact carries ``enriched_from`` pointing at the
    memory it was extracted from, and a derivative has exactly its source's
    rights, wherever the fact landed (knowledge or preference). Sources are
    resolved from this pass first and the store second; a fact whose source
    is gone is own, because the only thing that is ever enriched is a
    conversation write.

    Memories that arrived in a skill bundle (``imported_at``) are someone
    else's work and become ``unknown``, whatever namespace they sit in.

    Idempotent: only records with no ``licence`` field at all are touched,
    so a value a human has since set is never revisited. Writes are batched
    per value through set_fields_multi. Runs at startup and again after a
    backup restore, since a pre-6.6.1 dump carries no licence field.
    """
    from .licence import LICENCE_OWN, LICENCE_UNKNOWN

    own_keys: list[str] = []
    imported_keys: list[str] = []
    pending_facts: list[tuple[str, str]] = []  # (fact key, source key)
    for ns in ("episodic", "project", "preference"):
        keys = store.scan_prefix(f"mem:{ns}:")
        if not keys:
            continue
        rows = store.get_fields_multi(keys, ("licence", "imported_at", "enriched_from"))
        for key, row in zip(keys, rows):
            row = row or {}
            if row.get("licence"):
                continue
            # A memory that arrived in a skill bundle is someone else's work
            # by definition; "own" would be exactly the optimistic guess the
            # backfill exists to avoid.
            if row.get("imported_at"):
                imported_keys.append(key)
            elif row.get("enriched_from"):
                # An extracted preference inherits like an extracted fact.
                pending_facts.append((key, row["enriched_from"]))
            else:
                own_keys.append(key)
    # Stamp the conversation namespaces first: extracted facts inherit from
    # them, so the sources must carry a value before the facts are read.
    if own_keys:
        store.set_fields_multi(own_keys, {"licence": LICENCE_OWN})
    if imported_keys:
        store.set_fields_multi(imported_keys, {"licence": LICENCE_UNKNOWN})

    knowledge_keys = store.scan_prefix("mem:knowledge:")
    knowledge_by_value: dict[str, list[str]] = {}
    rows = store.get_fields_multi(
        knowledge_keys, ("licence", "feed_name", "enriched_from", "imported_at"),
    )
    for key, row in zip(knowledge_keys, rows):
        row = row or {}
        if row.get("licence"):
            continue
        source = row.get("enriched_from")
        if source and not row.get("feed_name") and not row.get("imported_at"):
            pending_facts.append((key, source))
        else:
            # Articles, plain writes, and anything imported — including an
            # imported fact, whose source lives on another instance.
            knowledge_by_value.setdefault(LICENCE_UNKNOWN, []).append(key)

    inherited = 0
    if pending_facts:
        # Sources stamped in this pass resolve in memory; the rest with one
        # lookup per distinct source, not per fact.
        stamped = {k: LICENCE_OWN for k in own_keys}
        stamped.update({k: LICENCE_UNKNOWN for k in imported_keys})
        unresolved = sorted({src for _, src in pending_facts if src not in stamped})
        source_rows = store.get_fields_multi(unresolved, ("licence",))
        stamped.update({
            src: (row or {}).get("licence") for src, row in zip(unresolved, source_rows)
        })
        for key, source in pending_facts:
            value = stamped.get(source)
            if value:
                inherited += 1
            # A fact's only possible source is a conversation write, so a
            # source that is gone was own — the same answer the read-time
            # fallback gives, so nothing flips when the backfill runs.
            knowledge_by_value.setdefault(value or LICENCE_OWN, []).append(key)

    for value, keys in knowledge_by_value.items():
        store.set_fields_multi(keys, {"licence": value})

    unknown = len(knowledge_by_value.get(LICENCE_UNKNOWN, [])) + len(imported_keys)
    total = len(own_keys) + len(imported_keys) + sum(
        len(keys) for keys in knowledge_by_value.values()
    )
    if total:
        logger.info(
            "Migration: backfilled licence on %d memories (%d own, %d unknown, "
            "%d extracted facts inherited their source's licence)",
            total, len(own_keys), unknown, inherited,
        )


def _is_project_context(key: str, row: dict) -> bool:
    """A context entry lives at mem:project:{project_name}; a ULID-keyed
    project memory from remember(namespace="project") sets project_name
    too but never matches its own key. Stack or goals is the fallback for
    contexts written before project_name was stamped consistently."""
    name = row.get("project_name")
    if name and key == f"mem:project:{name}":
        return True
    return bool(row.get("stack") or row.get("goals"))


def migrate_provenance(store) -> None:
    """Backfill the provenance class (v6.6.2).

    Knowledge with a ``feed_name`` is an article — retrieved. Preferences
    and project context entries (keyed by their project_name) are what the
    human told us — asserted. Extracted facts (``enriched_from``, in the
    knowledge or preference namespace) are restatements and inherit their
    source, exactly as live enrichment stamps them; a fact whose source is
    gone is concluded, since it exists only because the system produced
    it. Every other record — episodic memories, ULID-keyed project
    memories, plain knowledge writes — is the agent's own account and
    becomes concluded (a plain knowledge write made today defaults to
    retrieved, but a legacy one carries no evidence of where it came from).

    That last default is the deliberate, uncomfortable choice: an episodic
    memory is the write-up of work that really happened, and calling the
    lot "concluded" undersells the most valuable material in the store.
    But it is honest about who wrote it, and the human can vouch for any
    memory afterwards with set_provenance(..., "asserted"). Idempotent:
    only records with no ``provenance`` field are touched. Runs at startup
    and after a backup restore.
    """
    from .provenance import (
        PROVENANCE_ASSERTED,
        PROVENANCE_CONCLUDED,
        PROVENANCE_RETRIEVED,
    )

    assigned: dict[str, str] = {}
    pending_facts: list[tuple[str, str]] = []  # (fact key, source key)

    # Everything that is not an extracted fact gets its class from what it
    # is. Facts are deferred: they inherit from their source, which may be
    # assigned in this same pass — so sources are resolved from the
    # in-memory assignments first and the store second, never relying on
    # the order the namespaces happened to be written in.
    for ns in ("episodic", "project", "preference", "knowledge"):
        keys = store.scan_prefix(f"mem:{ns}:")
        if not keys:
            continue
        rows = store.get_fields_multi(
            keys, ("provenance", "project_name", "stack", "goals", "feed_name", "enriched_from"),
        )
        for key, row in zip(keys, rows):
            row = row or {}
            if row.get("provenance"):
                continue
            if row.get("feed_name"):
                assigned[key] = PROVENANCE_RETRIEVED
            elif row.get("enriched_from"):
                pending_facts.append((key, row["enriched_from"]))
            elif ns == "preference" or (ns == "project" and _is_project_context(key, row)):
                assigned[key] = PROVENANCE_ASSERTED
            else:
                assigned[key] = PROVENANCE_CONCLUDED

    inherited = 0
    if pending_facts:
        # One lookup per distinct source, not per fact.
        unresolved = sorted({src for _, src in pending_facts if src not in assigned})
        source_rows = store.get_fields_multi(unresolved, ("provenance",))
        stored = {
            src: (row or {}).get("provenance") for src, row in zip(unresolved, source_rows)
        }
        for key, source in pending_facts:
            value = assigned.get(source) or stored.get(source)
            if value:
                inherited += 1
            assigned[key] = value or PROVENANCE_CONCLUDED

    by_value: dict[str, list[str]] = {}
    for key, value in assigned.items():
        by_value.setdefault(value, []).append(key)
    for value, keys in by_value.items():
        store.set_fields_multi(keys, {"provenance": value})

    total = len(assigned)
    if total:
        logger.info(
            "Migration: backfilled provenance on %d memories "
            "(%d extracted facts inherited their source's class)",
            total, inherited,
        )
