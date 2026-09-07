"""Tests for the provenance class (v6.6.2): vocabulary, the shared lineage
engine, the backfill, every write path, recall reporting, the
set_provenance tool, and the web UI surfaces."""

import json
import sys
import time
from pathlib import Path

import numpy as np
import pytest

import tools as tools_module
from memory import provenance as prov
from memory.enrichment import EnrichmentWorker
from memory.extraction import ExtractedFact
from memory.lineage import is_classifiable_key, stamp_lineage
from memory.migrations import migrate_provenance
from tests.conftest import store_memory

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "rss_worker"))


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


def _put(store, key, **fields):
    base = {"content": key, "state": "active",
            "created_at": str(time.time()), "updated_at": str(time.time())}
    base.update(fields)
    store.upsert(key.split(":")[1], key, base, np.zeros(384, dtype=np.float32))


# ---------------------------------------------------------------------------
# memory/provenance.py and memory/lineage.py
# ---------------------------------------------------------------------------


class TestVocabulary:
    @pytest.mark.parametrize("raw,expected", [
        ("retrieved", "retrieved"), ("External", "retrieved"), ("looked up", "retrieved"),
        ("concluded", "concluded"), ("inferred", "concluded"), ("SYSTEM", "concluded"),
        ("asserted", "asserted"), ("user", "asserted"), ("dictated", "asserted"),
    ])
    def test_resolves(self, raw, expected):
        assert prov.resolve_provenance(raw) == expected

    @pytest.mark.parametrize("raw", ["", None, "unknown", "guessed"])
    def test_rejects_unrecognised_including_empty(self, raw):
        # There is no "unknown" provenance: a value is one of three or absent.
        with pytest.raises(ValueError, match="Unrecognised provenance"):
            prov.resolve_provenance(raw)

    def test_validate_class(self):
        assert prov.validate_provenance_class("asserted") == "asserted"
        with pytest.raises(ValueError, match="Invalid provenance class"):
            prov.validate_provenance_class("user")

    def test_every_alias_maps_to_a_class_and_labels_cover_all(self):
        assert set(prov.PROVENANCE_ALIASES.values()) == set(prov.PROVENANCE_CLASSES)
        assert set(prov.PROVENANCE_LABELS) == set(prov.PROVENANCE_CLASSES)

    @pytest.mark.parametrize("namespace,expected", [
        ("episodic", "concluded"), ("project", "concluded"),
        ("preference", "asserted"), ("knowledge", "retrieved"), ("skill", "concluded"),
    ])
    def test_defaults(self, namespace, expected):
        assert prov.default_provenance(namespace) == expected

    def test_worker_constant_matches(self):
        ingester = pytest.importorskip("ingester")
        assert ingester._PROVENANCE_RETRIEVED == prov.PROVENANCE_RETRIEVED


class TestLineageEngine:
    def test_classifiable_keys(self):
        assert is_classifiable_key("mem:episodic:01A")
        assert is_classifiable_key("mem:knowledge:x")
        assert not is_classifiable_key("mem:skill:gen:python-local")
        assert not is_classifiable_key("meta:feed:influence")

    def test_stamp_cascades_by_key_and_doc_id(self, fake_store):
        _put(fake_store, "mem:episodic:01SRC", doc_id="DOC")
        _put(fake_store, "mem:knowledge:01F1", enriched_from="mem:episodic:01SRC")
        _put(fake_store, "mem:preference:01F2", source_doc_id="DOC")
        _put(fake_store, "mem:knowledge:01OTHER")
        out = stamp_lineage(fake_store, ["mem:episodic:01SRC", "mem:episodic:GONE"], {"x": "1"})
        assert out["classified"] == ["mem:episodic:01SRC"]
        assert sorted(out["cascaded"]) == ["mem:knowledge:01F1", "mem:preference:01F2"]
        assert out["not_found"] == ["mem:episodic:GONE"]
        assert fake_store.get("mem:knowledge:01OTHER").get("x") is None

    def test_stamp_never_bumps_updated_at(self, fake_store):
        _put(fake_store, "mem:episodic:01A", updated_at="123.0")
        stamp_lineage(fake_store, ["mem:episodic:01A"], {"provenance": "asserted"})
        assert fake_store.get("mem:episodic:01A")["updated_at"] == "123.0"


# ---------------------------------------------------------------------------
# Backfill
# ---------------------------------------------------------------------------


class TestMigrateProvenance:
    def test_namespace_defaults(self, fake_store):
        _put(fake_store, "mem:episodic:01E")
        _put(fake_store, "mem:project:ctx", stack="python", goals="ship")
        _put(fake_store, "mem:project:01ULID")  # remember(namespace="project") write
        _put(fake_store, "mem:preference:01P")
        _put(fake_store, "mem:knowledge:art", feed_name="Feed")
        _put(fake_store, "mem:knowledge:01K")
        migrate_provenance(fake_store)
        got = {k: fake_store.get(k)["provenance"] for k in (
            "mem:episodic:01E", "mem:project:ctx", "mem:project:01ULID",
            "mem:preference:01P", "mem:knowledge:art", "mem:knowledge:01K",
        )}
        assert got == {
            "mem:episodic:01E": "concluded",
            "mem:project:ctx": "asserted",
            "mem:project:01ULID": "concluded",
            "mem:preference:01P": "asserted",
            "mem:knowledge:art": "retrieved",
            "mem:knowledge:01K": "concluded",
        }

    def test_context_detected_by_key_when_stack_and_goals_blank(self, fake_store):
        _put(fake_store, "mem:project:bare", project_name="bare", stack="", goals="",
             description="only a description")
        _put(fake_store, "mem:project:01ULID2", project_name="bare")
        migrate_provenance(fake_store)
        assert fake_store.get("mem:project:bare")["provenance"] == "asserted"
        assert fake_store.get("mem:project:01ULID2")["provenance"] == "concluded"

    def test_facts_inherit_and_orphans_are_concluded(self, fake_store):
        _put(fake_store, "mem:preference:01SRC")
        _put(fake_store, "mem:episodic:01RET", provenance="retrieved")
        _put(fake_store, "mem:knowledge:01F1", enriched_from="mem:preference:01SRC")
        _put(fake_store, "mem:knowledge:01F2", enriched_from="mem:episodic:01RET")
        _put(fake_store, "mem:knowledge:01F3", enriched_from="mem:episodic:GONE")
        # A preference extracted from an episodic write-up inherits too,
        # matching what live enrichment stamps
        _put(fake_store, "mem:episodic:01E")
        _put(fake_store, "mem:preference:01PF", enriched_from="mem:episodic:01E")
        # A fact whose source is a legacy knowledge write resolves from the
        # same pass, not from write order
        _put(fake_store, "mem:knowledge:01KSRC")
        _put(fake_store, "mem:knowledge:01F4", enriched_from="mem:knowledge:01KSRC")
        migrate_provenance(fake_store)
        assert fake_store.get("mem:knowledge:01F1")["provenance"] == "asserted"
        assert fake_store.get("mem:knowledge:01F2")["provenance"] == "retrieved"
        assert fake_store.get("mem:knowledge:01F3")["provenance"] == "concluded"
        assert fake_store.get("mem:preference:01PF")["provenance"] == "concluded"
        assert fake_store.get("mem:knowledge:01F4")["provenance"] == "concluded"

    def test_article_wins_over_enriched_from(self, fake_store):
        _put(fake_store, "mem:knowledge:odd", feed_name="Feed", enriched_from="mem:episodic:x")
        migrate_provenance(fake_store)
        assert fake_store.get("mem:knowledge:odd")["provenance"] == "retrieved"

    def test_idempotent_never_overwrites(self, fake_store):
        _put(fake_store, "mem:episodic:01A", provenance="asserted")
        _put(fake_store, "mem:knowledge:art", feed_name="F", provenance="asserted")
        migrate_provenance(fake_store)
        migrate_provenance(fake_store)
        assert fake_store.get("mem:episodic:01A")["provenance"] == "asserted"
        assert fake_store.get("mem:knowledge:art")["provenance"] == "asserted"

    def test_empty_store_logs_nothing(self, fake_store, caplog):
        migrate_provenance(fake_store)
        assert "backfilled provenance" not in caplog.text

    def test_logs_counts(self, fake_store, caplog):
        import logging
        caplog.set_level(logging.INFO)
        _put(fake_store, "mem:episodic:01A")
        _put(fake_store, "mem:knowledge:01F", enriched_from="mem:episodic:01A")
        migrate_provenance(fake_store)
        assert "backfilled provenance on 2 memories (1 extracted facts" in caplog.text

    def test_restore_backfills(self, fake_store, tmp_path, monkeypatch):
        from tools import backup as backup_tool
        monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
        dump = {"version": "6.6.1", "created_at": 1.0, "metadata": {},
                "data": {"mem:preference:01P": {"content": "x", "state": "active",
                                                "created_at": "1.0", "updated_at": "1.0"}}}
        (tmp_path / "old.json").write_text(json.dumps(dump))
        backup_tool.restore_from_file("old.json", dry_run=False)
        assert fake_store.get("mem:preference:01P")["provenance"] == "asserted"


# ---------------------------------------------------------------------------
# Write paths
# ---------------------------------------------------------------------------


class TestWritePaths:
    def test_remember_defaults_and_explicit(self, fake_store):
        from tools.core import remember
        r1 = remember("we decided X", mode="raw")
        r2 = remember("Ric said: always use uv", namespace="preference", mode="raw")
        r3 = remember("Docs say the flag is --cpu", namespace="knowledge", mode="raw")
        r4 = remember("Ric told me the deploy is Fridays", mode="raw", provenance="user")
        assert (r1["provenance"], r2["provenance"], r3["provenance"], r4["provenance"]) == (
            "concluded", "asserted", "retrieved", "asserted",
        )
        assert fake_store.get(r4["key"])["provenance"] == "asserted"

    def test_remember_project_namespace_write_is_concluded(self, fake_store):
        from tools.core import remember
        r = remember("deploys go out on Fridays", namespace="project", project="p", mode="raw")
        assert r["provenance"] == "concluded"

    def test_remember_rejects_unrecognised(self, fake_store):
        from tools.core import remember
        with pytest.raises(ValueError, match="Unrecognised provenance"):
            remember("x", mode="raw", provenance="guessed")
        assert not fake_store.scan_prefix("mem:episodic:")

    def test_remember_empty_string_is_default(self):
        from tools.core import remember
        assert remember("x", mode="raw", provenance="")["provenance"] == "concluded"

    def test_remember_document_stamps_every_chunk(self, fake_store):
        from tools.core import remember_document
        r = remember_document("Para one.\n\nPara two here.", mode="raw", provenance="retrieved")
        assert r["provenance"] == "retrieved"
        for key in r["keys"]:
            assert fake_store.get(key)["provenance"] == "retrieved"

    def test_project_context_is_asserted(self, fake_store, fake_embedder):
        from tools.project import compile_project_context, set_project_context
        set_project_context("proj", "desc", "python", "ship", "started")
        assert fake_store.get("mem:project:proj")["provenance"] == "asserted"
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "Decided uv",
                     project="proj2", tags=["python", "decision"])
        compile_project_context("proj2", auto_save=True)
        # A compiled draft is the system's synthesis, not a human statement
        assert fake_store.get("mem:project:proj2")["provenance"] == "concluded"
        # ...but a recompile carries a human's vouch forward
        fake_store.set_field("mem:project:proj2", "provenance", "asserted")
        compile_project_context("proj2", auto_save=True)
        assert fake_store.get("mem:project:proj2")["provenance"] == "asserted"

    def test_enrichment_inherits(self, monkeypatch, fake_store, fake_embedder):
        from memory import enrichment
        monkeypatch.setattr(enrichment, "extract_facts",
                            lambda content: [ExtractedFact(text="A fact", kind="fact")])
        store_memory(fake_store, fake_embedder, "mem:episodic:01SRC", "content")
        fake_store.set_field("mem:episodic:01SRC", "provenance", "asserted")
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": "mem:episodic:01SRC", "namespace": "episodic",
        })
        facts = [fake_store.get(k) for k in fake_store.scan_prefix("mem:knowledge:")]
        assert facts and facts[0]["provenance"] == "asserted"

    def test_enrichment_source_without_provenance_is_concluded(self, monkeypatch, fake_store, fake_embedder):
        from memory import enrichment
        monkeypatch.setattr(enrichment, "extract_facts",
                            lambda content: [ExtractedFact(text="A fact", kind="fact")])
        store_memory(fake_store, fake_embedder, "mem:episodic:01SRC", "content")
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": "mem:episodic:01SRC", "namespace": "episodic",
        })
        facts = [fake_store.get(k) for k in fake_store.scan_prefix("mem:knowledge:")]
        assert facts and facts[0]["provenance"] == "concluded"

    def test_skill_import_defaults_by_namespace(self, fake_store, fake_embedder):
        from memory.skill_transfer import apply_skill_import, build_skill_export, validate_skill_import
        from tests.test_skill_transfer_feeds import _seed_skill
        key = _seed_skill(fake_store, fake_embedder)
        bundle, _ = build_skill_export(fake_store, key)
        target = type(fake_store)()
        apply_skill_import(target, fake_embedder, validate_skill_import(bundle["data"]))
        assert target.get("mem:episodic:01A")["provenance"] == "concluded"

    def test_skill_import_rejects_out_of_vocabulary_value(self, fake_store, fake_embedder):
        from memory.skill_transfer import apply_skill_import, build_skill_export, validate_skill_import
        from tests.test_skill_transfer_feeds import _seed_skill
        key = _seed_skill(fake_store, fake_embedder)
        fake_store.set_field("mem:episodic:01A", "provenance", "Retrieved")
        bundle, _ = build_skill_export(fake_store, key)
        target = type(fake_store)()
        apply_skill_import(target, fake_embedder, validate_skill_import(bundle["data"]))
        assert target.get("mem:episodic:01A")["provenance"] == "concluded"

    def test_skill_import_keeps_bundled_provenance(self, fake_store, fake_embedder):
        from memory.skill_transfer import apply_skill_import, build_skill_export, validate_skill_import
        from tests.test_skill_transfer_feeds import _seed_skill
        key = _seed_skill(fake_store, fake_embedder)
        fake_store.set_field("mem:episodic:01A", "provenance", "asserted")
        bundle, _ = build_skill_export(fake_store, key)
        target = type(fake_store)()
        apply_skill_import(target, fake_embedder, validate_skill_import(bundle["data"]))
        assert target.get("mem:episodic:01A")["provenance"] == "asserted"


class TestIngesterProvenance:
    def test_articles_are_retrieved(self, monkeypatch):
        ingester = pytest.importorskip("ingester")
        from tests.test_rss_worker import FakeIngestValkey, FakeSentenceTransformer, _entry, _patch_feed
        client = FakeIngestValkey()
        monkeypatch.setattr(ingester, "_get_valkey", lambda: client)
        monkeypatch.setattr(ingester, "_get_embedder", lambda: FakeSentenceTransformer())
        _patch_feed(monkeypatch, [_entry(published=True)])
        monkeypatch.setattr(ingester, "summarise", lambda t, u, c: "A summary.")
        ingester.ingest_feed({"url": "https://example.org/feed.xml", "name": "Example"})
        assert list(client.data.values())[0]["provenance"] == "retrieved"


# ---------------------------------------------------------------------------
# Recall surfaces
# ---------------------------------------------------------------------------


class TestReadTimeFallback:
    def test_effective_provenance(self):
        from memory.provenance import effective_provenance
        assert effective_provenance({"provenance": "asserted"}, "episodic") == "asserted"
        assert effective_provenance({"provenance": "Bogus"}, "episodic") == "concluded"
        assert effective_provenance({"feed_name": "F"}, "knowledge") == "retrieved"
        assert effective_provenance({"stack": "python"}, "project") == "asserted"
        assert effective_provenance({}, "project") == "concluded"
        assert effective_provenance({}, "preference") == "asserted"

    def test_effective_licence(self):
        from memory.licence import effective_licence
        assert effective_licence({"licence": "open"}, "knowledge") == "open"
        assert effective_licence({"feed_name": "F"}, "knowledge") == "unknown"
        assert effective_licence({"imported_at": "1"}, "episodic") == "unknown"
        assert effective_licence({}, "episodic") == "own"

    def test_recall_reports_unstamped_article_as_retrieved_unknown(self, fake_store, fake_embedder):
        """A worker image that predates the fields stamps nothing; recall
        still reports the honest defaults instead of omitting the keys."""
        from tools.core import recall, recall_detail
        store_memory(fake_store, fake_embedder, "mem:knowledge:art", "unstamped article",
                     namespace="knowledge")
        fake_store.set_field("mem:knowledge:art", "feed_name", "Feed")
        rows = [r for r in recall("unstamped article", top_k=5) if r.get("key") == "mem:knowledge:art"]
        assert rows[0]["provenance"] == "retrieved"
        assert rows[0]["licence"] == "unknown"
        detail = recall_detail(["mem:knowledge:art"])[0]
        assert detail["provenance"] == "retrieved" and detail["licence"] == "unknown"

    def test_explain_memory_reports_both(self, fake_store, fake_embedder):
        from tools.audit import explain_memory
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "a")
        fake_store.set_fields("mem:episodic:01A", {"licence": "open", "licence_note": "CC BY 4.0",
                                                    "provenance": "asserted"})
        result = explain_memory("mem:episodic:01A")
        assert result["licence"] == "open"
        assert result["licence_note"] == "CC BY 4.0"
        assert result["provenance"] == "asserted"

    def test_briefing_new_knowledge_reports_provenance(self, fake_store, fake_embedder):
        from tools.briefing import _get_new_knowledge
        store_memory(fake_store, fake_embedder, "mem:knowledge:01F", "extracted fact",
                     namespace="knowledge")
        fake_store.set_field("mem:knowledge:01F", "provenance", "concluded")
        assert _get_new_knowledge(fake_store)[0]["provenance"] == "concluded"

    def test_alias_normaliser_shared(self):
        from memory.lineage import normalise_alias_key
        assert normalise_alias_key("Looked - Up") == "looked-up"
        assert normalise_alias_key(None) == ""
        assert normalise_alias_key(False) == "false"
        assert prov.resolve_provenance("looked - up") == "retrieved"


class TestRecallReports:
    def _seed(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01C", "python packaging conclusion")
        fake_store.set_field("mem:episodic:01C", "provenance", "concluded")
        store_memory(fake_store, fake_embedder, "mem:preference:01A", "python packaging preference",
                     namespace="preference")
        fake_store.set_field("mem:preference:01A", "provenance", "asserted")

    def test_recall_and_index_and_detail(self, fake_store, fake_embedder):
        from tools.core import recall, recall_detail, recall_index
        self._seed(fake_store, fake_embedder)
        by_key = {r["key"]: r for r in recall("python packaging", top_k=10) if "key" in r}
        assert by_key["mem:episodic:01C"]["provenance"] == "concluded"
        assert by_key["mem:preference:01A"]["provenance"] == "asserted"
        by_key = {r["key"]: r for r in recall_index("python packaging", top_k=10)["results"]}
        assert by_key["mem:preference:01A"]["provenance"] == "asserted"
        rows = recall_detail(["mem:episodic:01C"])
        assert rows[0]["provenance"] == "concluded"

    def test_no_ranking_effect(self, fake_store, fake_embedder):
        """The field is reported, never scored on: identical content with
        different provenance gets identical adjusted scores."""
        from tools.core import recall
        store_memory(fake_store, fake_embedder, "mem:episodic:01X", "identical text")
        store_memory(fake_store, fake_embedder, "mem:episodic:01Y", "identical text")
        fake_store.set_field("mem:episodic:01X", "provenance", "asserted")
        fake_store.set_field("mem:episodic:01Y", "provenance", "concluded")
        scores = {r["key"]: r["score"] for r in recall("identical text", top_k=5) if "key" in r}
        assert scores["mem:episodic:01X"] == scores["mem:episodic:01Y"]

    def test_recent_knowledge_reports(self, fake_store, fake_embedder):
        from tools.knowledge import recent_knowledge
        store_memory(fake_store, fake_embedder, "mem:knowledge:01U", "x", namespace="knowledge")
        fake_store.set_field("mem:knowledge:01U", "provenance", "retrieved")
        assert recent_knowledge()[0]["provenance"] == "retrieved"


# ---------------------------------------------------------------------------
# set_provenance tool
# ---------------------------------------------------------------------------


class TestSetProvenance:
    def test_reclassifies_and_cascades(self, fake_store, fake_embedder):
        from tools.provenance import set_provenance
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "a")
        fake_store.set_field("mem:episodic:01A", "provenance", "concluded")
        store_memory(fake_store, fake_embedder, "mem:knowledge:01F", "f", namespace="knowledge")
        fake_store.set_fields("mem:knowledge:01F", {"enriched_from": "mem:episodic:01A", "provenance": "concluded"})
        result = set_provenance("asserted", keys=["mem:episodic:01A", "mem:episodic:GONE"])
        assert result["classified"] == 1
        assert result["cascaded_facts"] == ["mem:knowledge:01F"]
        assert result["not_found"] == ["mem:episodic:GONE"]
        assert fake_store.get("mem:episodic:01A")["provenance"] == "asserted"
        assert fake_store.get("mem:knowledge:01F")["provenance"] == "asserted"

    def test_errors(self):
        from tools.provenance import set_provenance
        assert "Unrecognised provenance" in set_provenance("guessed", keys=["mem:episodic:01A"])["error"]
        assert "keys is required" in set_provenance("asserted", keys=[])["error"]
        assert "Too many" in set_provenance("asserted", keys=[f"mem:episodic:{i}" for i in range(201)])["error"]
        assert "carry no provenance" in set_provenance("asserted", keys=["mem:skill:gen:x"])["error"]
        assert "No valid memory keys" in set_provenance("asserted", keys=["meta:x"])["error"]

    def test_non_memory_keys_reported_as_skipped(self, fake_store, fake_embedder):
        from tools.provenance import set_provenance
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "a")
        result = set_provenance("asserted", keys=["mem:episodic:01A", "meta:x", 42])
        assert result["classified"] == 1
        assert result["skipped"] == ["meta:x", 42]

    def test_all_missing(self, fake_store):
        from tools.provenance import set_provenance
        result = set_provenance("asserted", keys=["mem:episodic:GONE"])
        assert result["classified"] == 0 and result["not_found"] == ["mem:episodic:GONE"]


# ---------------------------------------------------------------------------
# Web UI
# ---------------------------------------------------------------------------


class TestWebProvenance:
    def _seed(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01C", "concluded memory")
        fake_store.set_field("mem:episodic:01C", "provenance", "concluded")
        store_memory(fake_store, fake_embedder, "mem:preference:01A", "asserted preference",
                     namespace="preference")
        fake_store.set_field("mem:preference:01A", "provenance", "asserted")

    def test_list_filter(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/memories?provenance=asserted")
        assert "mem:preference:01A" in resp.text
        assert "mem:episodic:01C" not in resp.text
        assert 'for="filter-provenance"' in resp.text
        resp = web_client.get("/memories?provenance=bogus")
        assert "mem:episodic:01C" in resp.text

    def test_filter_in_pagination_params(self, web_client, fake_store, fake_embedder):
        for i in range(30):
            store_memory(fake_store, fake_embedder, f"mem:episodic:{i:03d}", f"m {i}")
            fake_store.set_field(f"mem:episodic:{i:03d}", "provenance", "concluded")
        assert "&amp;provenance=concluded" in web_client.get("/memories?provenance=concluded").text

    def test_detail_shows_and_form_posts(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/memory/mem:episodic:01C")
        assert "Concluded (system reasoning)" in resp.text
        assert 'action="/memory/mem:episodic:01C/provenance"' in resp.text
        resp = web_client.post("/memory/mem:episodic:01C/provenance", data={"provenance": "asserted"},
                               follow_redirects=False)
        assert resp.status_code == 303
        assert fake_store.get("mem:episodic:01C")["provenance"] == "asserted"

    def test_detail_unrecorded_does_not_preselect(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01X", "no provenance")
        resp = web_client.get("/memory/mem:episodic:01X")
        assert "Not recorded" in resp.text
        assert 'id="provenance-select"' in resp.text

    def test_detail_invalid_bounces(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.post("/memory/mem:episodic:01C/provenance", data={"provenance": "guessed"},
                               follow_redirects=False)
        assert "provenance_error=" in resp.headers["location"]
        assert "Unrecognised provenance" in web_client.get(resp.headers["location"]).text

    def test_detail_refuses_skill_and_missing(self, web_client, fake_store, fake_embedder):
        for key in ("mem:skill:gen:x", "mem:episodic:GONE"):
            resp = web_client.post(f"/memory/{key}/provenance", data={"provenance": "asserted"},
                                   follow_redirects=False)
            assert resp.status_code == 303

    def test_create_default_and_explicit(self, web_client, fake_store):
        resp = web_client.post("/create", data={
            "content": "pref text", "namespace": "preference", "force": "on",
        }, follow_redirects=False)
        key = resp.headers["location"].split("/memory/")[1]
        assert fake_store.get(key)["provenance"] == "asserted"
        resp = web_client.post("/create", data={
            "content": "an article summary", "provenance": "retrieved", "force": "on",
        }, follow_redirects=False)
        key = resp.headers["location"].split("/memory/")[1]
        assert fake_store.get(key)["provenance"] == "retrieved"

    def test_create_invalid_rerenders(self, web_client, fake_store):
        resp = web_client.post("/create", data={
            "content": "x", "provenance": "guessed", "force": "on",
        })
        assert "Unrecognised provenance" in resp.text
        assert not fake_store.scan_prefix("mem:episodic:")

    def test_project_routes_are_asserted(self, web_client, fake_store, fake_embedder):
        from tests.test_web_project_domains import seed_project
        web_client.post("/projects/new", data={
            "name": "fresh", "description": "d", "stack": "python", "goals": "g",
            "current_state": "s", "notes": "", "domains": "",
        }, follow_redirects=False)
        assert fake_store.get("mem:project:fresh")["provenance"] == "asserted"
        seed_project(fake_store, fake_embedder)
        web_client.post("/projects/webproj/edit", data={
            "description": "d", "stack": "python", "goals": "g",
            "current_state": "s", "notes": "", "domains": "",
        }, follow_redirects=False)
        assert fake_store.get("mem:project:webproj")["provenance"] == "asserted"

    def test_web_restore_backfills(self, web_client, fake_store, tmp_path, monkeypatch):
        monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
        dump = {"version": "6.6.1", "created_at": 1.0, "metadata": {},
                "data": {"mem:knowledge:art": {"content": "x", "state": "active", "feed_name": "F",
                                               "created_at": "1.0", "updated_at": "1.0"}}}
        (tmp_path / "old.json").write_text(json.dumps(dump))
        web_client.post("/backups/old.json/restore", follow_redirects=False)
        assert fake_store.get("mem:knowledge:art")["provenance"] == "retrieved"
