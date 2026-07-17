"""Tests for the one-time startup migrations in memory/migrations.py."""

from memory.migrations import (
    migrate_missing_state,
    migrate_project_names,
    migrate_rss_article_projects,
)


def _seed(store, key, fields):
    store._client.hset(key, mapping=fields)


class TestMigrateRssArticleProjects:
    def test_labels_articles_missing_project(self, fake_store):
        _seed(fake_store, "mem:knowledge:abc123", {
            "content": "An article", "feed_name": "Rust Official Blog",
            "state": "active",
        })

        migrate_rss_article_projects(fake_store)

        assert fake_store.get("mem:knowledge:abc123")["project"] == "RSS"

    def test_leaves_per_feed_labels_alone(self, fake_store):
        _seed(fake_store, "mem:knowledge:abc123", {
            "content": "An article", "feed_name": "Rust Official Blog",
            "project": "rust-news", "state": "active",
        })

        migrate_rss_article_projects(fake_store)

        assert fake_store.get("mem:knowledge:abc123")["project"] == "rust-news"

    def test_ignores_conversation_knowledge(self, fake_store):
        # Extracted facts and remember() writes carry no feed_name — the
        # migration must not stamp a project on them.
        _seed(fake_store, "mem:knowledge:01FACT", {
            "content": "A fact from conversation", "project": "omnimem",
            "enriched_from": "mem:episodic:01SRC", "state": "active",
        })
        _seed(fake_store, "mem:knowledge:01MANUAL", {
            "content": "Manually remembered knowledge", "state": "active",
        })

        migrate_rss_article_projects(fake_store)

        assert fake_store.get("mem:knowledge:01FACT")["project"] == "omnimem"
        assert "project" not in fake_store.get("mem:knowledge:01MANUAL")

    def test_noop_on_empty_store(self, fake_store):
        migrate_rss_article_projects(fake_store)  # must not raise


class TestMigrateProjectNames:
    def test_backfills_project_name_from_project(self, fake_store):
        _seed(fake_store, "mem:project:01ULID", {
            "content": "A project memory", "project": "omnimem",
        })
        _seed(fake_store, "mem:project:omnimem", {
            "content": "Context", "project_name": "omnimem",
        })

        migrate_project_names(fake_store)

        assert fake_store.get("mem:project:01ULID")["project_name"] == "omnimem"
        assert fake_store.get("mem:project:omnimem")["project_name"] == "omnimem"


class TestMigrateMissingState:
    def test_backfills_state_active(self, fake_store):
        _seed(fake_store, "mem:episodic:01A", {"content": "no state"})
        _seed(fake_store, "mem:knowledge:01B", {"content": "has state", "state": "archived"})

        migrate_missing_state(fake_store)

        assert fake_store.get("mem:episodic:01A")["state"] == "active"
        assert fake_store.get("mem:knowledge:01B")["state"] == "archived"
