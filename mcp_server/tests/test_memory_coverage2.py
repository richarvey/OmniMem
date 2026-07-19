"""Second targeted coverage pass for memory-package branches the main suites
skip: variant-search parse fallbacks, maintenance caps and error recovery,
skill bundle rejection paths, and reindex info failures."""

import io
import json
import time
import zipfile

import numpy as np
import pytest
import valkey

from tests.conftest import store_memory
from tests.test_skill_transfer import _rebuild_zip, _seed_skill
from tests.test_store import ExtClient, ExtSearchIndex

from memory import enrichment, maintenance, query_expansion, skill_scan, temporal
from memory import recall as recall_mod
from memory.enrichment import EnrichmentWorker
from memory.maintenance import expire_knowledge_items, run_maintenance
from memory.skill_compiler import safe_export_path
from memory.skill_scan import run_skill_scan
from memory.skill_transfer import (
    _read_entry,
    build_skill_export,
    validate_skill_import,
)
from memory.skills import (
    gather_promoted_knowledge,
    known_domains,
    summarise_rule_changes,
)
from memory.store import ValkeyStore


# ---------------------------------------------------------------------------
# recall.py — main-loop parse fallback, query expansion, variant search
# ---------------------------------------------------------------------------


class TestRecallEdges:
    def test_main_loop_ignores_unparseable_event_date(
        self, fake_store, fake_embedder, pipeline
    ):
        key = store_memory(
            fake_store, fake_embedder, "mem:episodic:01EVD",
            "deployment pipeline notes",
        )
        fake_store.set_field(key, "event_date", "not-a-number")

        results = pipeline.recall("deployment pipeline notes")
        hit = next(r for r in results if r.key == key)
        assert hit.event_date is None

    def test_expand_query_failure_falls_back_to_original(
        self, fake_store, fake_embedder, pipeline, monkeypatch
    ):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01EXP",
            "valkey connection pooling",
        )

        def boom(query, store=None):
            raise RuntimeError("expansion service down")

        monkeypatch.setattr(recall_mod, "expand_query", boom)
        results = pipeline.recall(
            "valkey connection pooling", expand_queries=True
        )
        assert any(r.key == "mem:episodic:01EXP" for r in results)

    def test_variant_search_filters_and_parse_fallbacks(
        self, fake_store, fake_embedder, pipeline, lifecycle, monkeypatch
    ):
        """One expanded recall exercising every skip/fallback branch in
        _search_variant: archived docs, suppressed topics, project mismatch,
        recency decay, and unparseable event_date/tags/effort/contradictions.
        Empty and identical variants must also be skipped."""
        now = time.time()
        query = "caching strategy notes"

        store_memory(
            fake_store, fake_embedder, "mem:episodic:01ARC",
            "caching strategy from the old stack", state="archived",
            project="proj",
        )
        lifecycle.suppress_topic("kubernetes")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01SUP",
            "caching strategy on kubernetes", project="proj",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01OTH",
            "caching strategy elsewhere", project="other",
        )
        messy = store_memory(
            fake_store, fake_embedder, "mem:episodic:01MES",
            "caching strategy with messy metadata", project="proj",
        )
        fake_store.set_field(messy, "created_at", str(now - 200 * 86400))
        fake_store.set_field(messy, "event_date", "garbage")
        fake_store.set_field(messy, "tags", "alpha, beta")
        fake_store.set_field(messy, "effort_score", "bad")
        fake_store.set_field(messy, "contradictions", "{not json")
        good = store_memory(
            fake_store, fake_embedder, "mem:episodic:01GOO",
            "caching strategy that worked", project="proj",
            effort_score=3,
        )

        monkeypatch.setattr(
            recall_mod, "expand_query",
            lambda q, store=None: ["", query, "cache approach ideas"],
        )
        results = pipeline.recall(
            query, project_filter="proj", expand_queries=True, top_k=10
        )
        keys = {r.key for r in results}
        assert messy in keys and good in keys
        assert "mem:episodic:01ARC" not in keys
        assert "mem:episodic:01SUP" not in keys
        assert "mem:episodic:01OTH" not in keys

        messy_hit = next(r for r in results if r.key == messy)
        assert messy_hit.event_date is None
        assert messy_hit.tags == ["alpha", "beta"]
        assert messy_hit.effort_score is None
        assert messy_hit.contradictions == []
        good_hit = next(r for r in results if r.key == good)
        assert good_hit.effort_score == 3

    def test_abandoned_entry_without_name_is_skipped(
        self, fake_store, fake_embedder, pipeline
    ):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01ABN",
            "tried a few approaches",
            abandoned_approaches=[
                {"name": "", "reason": "unnamed entry"},
                {"name": "graphql", "reason": "too much schema churn"},
            ],
        )
        matches = pipeline.warn_if_abandoned("graphql rewrite")
        assert [m["abandoned_name"] for m in matches] == ["graphql"]

    def test_duplicate_abandoned_names_deduped(
        self, fake_store, fake_embedder, pipeline
    ):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01DUP",
            "logged the same dead end twice",
            abandoned_approaches=[
                {"name": "graphql", "reason": "first note"},
                {"name": "graphql", "reason": "second note"},
            ],
        )
        matches = pipeline.warn_if_abandoned("graphql rewrite")
        assert len(matches) == 1


# ---------------------------------------------------------------------------
# maintenance.py + dedup.py — missing rows, caps, error recovery
# ---------------------------------------------------------------------------


_UNIT_VEC = np.ones(384, dtype=np.float32) / np.sqrt(np.float32(384))


class TestMaintenanceEdges:
    def test_missing_rows_skipped_in_every_phase(
        self, fake_store, fake_embedder, lifecycle, monkeypatch
    ):
        """A row that vanishes between SCAN and HMGET reads back as None —
        dedup, the contradiction scan, and knowledge expiry must all skip it."""
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01GON",
            "will read back as missing", project="proj",
        )
        know = store_memory(
            fake_store, fake_embedder, "mem:knowledge:01GON",
            "expired article", namespace="knowledge",
        )
        fake_store.set_fields(know, {
            "feed_name": "Feed", "expires_at": str(time.time() - 10),
        })
        monkeypatch.setattr(
            fake_store, "get_fields_multi",
            lambda keys, fields: [None for _ in keys],
        )

        result = run_maintenance(fake_store, fake_embedder, lifecycle, "proj")
        assert result["duplicates_archived"] == []
        assert result["contradictions_found"] == []
        assert result["knowledge_expired"] == []

    def test_expire_knowledge_skips_missing_row(
        self, fake_store, fake_embedder, lifecycle, monkeypatch
    ):
        store_memory(
            fake_store, fake_embedder, "mem:knowledge:01MIS",
            "article", namespace="knowledge",
        )
        monkeypatch.setattr(
            fake_store, "get_fields_multi",
            lambda keys, fields: [None for _ in keys],
        )
        assert expire_knowledge_items(fake_store, lifecycle) == []

    def test_contradiction_scan_key_cap(
        self, fake_store, fake_embedder, lifecycle, monkeypatch
    ):
        monkeypatch.setattr(maintenance, "find_all_duplicates",
                            lambda *a, **kw: [])
        monkeypatch.setattr(maintenance, "_CONTRADICTION_SCAN_CAP", 1)
        for i in range(2):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:01CAP{i}",
                f"memory number {i}", project="proj",
            )
        result = run_maintenance(fake_store, fake_embedder, lifecycle, "proj")
        # Capped at one active entry, so the pairwise scan never runs.
        assert result["contradictions_found"] == []

    def test_contradiction_results_cap_breaks_both_loops(
        self, fake_store, fake_embedder, lifecycle, monkeypatch
    ):
        monkeypatch.setattr(maintenance, "find_all_duplicates",
                            lambda *a, **kw: [])
        monkeypatch.setattr(maintenance, "_CONTRADICTION_RESULTS_CAP", 1)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01NEG",
            "never use tabs for indentation", project="proj",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01POS",
            "always use tabs for indentation", project="proj",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01THR",
            "third unrelated memory", project="proj",
        )
        # Identical stored vectors so every pair clears the similarity gate.
        monkeypatch.setattr(
            fake_store, "get_vectors_multi",
            lambda keys: [_UNIT_VEC for _ in keys],
        )
        result = run_maintenance(fake_store, fake_embedder, lifecycle, "proj")
        assert len(result["contradictions_found"]) == 1

    def test_contradiction_comparison_cap_breaks_inner_loop(
        self, fake_store, fake_embedder, lifecycle, monkeypatch
    ):
        monkeypatch.setattr(maintenance, "find_all_duplicates",
                            lambda *a, **kw: [])
        monkeypatch.setattr(maintenance, "_CONTRADICTION_COMPARISON_CAP", 1)
        for i in range(3):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:01CMP{i}",
                f"neutral note about caching {i}", project="proj",
            )
        monkeypatch.setattr(
            fake_store, "get_vectors_multi",
            lambda keys: [_UNIT_VEC for _ in keys],
        )
        result = run_maintenance(fake_store, fake_embedder, lifecycle, "proj")
        assert result["contradictions_found"] == []

    def test_contradiction_phase_error_is_logged_not_raised(
        self, fake_store, fake_embedder, lifecycle, monkeypatch
    ):
        monkeypatch.setattr(maintenance, "find_all_duplicates",
                            lambda *a, **kw: [])
        for i in range(2):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:01ERR{i}",
                f"memory {i}", project="proj",
            )

        def boom(keys):
            raise RuntimeError("vectors unavailable")

        monkeypatch.setattr(fake_store, "get_vectors_multi", boom)
        result = run_maintenance(fake_store, fake_embedder, lifecycle, "proj")
        assert result["contradictions_found"] == []


# ---------------------------------------------------------------------------
# enrichment.py — empty content and no-fact early returns
# ---------------------------------------------------------------------------


class TestEnrichmentEdges:
    def test_enrich_returns_on_empty_content(self, fake_store, fake_embedder):
        key = store_memory(
            fake_store, fake_embedder, "mem:episodic:01EMP", "",
        )
        worker = EnrichmentWorker(fake_store, fake_embedder)
        worker._enrich({"key": key})
        assert fake_store.scan_prefix("mem:knowledge:") == []

    def test_enrich_returns_when_no_facts_extracted(
        self, fake_store, fake_embedder, monkeypatch
    ):
        key = store_memory(
            fake_store, fake_embedder, "mem:episodic:01NOF",
            "nothing worth extracting here",
        )
        monkeypatch.setattr(enrichment, "extract_facts", lambda content: [])
        worker = EnrichmentWorker(fake_store, fake_embedder)
        worker._enrich({"key": key})
        assert fake_store.scan_prefix("mem:knowledge:") == []


# ---------------------------------------------------------------------------
# query_expansion.py — bytes round-trip from a decode_responses=False client
# ---------------------------------------------------------------------------


class TestQueryExpansionCache:
    def test_read_cache_decodes_bytes_values(self):
        class _BytesClient:
            def hgetall(self, key):
                return {b"variants": b'["alternative phrasing"]',
                        b"ts": b"123"}

        class _Store:
            client = _BytesClient()

        assert query_expansion._read_cache(_Store(), "qexp:x") == [
            "alternative phrasing"
        ]


# ---------------------------------------------------------------------------
# temporal.py — whole-string date parse fast path
# ---------------------------------------------------------------------------


class TestTemporalWholeStringParse:
    def test_query_that_is_entirely_a_date_parses_directly(self):
        # A bare date needs no search_dates fallback — dateparser handles
        # the whole string in one go.
        parsed = temporal.parse_query_date("15 March 2026")
        assert parsed is not None
        assert (parsed.year, parsed.month, parsed.day) == (2026, 3, 15)


# ---------------------------------------------------------------------------
# skill_compiler.py — export path edge cases
# ---------------------------------------------------------------------------


class TestSafeExportPath:
    def test_backslash_only_path_is_empty(self):
        filepath, err = safe_export_path("\\")
        assert filepath is None
        assert err == "export_path cannot be empty"

    def test_symlink_escape_is_refused(self, tmp_path, monkeypatch):
        exports = tmp_path / "exports"
        exports.mkdir()
        (exports / "link").symlink_to(tmp_path)
        monkeypatch.setenv("SKILL_EXPORT_DIR", str(exports))

        filepath, err = safe_export_path("link/SKILL.md")
        assert filepath is None
        assert err == "export_path must not escape the export directory"


# ---------------------------------------------------------------------------
# skill_scan.py — candidate cap
# ---------------------------------------------------------------------------


class TestSkillScanCandidateCap:
    def test_candidate_list_capped(
        self, fake_store, fake_embedder, monkeypatch
    ):
        monkeypatch.setattr(skill_scan, "_MAX_CANDIDATES", 1)
        for i in range(2):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:01DOC{i}",
                f"docker lesson {i}", tags=["docker"],
            )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01PYT",
            "python lesson", tags=["python"],
        )
        result = run_skill_scan(fake_store, fake_embedder)
        # Only the largest pool (docker) was considered; no lessons, so
        # nothing was proposed — the cap break is what this exercises.
        assert result["proposals"] == []


# ---------------------------------------------------------------------------
# skills.py — missing rows, dont-rule matching, archived tags
# ---------------------------------------------------------------------------


class TestSkillsEdges:
    def test_gather_promoted_knowledge_skips_missing_row(
        self, fake_store, fake_embedder, monkeypatch
    ):
        store_memory(
            fake_store, fake_embedder, "mem:knowledge:01PRO",
            "promoted article", namespace="knowledge",
        )
        monkeypatch.setattr(
            fake_store, "get_fields_multi",
            lambda keys, fields: [None for _ in keys],
        )
        assert gather_promoted_knowledge(fake_store, ["python"]) == {
            "python": []
        }

    def test_summarise_dont_rules_skip_already_matched_old(self):
        old = [
            {"kind": "dont", "name": "alpha", "text": "Avoid alpha",
             "sources": ["mem:episodic:01A"]},
            {"kind": "dont", "name": "beta", "text": "Avoid beta",
             "sources": ["mem:episodic:01B"]},
        ]
        new = [dict(r) for r in old]
        # Both rules identical: the second new rule must skip the already
        # matched first old rule and pair with the second — no changes.
        assert summarise_rule_changes(old, new) == []

    def test_known_domains_skips_archived_memories(
        self, fake_store, fake_embedder
    ):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01ACT",
            "active lesson", tags=["python"],
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01OLD",
            "archived lesson", tags=["docker"], state="archived",
        )
        counts = known_domains(fake_store)
        assert counts.get("python") == 1
        assert "docker" not in counts


# ---------------------------------------------------------------------------
# store.py — reindex when FT.INFO is unavailable
# ---------------------------------------------------------------------------


class _InfoFailIndex(ExtSearchIndex):
    def info(self):
        raise valkey.ResponseError("info unavailable")


class _InfoFailClient(ExtClient):
    def ft(self, index_name):
        return _InfoFailIndex(self, index_name)


class TestReindexInfoFailures:
    def test_reindex_falls_back_when_info_unavailable(self):
        store = ValkeyStore()
        client = _InfoFailClient()
        store._client = client
        store._raw_client = client
        client.hset("mem:episodic:01A", mapping={"content": "x"})

        result = store.reindex_namespace("episodic")
        assert result["before_num_docs"] == 0
        assert result["actual_records"] == 1
        # FT.INFO failed after recreate too — actual count stands in.
        assert result["after_num_docs"] == 1
        assert result["removed_phantoms"] == 0


# ---------------------------------------------------------------------------
# skill_transfer.py — bundle rejection paths
# ---------------------------------------------------------------------------


@pytest.fixture
def stored_bundle(fake_store, fake_embedder):
    """A valid bundle repacked with ZIP_STORED so bytes-level corruption of
    one entry is possible without touching the others."""
    _seed_skill(fake_store, fake_embedder)
    out, err = build_skill_export(fake_store, "mem:skill:gen:python-local")
    assert err is None
    return _rebuild_zip(out["data"])


class TestSkillTransferRejections:
    def test_read_entry_returns_none_for_missing_name(self):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as zf:
            zf.writestr("skill.json", b"{}")
        with zipfile.ZipFile(io.BytesIO(buf.getvalue())) as zf:
            assert _read_entry(zf, "not-there.json") is None

    def test_encrypted_entry_rejected(self):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as zf:
            zf.writestr("skill.json", b"{}")
        data = bytearray(buf.getvalue())
        # Set the encrypted bit in the central directory's general-purpose
        # flags — writestr strips it, so patch the bytes directly.
        idx = bytes(data).find(b"PK\x01\x02")
        data[idx + 8] |= 0x1
        result = validate_skill_import(bytes(data))
        assert result == {
            "ok": False, "error": "Encrypted zip entries are not supported",
        }

    def test_bundle_expanding_too_large_rejected(self):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
            # 51 entries of exactly 1 MB: each passes the per-entry cap,
            # the 51 MB total breaches the 50 MB expansion bound.
            for i in range(51):
                zf.writestr(f"m{i:02d}", b"\x00" * (1024 * 1024))
        result = validate_skill_import(buf.getvalue())
        assert result == {"ok": False, "error": "Bundle expands too large"}

    def test_missing_skill_md_rejected(self):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as zf:
            zf.writestr("manifest.json", b"{}")
            zf.writestr("skill.json", b"{}")
        result = validate_skill_import(buf.getvalue())
        assert result == {
            "ok": False, "error": "Bundle is missing skill.json or SKILL.md",
        }

    def test_unreadable_manifest_rejected(self, stored_bundle):
        # Flip one content byte of the stored manifest entry so its CRC no
        # longer matches and zf.read() raises.
        corrupt = stored_bundle.replace(
            b'"omnimem-skill-export"', b'"omnimem-skill-exporX"'
        )
        assert corrupt != stored_bundle
        result = validate_skill_import(corrupt)
        assert result == {
            "ok": False, "error": "Could not read manifest.json",
        }

    def test_unreadable_payload_rejected(self, stored_bundle):
        # Same corruption trick, but on skill.json — the checksum pass
        # cannot even read the entry back.
        corrupt = stored_bundle.replace(
            b"Distilled python procedure", b"Xistilled python procedure"
        )
        assert corrupt != stored_bundle
        result = validate_skill_import(corrupt)
        assert result == {"ok": False, "error": "Could not read skill.json"}

    def test_oversized_body_rejected(self, stored_bundle):
        with zipfile.ZipFile(io.BytesIO(stored_bundle)) as zf:
            skill_fields = json.loads(zf.read("skill.json"))
        skill_fields["body"] = "x" * 100_001
        tampered = _rebuild_zip(
            stored_bundle,
            replace={
                "skill.json": json.dumps(skill_fields).encode(),
                "SKILL.md": skill_fields["body"].encode(),
            },
            refresh_checksums=True,
        )
        result = validate_skill_import(tampered)
        assert result == {
            "ok": False, "error": "Skill body exceeds 100000 chars",
        }

    def test_too_many_memories_rejected(self, stored_bundle):
        extra = {
            f"memories/{i:04d}.json": b"{}" for i in range(3, 501)
        }
        tampered = _rebuild_zip(
            stored_bundle, add=extra, refresh_checksums=True
        )
        result = validate_skill_import(tampered)
        assert result == {
            "ok": False,
            "error": "Bundle carries too many memories (max 500)",
        }
