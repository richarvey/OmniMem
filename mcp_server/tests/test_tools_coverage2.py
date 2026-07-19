"""Second targeted coverage pass: tools, oauth, and rss_worker branches.

Covers the remaining missed lines in tools/*, oauth/provider.py,
oauth/storage.py, and rss_worker/ingester.py without touching the
existing coverage suites.
"""

import asyncio
import json
import sys
import time
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from tests.conftest import store_memory

import tools as tools_module

# The RSS worker modules import each other as top-level modules (they run
# with /app as the working directory in Docker), so rss_worker goes on
# sys.path — same arrangement as test_rss_worker.py.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "rss_worker"))
ingester = pytest.importorskip(
    "ingester", reason="rss_worker not present in this test image"
)


def _run(coro):
    """Run an async coroutine synchronously."""
    return asyncio.run(coro)


@pytest.fixture(autouse=True)
def inject_deps(fake_store, fake_embedder, lifecycle, pipeline):
    tools_module._store = fake_store
    tools_module._embedder = fake_embedder
    tools_module._lifecycle = lifecycle
    tools_module._pipeline = pipeline
    yield
    tools_module._store = None
    tools_module._embedder = None
    tools_module._lifecycle = None
    tools_module._pipeline = None


def _ghost(fake_store, key):
    """Store a key whose readable fields are all missing (vector only)."""
    fake_store.client.hset(key, mapping={"vector": b"\x00"})


# ---------------------------------------------------------------------------
# tools/__init__.py
# ---------------------------------------------------------------------------


class TestCompact:
    def test_empty_dict_value_stripped(self):
        from tools import _compact

        assert _compact({"keep": {"a": 1}, "drop": {}}) == {"keep": {"a": 1}}


# ---------------------------------------------------------------------------
# tools/project.py
# ---------------------------------------------------------------------------


class TestProjectBranches:
    def test_list_projects_skips_unreadable_key(self, fake_store, fake_embedder):
        from tools.project import list_projects, set_project_context

        set_project_context("mine", "desc", "python", "ship it", "going well")
        _ghost(fake_store, "mem:project:ghost")

        names = [p["project_name"] for p in list_projects()["projects"]]
        assert names == ["mine"]

    def test_delete_project_skips_unreadable_key(self, fake_store, fake_embedder):
        from tools.project import delete_project

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "a memory", project="mine")
        _ghost(fake_store, "mem:episodic:ghost")

        result = delete_project("mine")
        assert result["status"] == "preview"
        assert result["total"] == 1

    def test_compile_context_full_branch_coverage(self, fake_store, fake_embedder):
        from tools.project import compile_project_context, set_project_context

        # Existing context with goals/stack → existing_context branch.
        set_project_context("mine", "a project", "python", "ship it", "ongoing")

        # Unreadable episodic key → skipped.
        _ghost(fake_store, "mem:episodic:ghost")

        # Healthy memory with experience data and an outcome.
        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "solid decision", project="mine", tags=["python"],
                     effort_score=5, outcome="succeeded")

        # Corrupt tags, effort score, and abandoned approaches — all must
        # be tolerated, not raised.
        store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                     "broken metadata", project="mine")
        fake_store.set_fields("mem:episodic:01B", {
            "tags": "{broken",
            "effort_score": "not a number",
            "abandoned_approaches": "{broken",
        })

        result = compile_project_context("mine")
        assert result["existing_context"]["goals"] == "ship it"
        assert result["memory_count"] == 2
        by_key = {m["key"]: m for m in result["memories"]}
        assert by_key["mem:episodic:01A"]["effort_score"] == 5
        assert by_key["mem:episodic:01A"]["outcome"] == "succeeded"
        assert "effort_score" not in by_key["mem:episodic:01B"]


# ---------------------------------------------------------------------------
# tools/contradiction.py
# ---------------------------------------------------------------------------


class TestContradictionBranches:
    def test_scan_skips_unreadable_and_contentless(self, fake_store, fake_embedder):
        from tools.contradiction import check_contradictions

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use docker compose here")
        # Unreadable key → data is None branch.
        _ghost(fake_store, "mem:episodic:ghost")
        # Readable but contentless → content_b empty branch.
        fake_store.client.hset("mem:episodic:01B", mapping={"state": "active"})

        assert check_contradictions()["contradictions"] == []

    def test_comparison_cap_breaks_both_loops(self, fake_store, fake_embedder, monkeypatch):
        import tools.contradiction as tc

        monkeypatch.setattr(tc, "_CONTRADICTION_COMPARISON_CAP", 1)
        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use docker compose here")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                     "never use docker compose here")
        store_memory(fake_store, fake_embedder, "mem:episodic:01C",
                     "always use docker compose here too")

        result = tc.check_contradictions()
        # Only one comparison was allowed, so at most one pair got flagged.
        assert len(result["contradictions"]) <= 1

    def test_results_cap_breaks_both_loops(self, fake_store, fake_embedder, monkeypatch):
        import tools.contradiction as tc

        monkeypatch.setattr(tc, "_CONTRADICTION_RESULTS_CAP", 1)
        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use docker compose here")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                     "never use docker compose here")
        store_memory(fake_store, fake_embedder, "mem:episodic:01C",
                     "never use docker compose here please")

        result = tc.check_contradictions()
        assert len(result["contradictions"]) == 1

    def test_duplicate_pair_seen_once(self, fake_store, fake_embedder, monkeypatch):
        from tools.contradiction import check_contradictions

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use docker compose here")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                     "never use docker compose here")
        # The query path takes whatever search returns — a duplicated doc
        # produces a repeated pair, which the seen_pairs guard must swallow.
        docs = [
            {"key": "mem:episodic:01A", "state": "active",
             "content": "always use docker compose here"},
            {"key": "mem:episodic:01B", "state": "active",
             "content": "never use docker compose here"},
            {"key": "mem:episodic:01A", "state": "active",
             "content": "always use docker compose here"},
        ]
        monkeypatch.setattr(
            fake_store, "search",
            lambda namespace, vector, top_k=10, filter_expr=None: docs,
        )
        result = check_contradictions(query="docker compose")
        assert len(result["contradictions"]) == 1

    def test_api_tier_confirms_and_rejects(self, fake_store, fake_embedder, monkeypatch):
        import tools.contradiction as tc

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use docker compose here")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                     "never use docker compose here")
        store_memory(fake_store, fake_embedder, "mem:episodic:01C",
                     "never use docker compose here please")

        api = MagicMock(side_effect=[
            {"is_contradiction": True, "confidence": 0.91,
             "explanation": "Direct opposition."},
            {"is_contradiction": False},
        ])
        monkeypatch.setattr(tc, "check_contradiction_api", api)

        result = tc.check_contradictions(use_api=True)
        assert api.call_count == 2
        assert len(result["contradictions"]) == 1
        entry = result["contradictions"][0]
        assert entry["method"] == "api_confirmed"
        assert entry["confidence"] == 0.91


# ---------------------------------------------------------------------------
# tools/audit.py
# ---------------------------------------------------------------------------


class TestAuditBranches:
    def test_memory_audit_unreadable_key_and_bad_effort(self, fake_store, fake_embedder):
        from tools.audit import memory_audit

        _ghost(fake_store, "mem:episodic:ghost")
        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "with broken effort")
        fake_store.set_fields("mem:episodic:01A", {"effort_score": "not a number"})

        result = memory_audit()
        assert result["returned"] == 1
        assert "effort_score" not in result["entries"][0]

    def test_why_did_you_mention_all_logs_unreadable(self, fake_store):
        from tools.audit import why_did_you_mention

        _ghost(fake_store, "log:recall:1")
        assert why_did_you_mention("anything")["status"] == "not_found"

    def test_why_did_you_mention_semantic_match(self, fake_store):
        from tools.audit import why_did_you_mention

        # Same words, different order: no substring match, but the additive
        # word-vector embeddings are identical → semantic similarity 1.0.
        fake_store.client.hset("log:recall:2", mapping={
            "query": "tuning vector index hnsw valkey",
            "timestamp": "1", "result_keys": "[]",
        })
        result = why_did_you_mention("valkey hnsw vector index tuning")
        assert result["status"] == "found"
        assert result["match_type"] == "semantic"
        assert result["similarity"] > 0.5

    def test_explain_memory_bad_effort_score(self, fake_store, fake_embedder):
        from tools.audit import explain_memory

        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "a memory")
        fake_store.set_fields("mem:episodic:01A", {"effort_score": "not a number"})
        result = explain_memory("mem:episodic:01A")
        assert result["status"] == "found"
        assert "effort_score" not in result


# ---------------------------------------------------------------------------
# tools/skills.py
# ---------------------------------------------------------------------------


def _store_skill(fake_store, fake_embedder, **overrides):
    now = str(time.time())
    fields = {
        "name": "python-local", "description": "python things",
        "domain": "python", "user": "local", "state": "active",
        "generated": "true", "body": "---\nname: python-local\n---\n",
        "compiled_at": now, "created_at": now, "updated_at": now,
        "source_manifest": json.dumps([]),
        "rule_manifest": json.dumps([]),
    }
    fields.update(overrides)
    key = overrides.pop("key", "mem:skill:gen:python-local")
    fields.pop("key", None)
    fake_store.upsert("skill", key, fields, fake_embedder.embed("python skill"))
    return key


class TestSkillsBranches:
    def test_suggest_no_active_skills(self, fake_store, fake_embedder):
        from tools.skills import suggest_skills_for_briefing

        _store_skill(fake_store, fake_embedder, state="archived")
        assert suggest_skills_for_briefing(fake_store, fake_embedder, "ctx") == []

    def test_suggest_skips_inactive_search_hit(self, fake_store, fake_embedder, monkeypatch):
        from tools.skills import suggest_skills_for_briefing

        monkeypatch.setenv("SKILL_SUGGEST_MIN_SIMILARITY", "-1.0")
        _store_skill(fake_store, fake_embedder)
        _store_skill(fake_store, fake_embedder, key="mem:skill:gen:rust-local",
                     name="rust-local", domain="rust", state="archived")

        result = suggest_skills_for_briefing(
            fake_store, fake_embedder, "python skill work",
        )
        assert [s["skill_id"] for s in result] == ["mem:skill:gen:python-local"]

    def test_pending_updates_no_active_skills(self, fake_store, fake_embedder):
        from tools.skills import pending_skill_updates

        _store_skill(fake_store, fake_embedder, state="archived")
        assert pending_skill_updates(fake_store) == []

    def test_knowledge_watch_no_active_skills(self, fake_store, fake_embedder):
        from tools.skills import knowledge_watch

        _store_skill(fake_store, fake_embedder, state="archived")
        assert knowledge_watch(fake_store) == []

    def test_knowledge_watch_article_filters(self, fake_store, fake_embedder):
        from tools.skills import knowledge_watch

        now = str(time.time())
        _store_skill(fake_store, fake_embedder,
                     source_manifest=json.dumps(["mem:knowledge:01M"]))
        # Archived article → skipped by the state filter.
        fake_store.upsert("knowledge", "mem:knowledge:01X", {
            "content": "python archived", "state": "archived", "created_at": now,
        }, fake_embedder.embed("python archived"))
        # Article already in the skill's manifest → skipped.
        fake_store.upsert("knowledge", "mem:knowledge:01M", {
            "content": "python skill guidance", "state": "active",
            "created_at": now,
        }, fake_embedder.embed("python skill guidance"))
        # Vectorless article → skipped.
        fake_store.client.hset("mem:knowledge:01V", mapping={
            "content": "fresh python news", "state": "active", "created_at": now,
        })

        assert knowledge_watch(fake_store) == []


# ---------------------------------------------------------------------------
# tools/core.py
# ---------------------------------------------------------------------------


class TestCoreBranches:
    def test_remember_document_no_chunks(self, monkeypatch):
        from tools.core import remember_document

        monkeypatch.setattr("tools.core.chunk_content", lambda *a, **k: [])
        result = remember_document("some content", mode="raw")
        assert result == {"doc_id": None, "keys": [], "chunks_stored": 0}

    def test_remember_document_truncates_oversize_chunk(self, fake_store, monkeypatch):
        from tools.core import MAX_CONTENT_LENGTH, remember_document

        monkeypatch.setattr(
            "tools.core.chunk_content",
            lambda *a, **k: ["x" * (MAX_CONTENT_LENGTH + 100)],
        )
        result = remember_document("seed content", mode="raw")
        assert result["chunks_stored"] == 1
        stored = fake_store.get(result["keys"][0])
        assert len(stored["content"]) == MAX_CONTENT_LENGTH

    def test_remember_document_skips_duplicate_chunks(self):
        from tools.core import remember_document

        content = "Same paragraph text here.\n\nSame paragraph text here."
        result = remember_document(content, mode="raw")
        assert result["chunks_stored"] == 1
        assert result["duplicates_skipped"] == 1

    def test_recall_surfaces_event_date(self, fake_store, fake_embedder):
        from tools.core import recall

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "release shipped on the fourteenth")
        fake_store.set_fields("mem:episodic:01A", {"event_date": "1752000000"})

        results = recall("release shipped")
        entry = next(r for r in results if r["key"] == "mem:episodic:01A")
        assert entry["event_date"] == 1752000000.0


# ---------------------------------------------------------------------------
# tools/backup.py
# ---------------------------------------------------------------------------


class TestBackupBranches:
    def test_symlink_escaping_backup_dir_rejected(self, tmp_path, monkeypatch):
        from tools.backup import dump_to_file

        backups = tmp_path / "backups"
        backups.mkdir()
        outside = tmp_path / "outside.json"
        outside.write_text("{}", encoding="utf-8")
        (backups / "escape.json").symlink_to(outside)
        monkeypatch.setenv("BACKUP_DIR", str(backups))

        result = dump_to_file(filename="escape.json")
        assert result["status"] == "error"
        assert "escape" in result["message"]

    def test_dump_counts_skills(self, fake_store, fake_embedder, tmp_path, monkeypatch):
        from tools.backup import dump_to_file

        monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
        _store_skill(fake_store, fake_embedder)

        result = dump_to_file(filename="dump.json")
        assert result["total_keys"] == 1
        payload = json.loads((tmp_path / "dump.json").read_text(encoding="utf-8"))
        assert payload["metadata"]["namespaces"]["skill"] == 1

    def test_restore_re_embeds_skill_discovery_text(
        self, fake_store, tmp_path, monkeypatch,
    ):
        from tools.backup import restore_from_file

        monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
        backup = {
            "metadata": {"version": "test"},
            "data": {
                "mem:skill:gen:python-local": {
                    "name": "python-local", "description": "python things",
                    "domain": "python", "body": "---\n", "state": "active",
                    "generated": "true", "updated_at": str(time.time()),
                },
                "mem:episodic:01A": {
                    "content": "an ordinary memory", "state": "active",
                    "updated_at": str(time.time()),
                },
            },
        }
        (tmp_path / "restore.json").write_text(
            json.dumps(backup), encoding="utf-8",
        )

        result = restore_from_file("restore.json", dry_run=False)
        assert result["status"] == "restored"
        assert result["restored_keys"] == 2
        assert result["re_embedded"] == 2
        # The skill's vector was regenerated from its discovery metadata.
        vec = fake_store.get_vectors_multi(["mem:skill:gen:python-local"])[0]
        assert vec is not None


# ---------------------------------------------------------------------------
# tools/briefing.py
# ---------------------------------------------------------------------------


class TestBriefingBranches:
    def test_briefing_surfaces_stale_memories(self, fake_store, fake_embedder):
        from tools.briefing import briefing

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "an old decision")
        fake_store.set_fields("mem:episodic:01A", {
            "updated_at": str(time.time() - 40 * 86400),
        })
        result = briefing()
        assert result["stale_memories"][0]["key"] == "mem:episodic:01A"

    def test_briefing_survives_skill_scan_failure(self, fake_store, monkeypatch):
        from tools.briefing import briefing

        monkeypatch.setattr(
            "memory.skill_scan.scan_due",
            MagicMock(side_effect=RuntimeError("scan down")),
        )
        result = briefing(project="p")
        assert "auto_proposed_skills" not in result


# ---------------------------------------------------------------------------
# tools/experience.py
# ---------------------------------------------------------------------------


class TestExperienceBranches:
    def test_log_abandoned_missing_key(self):
        from tools.experience import log_abandoned

        with pytest.raises(ValueError, match="not found"):
            log_abandoned("mem:episodic:missing", "thing", "tool", "no good")

    def test_summary_tolerates_corrupt_abandoned_json(self, fake_store, fake_embedder):
        from tools.experience import experience_summary

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "effortful memory", effort_score=3, outcome="succeeded")
        fake_store.set_fields("mem:episodic:01A", {
            "abandoned_approaches": "{broken",
        })
        result = experience_summary()
        assert result["memories_with_experience"] == 1
        assert "graveyard" not in result


# ---------------------------------------------------------------------------
# oauth/provider.py and oauth/storage.py
# ---------------------------------------------------------------------------


class TestOAuthBranches:
    @pytest.fixture
    def provider(self):
        from oauth.provider import OmniMemOAuthProvider

        return OmniMemOAuthProvider(
            base_url="https://mcp.example.com",
            admin_user="admin",
            admin_password="secret123",
        )

    def test_public_client_gets_no_secret(self, provider):
        from mcp.shared.auth import OAuthClientInformationFull

        client = OAuthClientInformationFull(
            client_id="pub-1",
            client_name="Public Client",
            redirect_uris=["https://app.example.com/cb"],
            token_endpoint_auth_method="none",
            client_secret="accidentally-sent",
        )
        _run(provider.register_client(client))
        assert client.client_secret is None

    def test_refresh_without_absolute_cap_gets_fresh_one(self, provider):
        from oauth.provider import _StoredToken

        from mcp.shared.auth import OAuthClientInformationFull

        client = OAuthClientInformationFull(
            client_id="client-1",
            redirect_uris=["https://app.example.com/cb"],
        )
        # Pre-upgrade token: no absolute_expires_at recorded.
        stored = _StoredToken("rt-legacy", "client-1", ["omnimem"], 3600)
        assert stored.absolute_expires_at is None
        provider._storage.save_refresh(stored)

        token = _run(provider.exchange_refresh_token(client, stored, []))
        successor = provider._refresh_tokens[token.refresh_token]
        assert successor.absolute_expires_at is not None
        assert successor.absolute_expires_at > time.time()

    def test_valkey_storage_connect_zero_retries_unreachable_guard(self):
        from oauth.storage import ValkeyOAuthStorage

        # With zero retries the retry loop never runs, falling through to
        # the defensive terminal raise. No network access is attempted.
        with pytest.raises(RuntimeError, match="unreachable"):
            ValkeyOAuthStorage._connect(max_retries=0)


# ---------------------------------------------------------------------------
# rss_worker/ingester.py
# ---------------------------------------------------------------------------


class TestIngesterBranches:
    def test_get_embedder_lazy_loads_and_caches(self, monkeypatch):
        fake_model = MagicMock(name="model")
        factory = MagicMock(return_value=fake_model)
        monkeypatch.setattr(ingester, "SentenceTransformer", factory)
        monkeypatch.setattr(ingester, "_embedder", None)
        monkeypatch.setenv("EMBEDDING_MODEL", "test-model")

        assert ingester._get_embedder() is fake_model
        assert ingester._get_embedder() is fake_model
        factory.assert_called_once_with("test-model")

    def test_get_valkey_lazy_connects_and_caches(self, monkeypatch):
        fake_client = MagicMock(name="valkey-client")
        fake_valkey = MagicMock()
        fake_valkey.Valkey.return_value = fake_client
        monkeypatch.setattr(ingester, "valkey", fake_valkey)
        monkeypatch.setattr(ingester, "_valkey_client", None)
        monkeypatch.setenv("VALKEY_HOST", "testhost")
        monkeypatch.setenv("VALKEY_PORT", "6390")
        monkeypatch.setenv("VALKEY_PASSWORD", "pw")

        assert ingester._get_valkey() is fake_client
        assert ingester._get_valkey() is fake_client
        fake_valkey.ConnectionPool.assert_called_once()
        assert fake_valkey.ConnectionPool.call_args.kwargs["host"] == "testhost"

    def test_ingest_feed_fetch_failure_counts_error(self, monkeypatch):
        broken = MagicMock()
        broken.parse.side_effect = RuntimeError("network down")
        monkeypatch.setattr(ingester, "feedparser", broken)

        stats = ingester.ingest_feed({"url": "http://feed", "name": "broken"})
        assert stats == {"added": 0, "skipped": 0, "errors": 1}
