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
