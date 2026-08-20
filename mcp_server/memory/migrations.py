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
