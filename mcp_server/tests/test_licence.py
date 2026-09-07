"""Tests for the licence / redistribution field (v6.6.1).

Covers the vocabulary module, the parity of the RSS worker's mirrored alias
table, the startup backfill, the write paths that stamp it (remember,
remember_document, project context, enrichment, the ingester), the recall
surfaces that report it, and the set_licence tool.
"""

import sys
import time
from pathlib import Path

import pytest

import tools as tools_module
from memory import licence as lic
from memory.enrichment import EnrichmentWorker
from memory.extraction import ExtractedFact
from memory.migrations import migrate_licence
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


# ---------------------------------------------------------------------------
# memory/licence.py
# ---------------------------------------------------------------------------


class TestResolveLicence:
    @pytest.mark.parametrize("raw,expected", [
        ("own", ("own", None)),
        ("Open", ("open", None)),
        ("RESTRICTED", ("restricted", None)),
        ("unknown", ("unknown", None)),
        ("", ("unknown", None)),
        (None, ("unknown", None)),
        ("  tbd ", ("unknown", None)),
        ("in-house", ("own", None)),
        ("in_house", ("own", None)),
        ("ogl", ("open", "OGL v3.0")),
        ("OGL-3.0", ("open", "OGL v3.0")),
        ("CC BY 4.0", ("open", "CC BY 4.0")),
        ("cc_by_sa", ("open", "CC BY-SA 4.0")),
        ("public domain", ("open", "Public domain")),
        ("cc-by-nc", ("restricted", "CC BY-NC 4.0")),
        ("CC BY-ND 4.0", ("restricted", "CC BY-ND 4.0")),
        ("All Rights Reserved", ("restricted", "All rights reserved")),
        ("paywalled", ("restricted", "Paywalled")),
        ("crown copyright", ("restricted", "Crown copyright (not under OGL)")),
    ])
    def test_resolves_classes_and_identifiers(self, raw, expected):
        assert lic.resolve_licence(raw) == expected

    def test_unrecognised_identifier_is_rejected_not_guessed(self):
        with pytest.raises(ValueError, match="Unrecognised licence 'wtfpl'"):
            lic.resolve_licence("wtfpl")

    def test_every_alias_resolves_to_a_valid_class(self):
        for key, (cls, note) in lic.LICENCE_ALIASES.items():
            assert cls in lic.LICENCE_CLASSES, key
            assert note is None or (isinstance(note, str) and note)
            # The table must be keyed in its own normalised form or lookups
            # would miss it.
            assert lic.normalise_alias_key(key) == key

    def test_labels_cover_every_class(self):
        assert set(lic.LICENCE_LABELS) == set(lic.LICENCE_CLASSES)


class TestValidation:
    def test_validate_class_accepts_and_rejects(self):
        assert lic.validate_licence_class("open") == "open"
        with pytest.raises(ValueError, match="Invalid licence class"):
            lic.validate_licence_class("cc-by-4.0")

    def test_note_is_trimmed_bounded_and_optional(self):
        assert lic.validate_licence_note(None) is None
        assert lic.validate_licence_note("   ") is None
        assert lic.validate_licence_note("  CC   BY  ") == "CC BY"
        with pytest.raises(ValueError, match="200 characters"):
            lic.validate_licence_note("x" * 201)

    def test_licence_fields_shape(self):
        assert lic.licence_fields("open") == {"licence": "open"}
        assert lic.licence_fields("open", "OGL v3.0") == {
            "licence": "open", "licence_note": "OGL v3.0",
        }
        with pytest.raises(ValueError):
            lic.licence_fields("nope")

    @pytest.mark.parametrize("namespace,expected", [
        ("episodic", "own"), ("project", "own"), ("preference", "own"),
        ("knowledge", "unknown"), ("skill", "unknown"),
    ])
    def test_default_licence_by_namespace(self, namespace, expected):
        assert lic.default_licence(namespace) == expected


class TestWorkerParity:
    """The worker image doesn't ship the memory package, so it carries a copy
    of the alias table. Drift between the two would mean feeds.yml resolves
    differently in the ingester and the web UI."""

    def test_alias_tables_are_identical(self):
        ingester = pytest.importorskip("ingester")
        assert ingester._LICENCE_ALIASES == lic.LICENCE_ALIASES
        assert ingester._MAX_LICENCE_NOTE == lic.MAX_LICENCE_NOTE
        assert ingester._LICENCE_UNKNOWN == lic.LICENCE_UNKNOWN

    @pytest.mark.parametrize("raw", ["CC BY 4.0", "ogl_3", "  Open ", "cc-by-nd"])
    def test_normalisation_agrees(self, raw):
        ingester = pytest.importorskip("ingester")
        fields = ingester._resolve_licence({"name": "f", "licence": raw})
        cls, note = lic.resolve_licence(raw)
        assert fields["licence"] == cls
        assert fields.get("licence_note") == note


# ---------------------------------------------------------------------------
# Startup backfill
# ---------------------------------------------------------------------------


def _put(store, key, **fields):
    import numpy as np
    base = {"content": key, "state": "active",
            "created_at": str(time.time()), "updated_at": str(time.time())}
    base.update(fields)
    store.upsert(key.split(":")[1], key, base, np.zeros(384, dtype=np.float32))


class TestMigrateLicence:
    def test_conversation_namespaces_become_own(self, fake_store):
        _put(fake_store, "mem:episodic:01A")
        _put(fake_store, "mem:project:proj", stack="python")
        _put(fake_store, "mem:preference:01P")
        migrate_licence(fake_store)
        for key in ("mem:episodic:01A", "mem:project:proj", "mem:preference:01P"):
            assert fake_store.get(key)["licence"] == "own"

    def test_rss_articles_and_bare_knowledge_become_unknown(self, fake_store):
        _put(fake_store, "mem:knowledge:art1", feed_name="Feed", source_url="https://x")
        _put(fake_store, "mem:knowledge:01K")
        migrate_licence(fake_store)
        assert fake_store.get("mem:knowledge:art1")["licence"] == "unknown"
        assert fake_store.get("mem:knowledge:01K")["licence"] == "unknown"

    def test_extracted_facts_inherit_from_their_source(self, fake_store):
        _put(fake_store, "mem:episodic:01SRC")
        _put(fake_store, "mem:episodic:01RES", licence="restricted",
             licence_note="Paywalled")
        _put(fake_store, "mem:knowledge:01F1", enriched_from="mem:episodic:01SRC")
        _put(fake_store, "mem:knowledge:01F2", enriched_from="mem:episodic:01RES")
        _put(fake_store, "mem:knowledge:01F3", enriched_from="mem:episodic:01GONE")
        migrate_licence(fake_store)
        # Source stamped own in the same run → fact inherits own
        assert fake_store.get("mem:knowledge:01F1")["licence"] == "own"
        # Source already classified → fact takes that class
        assert fake_store.get("mem:knowledge:01F2")["licence"] == "restricted"
        # Source gone → it was a conversation write (the only thing ever
        # enriched), so own — the same answer the read-time fallback gives
        assert fake_store.get("mem:knowledge:01F3")["licence"] == "own"

    def test_imported_fact_is_unknown_not_own(self, fake_store):
        # A fact that arrived in a bundle: its source lives on another
        # instance, and imported means unknown whatever the namespace
        _put(fake_store, "mem:knowledge:01IF", enriched_from="mem:episodic:REMOTE", imported_at="1")
        migrate_licence(fake_store)
        assert fake_store.get("mem:knowledge:01IF")["licence"] == "unknown"

    def test_article_with_enriched_from_is_still_an_article(self, fake_store):
        # An RSS article never carries enriched_from, but if one did the
        # feed_name must win: it is third-party content, not a derivative
        # of our own memory.
        _put(fake_store, "mem:episodic:01SRC")
        _put(fake_store, "mem:knowledge:odd", feed_name="Feed",
             enriched_from="mem:episodic:01SRC")
        migrate_licence(fake_store)
        assert fake_store.get("mem:knowledge:odd")["licence"] == "unknown"

    def test_idempotent_and_never_overwrites(self, fake_store):
        _put(fake_store, "mem:episodic:01A", licence="restricted")
        _put(fake_store, "mem:knowledge:art", feed_name="Feed", licence="open")
        migrate_licence(fake_store)
        migrate_licence(fake_store)
        assert fake_store.get("mem:episodic:01A")["licence"] == "restricted"
        assert fake_store.get("mem:knowledge:art")["licence"] == "open"

    def test_empty_store_is_a_no_op(self, fake_store, caplog):
        migrate_licence(fake_store)
        assert "backfilled licence" not in caplog.text

    def test_logs_counts(self, fake_store, caplog):
        import logging
        caplog.set_level(logging.INFO)
        _put(fake_store, "mem:episodic:01A")
        _put(fake_store, "mem:knowledge:art", feed_name="Feed")
        _put(fake_store, "mem:knowledge:fact", enriched_from="mem:episodic:01A")
        _put(fake_store, "mem:preference:pf", enriched_from="mem:episodic:01A")
        migrate_licence(fake_store)
        assert fake_store.get("mem:preference:pf")["licence"] == "own"
        assert "backfilled licence on 4 memories (1 own, 1 unknown, 2 extracted" in caplog.text


# ---------------------------------------------------------------------------
# Write paths
# ---------------------------------------------------------------------------


class TestRememberLicence:
    def test_defaults_by_namespace(self, fake_store):
        from tools.core import remember
        r1 = remember("A decision we made", mode="raw")
        r2 = remember("An article summary", namespace="knowledge", mode="raw")
        assert r1["licence"] == "own"
        assert r2["licence"] == "unknown"
        assert fake_store.get(r1["key"])["licence"] == "own"
        assert fake_store.get(r2["key"])["licence"] == "unknown"
        assert "licence_note" not in fake_store.get(r1["key"])

    def test_explicit_identifier_keeps_note(self, fake_store):
        from tools.core import remember
        r = remember("Approved Document B says...", namespace="knowledge",
                     mode="raw", licence="OGL 3.0")
        data = fake_store.get(r["key"])
        assert r["licence"] == "open"
        assert data["licence"] == "open"
        assert data["licence_note"] == "OGL v3.0"

    def test_unrecognised_licence_rejected_before_write(self, fake_store):
        from tools.core import remember
        with pytest.raises(ValueError, match="Unrecognised licence"):
            remember("Something", mode="raw", licence="wtfpl")
        assert not fake_store.scan_prefix("mem:episodic:")

    def test_remember_document_stamps_every_chunk(self, fake_store):
        from tools.core import remember_document
        r = remember_document(
            "Para one about a topic.\n\nPara two about another topic entirely.",
            mode="raw", licence="restricted",
        )
        assert r["licence"] == "restricted"
        assert r["chunks_stored"] == 2
        for key in r["keys"]:
            assert fake_store.get(key)["licence"] == "restricted"

    def test_remember_document_default(self, fake_store):
        from tools.core import remember_document
        r = remember_document("Some text.\n\nMore text here.", mode="raw",
                              namespace="knowledge")
        assert r["licence"] == "unknown"


class TestProjectContextLicence:
    def test_set_project_context_is_own(self, fake_store):
        from tools.project import set_project_context
        set_project_context("proj", "desc", "python", "ship", "started")
        assert fake_store.get("mem:project:proj")["licence"] == "own"

    def test_compile_project_context_auto_save_is_own(self, fake_store, fake_embedder):
        from tools.project import compile_project_context
        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "Decided to use uv for packaging", project="proj",
                     tags=["python", "decision"])
        compile_project_context("proj", auto_save=True)
        assert fake_store.get("mem:project:proj")["licence"] == "own"


class TestEnrichmentInheritsLicence:
    def _facts(self, monkeypatch):
        from memory import enrichment
        monkeypatch.setattr(
            enrichment, "extract_facts",
            lambda content: [
                ExtractedFact(text="A fact", kind="fact"),
                ExtractedFact(text="A preference", kind="preference"),
            ],
        )

    def test_single_key_facts_take_source_licence(self, monkeypatch, fake_store, fake_embedder):
        self._facts(monkeypatch)
        store_memory(fake_store, fake_embedder, "mem:episodic:01SRC", "content")
        fake_store.set_fields("mem:episodic:01SRC", {
            "licence": "restricted", "licence_note": "Paywalled",
        })
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": "mem:episodic:01SRC", "namespace": "episodic",
        })
        facts = [fake_store.get(k) for k in fake_store.scan_prefix("mem:knowledge:")]
        prefs = [fake_store.get(k) for k in fake_store.scan_prefix("mem:preference:")]
        assert facts and prefs
        for row in facts + prefs:
            assert row["licence"] == "restricted"
            assert row["licence_note"] == "Paywalled"

    def test_batch_mode_reads_first_chunk(self, monkeypatch, fake_store, fake_embedder):
        self._facts(monkeypatch)
        store_memory(fake_store, fake_embedder, "mem:episodic:01C0", "chunk 0")
        fake_store.set_field("mem:episodic:01C0", "licence", "open")
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": "mem:episodic:01C0", "namespace": "episodic",
            "batch_mode": True, "batch_content": "chunk 0 chunk 1",
            "created_at": "1.0",
        })
        facts = [fake_store.get(k) for k in fake_store.scan_prefix("mem:knowledge:")]
        assert facts and all(f["licence"] == "open" for f in facts)

    def test_source_without_licence_yields_own(self, monkeypatch, fake_store, fake_embedder):
        """Only conversation writes are ever enriched, so an unstamped source
        is own — the same answer the backfill gives it."""
        self._facts(monkeypatch)
        store_memory(fake_store, fake_embedder, "mem:episodic:01SRC", "content")
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": "mem:episodic:01SRC", "namespace": "episodic",
        })
        facts = [fake_store.get(k) for k in fake_store.scan_prefix("mem:knowledge:")]
        assert facts and all(f["licence"] == "own" for f in facts)

    def test_batch_mode_missing_first_chunk_and_no_payload_yields_own(self, monkeypatch, fake_store, fake_embedder):
        self._facts(monkeypatch)
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": "mem:episodic:01GONE", "namespace": "episodic",
            "batch_mode": True, "batch_content": "text",
        })
        facts = [fake_store.get(k) for k in fake_store.scan_prefix("mem:knowledge:")]
        assert facts and all(f["licence"] == "own" for f in facts)


# ---------------------------------------------------------------------------
# Recall surfaces
# ---------------------------------------------------------------------------


class TestRecallReportsLicence:
    def _seed(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01OWN",
                     "Python packaging with uv works well")
        fake_store.set_field("mem:episodic:01OWN", "licence", "own")
        store_memory(fake_store, fake_embedder, "mem:knowledge:01UNK",
                     "Python packaging article from a feed", namespace="knowledge")
        fake_store.set_field("mem:knowledge:01UNK", "licence", "unknown")
        store_memory(fake_store, fake_embedder, "mem:knowledge:01OPEN",
                     "Python packaging guide under OGL", namespace="knowledge")
        fake_store.set_fields("mem:knowledge:01OPEN", {
            "licence": "open", "licence_note": "OGL v3.0",
        })

    def test_recall_carries_licence_and_trailing_notice(self, fake_store, fake_embedder):
        from tools.core import recall
        self._seed(fake_store, fake_embedder)
        results = recall("python packaging", top_k=10)
        by_key = {r["key"]: r for r in results if "key" in r}
        assert by_key["mem:episodic:01OWN"]["licence"] == "own"
        assert by_key["mem:knowledge:01OPEN"]["licence"] == "open"
        assert by_key["mem:knowledge:01OPEN"]["licence_note"] == "OGL v3.0"
        notice = results[-1]
        assert notice["result_type"] == "licence_notice"
        assert notice["unclassified"] == ["mem:knowledge:01UNK"]
        assert "1 result above has no recorded" in notice["note"]
        assert "set_licence(" in notice["note"]

    def test_no_notice_when_everything_is_classified(self, fake_store, fake_embedder):
        from tools.core import recall
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "classified content")
        fake_store.set_field("mem:episodic:01A", "licence", "own")
        results = recall("classified", top_k=5)
        assert all(r.get("result_type") != "licence_notice" for r in results)

    def test_notice_pluralises(self, fake_store, fake_embedder):
        from tools.core import recall
        for i in range(2):
            store_memory(fake_store, fake_embedder, f"mem:knowledge:0{i}",
                         f"unclassified article {i}", namespace="knowledge")
            fake_store.set_field(f"mem:knowledge:0{i}", "licence", "unknown")
        notice = recall("unclassified article", top_k=5)[-1]
        assert notice["result_type"] == "licence_notice"
        assert "2 results above have no recorded" in notice["note"]

    def test_notice_ignores_non_memory_rows(self):
        from tools.core import _licence_notice
        assert _licence_notice([
            {"key": "x", "licence": "unknown", "result_type": "abandoned_warning"},
        ]) is None

    def test_recall_index_reports_licence_and_notice(self, fake_store, fake_embedder):
        from tools.core import recall_index
        self._seed(fake_store, fake_embedder)
        payload = recall_index("python packaging", top_k=10)
        by_key = {r["key"]: r for r in payload["results"]}
        assert by_key["mem:knowledge:01UNK"]["licence"] == "unknown"
        assert payload["licence_notice"]["unclassified"] == ["mem:knowledge:01UNK"]

    def test_recall_index_no_notice_when_classified(self, fake_store, fake_embedder):
        from tools.core import recall_index
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "classified content")
        fake_store.set_field("mem:episodic:01A", "licence", "own")
        assert "licence_notice" not in recall_index("classified")

    def test_recall_detail_reports_licence(self, fake_store, fake_embedder):
        from tools.core import recall_detail
        self._seed(fake_store, fake_embedder)
        rows = recall_detail(["mem:knowledge:01OPEN", "mem:episodic:01OWN"])
        assert rows[0]["licence"] == "open"
        assert rows[0]["licence_note"] == "OGL v3.0"
        assert rows[1]["licence"] == "own"
        assert "licence_note" not in rows[1]


class TestKnowledgeToolsReportLicence:
    def test_recent_knowledge_filters_and_reports(self, fake_store, fake_embedder):
        from tools.knowledge import recent_knowledge
        store_memory(fake_store, fake_embedder, "mem:knowledge:01U", "unknown one",
                     namespace="knowledge")
        fake_store.set_field("mem:knowledge:01U", "licence", "unknown")
        store_memory(fake_store, fake_embedder, "mem:knowledge:01O", "open one",
                     namespace="knowledge")
        fake_store.set_fields("mem:knowledge:01O", {"licence": "open", "licence_note": "CC BY 4.0"})

        everything = recent_knowledge()
        assert {r["key"]: r["licence"] for r in everything} == {
            "mem:knowledge:01U": "unknown", "mem:knowledge:01O": "open",
        }
        unknown_only = recent_knowledge(licence="unknown")
        assert [r["key"] for r in unknown_only] == ["mem:knowledge:01U"]
        assert recent_knowledge(licence="open")[0]["licence_note"] == "CC BY 4.0"

    def test_recent_knowledge_rejects_bad_licence_filter(self):
        from tools.knowledge import recent_knowledge
        with pytest.raises(ValueError, match="Invalid licence class"):
            recent_knowledge(licence="cc-by")

    def test_briefing_new_knowledge_carries_licence(self, fake_store, fake_embedder):
        from tools.briefing import _get_new_knowledge
        store_memory(fake_store, fake_embedder, "mem:knowledge:01U", "fresh article",
                     namespace="knowledge")
        fake_store.set_field("mem:knowledge:01U", "licence", "unknown")
        assert _get_new_knowledge(fake_store)[0]["licence"] == "unknown"


# ---------------------------------------------------------------------------
# set_licence tool
# ---------------------------------------------------------------------------


class TestSetLicence:
    def test_requires_exactly_one_target(self):
        from tools.licence import set_licence
        assert "error" in set_licence("open")
        assert "error" in set_licence("open", keys=["mem:episodic:01A"], feed_name="F")

    def test_rejects_unrecognised_licence(self):
        from tools.licence import set_licence
        assert "Unrecognised licence" in set_licence("wtfpl", keys=["mem:episodic:01A"])["error"]

    def test_rejects_over_long_note(self):
        from tools.licence import set_licence
        result = set_licence("open", keys=["mem:episodic:01A"], note="x" * 201)
        assert "200 characters" in result["error"]

    def test_classifies_keys_and_reports_missing(self, fake_store, fake_embedder):
        from tools.licence import set_licence
        store_memory(fake_store, fake_embedder, "mem:knowledge:01A", "a", namespace="knowledge")
        store_memory(fake_store, fake_embedder, "mem:knowledge:01B", "b", namespace="knowledge")
        result = set_licence("cc-by-4.0", keys=[
            "mem:knowledge:01A", "mem:knowledge:01B", "mem:knowledge:01GONE", "not-a-key",
        ])
        assert result["licence"] == "open"
        assert result["licence_note"] == "CC BY 4.0"
        assert result["classified"] == 2
        assert result["keys"] == ["mem:knowledge:01A", "mem:knowledge:01B"]
        assert result["not_found"] == ["mem:knowledge:01GONE"]
        assert fake_store.get("mem:knowledge:01A")["licence"] == "open"
        assert fake_store.get("mem:knowledge:01A")["licence_note"] == "CC BY 4.0"

    def test_explicit_note_overrides_identifier_note(self, fake_store, fake_embedder):
        from tools.licence import set_licence
        store_memory(fake_store, fake_embedder, "mem:knowledge:01A", "a", namespace="knowledge")
        set_licence("ogl", keys=["mem:knowledge:01A"], note="Checked gov.uk 2026-09-07")
        assert fake_store.get("mem:knowledge:01A")["licence_note"] == "Checked gov.uk 2026-09-07"

    def test_reclassifying_clears_stale_note(self, fake_store, fake_embedder):
        from tools.licence import set_licence
        store_memory(fake_store, fake_embedder, "mem:knowledge:01A", "a", namespace="knowledge")
        set_licence("cc-by-4.0", keys=["mem:knowledge:01A"])
        result = set_licence("restricted", keys=["mem:knowledge:01A"])
        assert "licence_note" not in result
        assert fake_store.get("mem:knowledge:01A")["licence"] == "restricted"
        assert not fake_store.get("mem:knowledge:01A").get("licence_note")

    def test_no_valid_keys(self):
        from tools.licence import set_licence
        assert "No valid memory keys" in set_licence("open", keys=["nope"])["error"]

    def test_skills_are_refused(self, fake_store, fake_embedder):
        from tools.licence import set_licence
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "a")
        result = set_licence("open", keys=["mem:skill:gen:python-local", "mem:episodic:01A"])
        assert "carry no licence" in result["error"]
        assert result["skill_keys"] == ["mem:skill:gen:python-local"]
        assert "licence" not in fake_store.get("mem:episodic:01A")

    def test_record_without_state_still_counts_as_found(self, fake_store):
        import numpy as np
        from tools.licence import set_licence
        fake_store.upsert("knowledge", "mem:knowledge:01N", {
            "content": "no state field", "created_at": "1.0",
        }, np.zeros(384, dtype=np.float32))
        result = set_licence("open", keys=["mem:knowledge:01N"])
        assert result["classified"] == 1

    def test_all_keys_missing_writes_nothing(self, fake_store):
        from tools.licence import set_licence
        result = set_licence("open", keys=["mem:knowledge:01GONE"])
        assert result["classified"] == 0
        assert result["not_found"] == ["mem:knowledge:01GONE"]

    def test_too_many_keys(self):
        from tools.licence import set_licence
        result = set_licence("open", keys=[f"mem:knowledge:{i}" for i in range(201)])
        assert "Too many keys" in result["error"]

    def test_feed_name_classifies_every_article_from_that_feed(self, fake_store, fake_embedder):
        from tools.licence import set_licence
        for i, feed in enumerate(["Gov Feed", "Gov Feed", "Other"]):
            store_memory(fake_store, fake_embedder, f"mem:knowledge:0{i}", f"a{i}",
                         namespace="knowledge")
            fake_store.set_fields(f"mem:knowledge:0{i}", {"feed_name": feed, "licence": "unknown"})
        store_memory(fake_store, fake_embedder, "mem:episodic:01E", "not an article")

        result = set_licence("ogl-3.0", feed_name="Gov Feed")
        assert result["classified"] == 2
        assert result["licence"] == "open"
        assert "feeds.yml" in result["note"]
        assert fake_store.get("mem:knowledge:00")["licence"] == "open"
        assert fake_store.get("mem:knowledge:01")["licence_note"] == "OGL v3.0"
        assert fake_store.get("mem:knowledge:02")["licence"] == "unknown"

    def test_feed_name_with_no_articles(self, fake_store):
        from tools.licence import set_licence
        assert "No knowledge articles" in set_licence("open", feed_name="Nope")["error"]


# ---------------------------------------------------------------------------
# Skill bundles carry a feed's declared licence
# ---------------------------------------------------------------------------


class TestBundleFeedLicence:
    def test_feed_licence_travels_and_validates(self, fake_store, fake_embedder):
        from memory.feed_influence import sync_feed_influences
        from memory.skill_transfer import build_skill_export, validate_skill_import
        from tests.test_skill_transfer_feeds import _seed_skill

        key = _seed_skill(fake_store, fake_embedder)
        sync_feed_influences(fake_store.client, [
            {"name": "Gov Feed", "url": "https://gov.example/feed",
             "licence": "ogl-3.0", "skills": {"python": 4}},
        ])
        bundle, err = build_skill_export(fake_store, key)
        assert err is None
        result = validate_skill_import(bundle["data"])
        assert result["ok"], result
        assert result["feeds"][0]["licence"] == "ogl-3.0"

    @pytest.mark.parametrize("licence", ["wtfpl", 42])
    def test_unrecognised_feed_licence_rejected(self, fake_store, fake_embedder, licence):
        from memory.skill_transfer import validate_skill_import
        from tests.test_skill_transfer_feeds import _export, _patch_bundle

        bundle = _export(fake_store, fake_embedder)
        feeds = [{"name": "F", "url": "https://x.example", "skills": {"python": 5},
                  "licence": licence}]
        result = validate_skill_import(_patch_bundle(bundle["data"], feeds=feeds))
        assert not result["ok"]
        assert "unrecognised licence" in result["error"]

    def test_imported_memories_without_licence_are_unknown(self, fake_store, fake_embedder):
        from memory.skill_transfer import apply_skill_import, build_skill_export, validate_skill_import
        from tests.test_skill_transfer_feeds import _seed_skill

        key = _seed_skill(fake_store, fake_embedder)  # sources carry no licence
        bundle, _ = build_skill_export(fake_store, key)
        validated = validate_skill_import(bundle["data"])
        # Import into an empty instance
        target = type(fake_store)()
        apply_skill_import(target, fake_embedder, validated)
        assert target.get("mem:episodic:01A")["licence"] == "unknown"

    def test_imported_memories_keep_a_bundled_licence(self, fake_store, fake_embedder):
        from memory.skill_transfer import apply_skill_import, build_skill_export, validate_skill_import
        from tests.test_skill_transfer_feeds import _seed_skill

        key = _seed_skill(fake_store, fake_embedder)
        fake_store.set_field("mem:episodic:01A", "licence", "open")
        bundle, _ = build_skill_export(fake_store, key)
        target = type(fake_store)()
        apply_skill_import(target, fake_embedder, validate_skill_import(bundle["data"]))
        assert target.get("mem:episodic:01A")["licence"] == "open"

    def test_empty_feed_licence_is_dropped(self, fake_store, fake_embedder):
        from memory.skill_transfer import validate_skill_import
        from tests.test_skill_transfer_feeds import _export, _patch_bundle

        bundle = _export(fake_store, fake_embedder)
        feeds = [{"name": "F", "url": "https://x.example", "skills": {"python": 5},
                  "licence": ""}]
        result = validate_skill_import(_patch_bundle(bundle["data"], feeds=feeds))
        assert result["ok"], result
        assert "licence" not in result["feeds"][0]


# ---------------------------------------------------------------------------
# Feed influence mirror carries the declared licence
# ---------------------------------------------------------------------------


def test_feed_mirror_carries_licence(fake_store):
    from memory.feed_influence import load_feed_influences, sync_feed_influences
    sync_feed_influences(fake_store.client, [
        {"name": "A", "url": "https://a", "licence": "cc-by-4.0"},
        {"name": "B", "url": "https://b"},
    ])
    mirrored = load_feed_influences(fake_store.client)
    assert mirrored["A"]["licence"] == "cc-by-4.0"
    assert "licence" not in mirrored["B"]


# ---------------------------------------------------------------------------
# Second-review fixes
# ---------------------------------------------------------------------------


class TestClassifyCascade:
    def test_facts_follow_their_source(self, fake_store, fake_embedder):
        from tools.licence import set_licence
        store_memory(fake_store, fake_embedder, "mem:episodic:01SRC", "source")
        store_memory(fake_store, fake_embedder, "mem:knowledge:01F", "fact", namespace="knowledge")
        fake_store.set_fields("mem:knowledge:01F", {"enriched_from": "mem:episodic:01SRC", "licence": "unknown"})
        store_memory(fake_store, fake_embedder, "mem:preference:01P", "pref", namespace="preference")
        fake_store.set_fields("mem:preference:01P", {"enriched_from": "mem:episodic:01SRC", "licence": "unknown"})
        store_memory(fake_store, fake_embedder, "mem:knowledge:01OTHER", "unrelated", namespace="knowledge")
        fake_store.set_field("mem:knowledge:01OTHER", "licence", "unknown")

        result = set_licence("open", keys=["mem:episodic:01SRC"])
        assert result["classified"] == 1
        assert sorted(result["cascaded_facts"]) == ["mem:knowledge:01F", "mem:preference:01P"]
        assert fake_store.get("mem:knowledge:01F")["licence"] == "open"
        assert fake_store.get("mem:preference:01P")["licence"] == "open"
        assert fake_store.get("mem:knowledge:01OTHER")["licence"] == "unknown"

    def test_batch_facts_follow_by_doc_id(self, fake_store, fake_embedder):
        from tools.licence import set_licence
        store_memory(fake_store, fake_embedder, "mem:episodic:01C1", "chunk 1")
        fake_store.set_field("mem:episodic:01C1", "doc_id", "DOC1")
        store_memory(fake_store, fake_embedder, "mem:knowledge:01F", "fact", namespace="knowledge")
        # batch enrichment names the document, not any one chunk
        fake_store.set_fields("mem:knowledge:01F", {"enriched_from": "mem:episodic:01C0", "source_doc_id": "DOC1"})
        set_licence("restricted", keys=["mem:episodic:01C1"])
        assert fake_store.get("mem:knowledge:01F")["licence"] == "restricted"

    def test_does_not_bump_updated_at(self, fake_store, fake_embedder):
        from tools.licence import set_licence
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "a")
        before = fake_store.get("mem:episodic:01A")["updated_at"]
        set_licence("open", keys=["mem:episodic:01A"])
        assert fake_store.get("mem:episodic:01A")["updated_at"] == before

    def test_meta_keys_are_not_classifiable(self):
        from tools.licence import set_licence
        assert "No valid memory keys" in set_licence("open", keys=["meta:feed:influence"])["error"]

    def test_classify_memories_nothing_found(self, fake_store):
        from memory.licence import classify_memories
        out = classify_memories(fake_store, ["mem:episodic:GONE"], "open")
        assert out == {"classified": [], "cascaded": [], "not_found": ["mem:episodic:GONE"]}


class TestEmptyStringLicence:
    def test_remember_treats_empty_as_default(self, fake_store):
        from tools.core import remember
        r = remember("A decision", mode="raw", licence="")
        assert r["licence"] == "own"

    def test_recent_knowledge_treats_empty_as_no_filter(self, fake_store, fake_embedder):
        from tools.knowledge import recent_knowledge
        store_memory(fake_store, fake_embedder, "mem:knowledge:01U", "x", namespace="knowledge")
        assert len(recent_knowledge(licence="")) == 1


class TestMigrationImportedMemories:
    def test_imported_memories_are_unknown_not_own(self, fake_store):
        _put(fake_store, "mem:episodic:01IMP", imported_at="1.0")
        _put(fake_store, "mem:episodic:01OWN")
        migrate_licence(fake_store)
        assert fake_store.get("mem:episodic:01IMP")["licence"] == "unknown"
        assert fake_store.get("mem:episodic:01OWN")["licence"] == "own"


class TestRestoreBackfills:
    def test_restore_from_file_backfills(self, fake_store, tmp_path, monkeypatch):
        import json
        from tools import backup as backup_tool
        monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
        dump = {"version": "6.6.0", "created_at": 1.0, "metadata": {},
                "data": {"mem:knowledge:art": {"content": "x", "state": "active",
                                               "feed_name": "F", "created_at": "1.0",
                                               "updated_at": "1.0"}}}
        (tmp_path / "old.json").write_text(json.dumps(dump))
        result = backup_tool.restore_from_file("old.json", dry_run=False)
        assert result.get("status") != "error", result
        assert fake_store.get("mem:knowledge:art")["licence"] == "unknown"


class TestMirrorValidation:
    def test_unrecognised_licence_not_mirrored(self, fake_store, caplog):
        from memory.feed_influence import load_feed_influences, sync_feed_influences
        sync_feed_influences(fake_store.client, [
            {"name": "A", "url": "https://a", "licence": "cc-by-4"},
            {"name": "B", "url": "https://b", "licence": False},
            {"name": "C", "url": "https://c", "licence": "ogl", "licence_note": "  checked  gov.uk "},
        ])
        mirrored = load_feed_influences(fake_store.client)
        assert "licence" not in mirrored["A"]
        assert "licence" not in mirrored["B"]
        assert mirrored["C"]["licence"] == "ogl"
        assert mirrored["C"]["licence_note"] == "checked gov.uk"
        assert "unrecognised licence 'cc-by-4' not mirrored" in caplog.text

    def test_note_travels_in_bundles(self, fake_store, fake_embedder):
        from memory.feed_influence import sync_feed_influences
        from memory.skill_transfer import build_skill_export, validate_skill_import
        from tests.test_skill_transfer_feeds import _seed_skill
        key = _seed_skill(fake_store, fake_embedder)
        sync_feed_influences(fake_store.client, [
            {"name": "Gov", "url": "https://gov.example/feed", "licence": "restricted",
             "licence_note": "Vendor EULA s.4", "skills": {"python": 4}},
        ])
        bundle, _ = build_skill_export(fake_store, key)
        result = validate_skill_import(bundle["data"])
        assert result["ok"], result
        assert result["feeds"][0]["licence_note"] == "Vendor EULA s.4"

    @pytest.mark.parametrize("note", [42, "x" * 201])
    def test_bad_note_rejected(self, fake_store, fake_embedder, note):
        from memory.skill_transfer import validate_skill_import
        from tests.test_skill_transfer_feeds import _export, _patch_bundle
        bundle = _export(fake_store, fake_embedder)
        feeds = [{"name": "F", "url": "https://x.example", "skills": {"python": 5},
                  "licence": "open", "licence_note": note}]
        result = validate_skill_import(_patch_bundle(bundle["data"], feeds=feeds))
        assert not result["ok"]
        assert "invalid licence note" in result["error"]

    def test_empty_note_dropped(self, fake_store, fake_embedder):
        from memory.skill_transfer import validate_skill_import
        from tests.test_skill_transfer_feeds import _export, _patch_bundle
        bundle = _export(fake_store, fake_embedder)
        feeds = [{"name": "F", "url": "https://x.example", "skills": {"python": 5},
                  "licence": "open", "licence_note": ""}]
        result = validate_skill_import(_patch_bundle(bundle["data"], feeds=feeds))
        assert result["ok"]
        assert "licence_note" not in result["feeds"][0]


class TestDocumentLevelCascade:
    def test_classifying_one_chunk_reaches_siblings_and_facts(self, fake_store):
        for i in range(3):
            _put(fake_store, f"mem:episodic:0{i}", doc_id="DOC", licence="own")
        _put(fake_store, "mem:episodic:other", licence="own")
        _put(fake_store, "mem:knowledge:01F", enriched_from="mem:episodic:00", source_doc_id="DOC")
        from tools.licence import set_licence
        result = set_licence("restricted", keys=["mem:episodic:01"])
        assert sorted(result["keys"]) == ["mem:episodic:00", "mem:episodic:01", "mem:episodic:02"]
        assert result["cascaded_facts"] == ["mem:knowledge:01F"]
        for k in ("mem:episodic:00", "mem:episodic:01", "mem:episodic:02", "mem:knowledge:01F"):
            assert fake_store.get(k)["licence"] == "restricted"
        assert fake_store.get("mem:episodic:other")["licence"] == "own"

    def test_knowledge_only_call_does_not_scan_for_facts(self, fake_store, monkeypatch):
        _put(fake_store, "mem:knowledge:art", feed_name="F")
        calls = []
        original = fake_store.scan_prefix
        monkeypatch.setattr(fake_store, "scan_prefix", lambda p: calls.append(p) or original(p))
        from memory.lineage import stamp_lineage
        out = stamp_lineage(fake_store, ["mem:knowledge:art"], {"licence": "open"})
        assert out["classified"] == ["mem:knowledge:art"] and out["cascaded"] == []
        assert calls == []

    def test_feed_path_goes_through_the_engine(self, fake_store, fake_embedder):
        from tools.licence import set_licence
        _put(fake_store, "mem:knowledge:art", feed_name="F", licence="unknown",
             licence_note="stale")
        result = set_licence("ogl", feed_name="F")
        assert result["classified"] == 1 and result["licence_note"] == "OGL v3.0"
        assert fake_store.get("mem:knowledge:art")["licence_note"] == "OGL v3.0"
        set_licence("restricted", feed_name="F")
        assert not fake_store.get("mem:knowledge:art").get("licence_note")


class TestEnrichmentPayloadClassification:
    def _facts(self, monkeypatch):
        from memory import enrichment
        monkeypatch.setattr(enrichment, "extract_facts",
                            lambda content: [ExtractedFact(text="A fact", kind="fact")])

    def test_batch_facts_take_declared_classification_when_first_chunk_is_gone(
            self, monkeypatch, fake_store, fake_embedder):
        self._facts(monkeypatch)
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": "mem:episodic:GONE", "namespace": "episodic",
            "batch_mode": True, "batch_content": "text", "created_at": "1.0",
            "classification": {"licence": "restricted", "licence_note": "EULA",
                               "provenance": "retrieved"},
        })
        facts = [fake_store.get(k) for k in fake_store.scan_prefix("mem:knowledge:")]
        assert facts and facts[0]["licence"] == "restricted"
        assert facts[0]["licence_note"] == "EULA"
        assert facts[0]["provenance"] == "retrieved"

    def test_live_record_beats_payload_snapshot(self, monkeypatch, fake_store, fake_embedder):
        """A reclassification between enqueue and processing is inherited;
        the payload only fills in what the record lacks."""
        self._facts(monkeypatch)
        store_memory(fake_store, fake_embedder, "mem:episodic:01SRC", "content")
        fake_store.set_fields("mem:episodic:01SRC", {"licence": "restricted"})
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": "mem:episodic:01SRC", "namespace": "episodic",
            "classification": {"licence": "own", "provenance": "asserted"},
        })
        facts = [fake_store.get(k) for k in fake_store.scan_prefix("mem:knowledge:")]
        assert facts[0]["licence"] == "restricted"   # live record
        assert facts[0]["provenance"] == "asserted"  # payload filled the gap

    def test_remember_document_queues_its_classification(self, fake_store, monkeypatch):
        import json
        from tools.core import remember_document
        monkeypatch.setenv("ENRICHMENT_BATCH_MODE", "true")
        monkeypatch.setattr("tools.core.check_duplicate", lambda *a, **k: None)
        remember_document("Para one.\n\nPara two.", mode="full", licence="cc-by-4.0",
                          provenance="retrieved")
        payload = json.loads(fake_store.client._data["queue:enrich"]["_list"][0])
        assert payload["classification"] == {
            "licence": "open", "licence_note": "CC BY 4.0", "provenance": "retrieved",
        }

    def test_remember_queues_its_classification(self, fake_store, monkeypatch):
        import json
        from tools.core import remember
        remember("Something worth a fact", mode="full", provenance="asserted")
        payload = json.loads(fake_store.client._data["queue:enrich"]["_list"][0])
        assert payload["classification"] == {"licence": "own", "provenance": "asserted"}


class TestNoteForReclassification:
    @pytest.mark.parametrize("old_class,old_note,new_class,submitted,expected", [
        ("open", "CC BY 4.0", "restricted", "CC BY 4.0", None),      # pre-filled, class changed
        ("open", "CC BY 4.0", "restricted", "Paywalled", "Paywalled"),  # typed
        ("open", "CC BY 4.0", "open", "CC BY 4.0", "CC BY 4.0"),     # unchanged
        ("", None, "open", "checked", "checked"),                   # previously unset
        ("open", "CC BY 4.0", "restricted", None, None),
    ])
    def test_rule(self, old_class, old_note, new_class, submitted, expected):
        assert lic.note_for_reclassification(old_class, old_note, new_class, submitted) == expected
