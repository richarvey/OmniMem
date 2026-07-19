"""Targeted coverage for memory/skills.py parser, gate, and guard branches."""

import json

import pytest

from tests.conftest import store_memory

from memory import skills as sk


class TestSmallHelpers:
    def test_safe_float_fallback(self):
        assert sk._safe_float("not a number", 2.5) == 2.5
        assert sk._safe_float(None) == 0.0
        assert sk._safe_float("3") == 3.0

    def test_parse_tags_comma_fallback_and_junk(self):
        assert sk._parse_tags("python, docker , ") == ["python", "docker"]
        assert sk._parse_tags('["a", "b"]') == ["a", "b"]
        assert sk._parse_tags('"a string"') == []
        assert sk._parse_tags(None) == []

    def test_one_line_truncates(self):
        collapsed = sk._one_line("word " * 200)
        assert len(collapsed) <= 400
        assert collapsed.endswith("…")


class TestPoolGathering:
    def test_empty_domain_list(self, fake_store):
        assert sk.gather_domain_pools(fake_store, []) == {}
        assert sk.gather_promoted_knowledge(fake_store, []) == {}

    def test_scan_cap_warning(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setattr(sk, "_MAX_POOL_KEYS", 1)
        for i in range(3):
            store_memory(fake_store, fake_embedder, f"mem:episodic:01{i}",
                         f"m {i}", tags=["python"])
        pools = sk.gather_domain_pools(fake_store, ["python"])
        assert len(pools["python"]) == 1

    def test_row_damage_tolerated(self, fake_store, fake_embedder):
        # Row with none of the projected fields
        fake_store.client.hset("mem:episodic:01A", mapping={"other": "x"})
        # Archived row
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "gone",
                     tags=["python"], state="archived")
        # Corrupt abandoned_approaches JSON and non-list JSON
        store_memory(fake_store, fake_embedder, "mem:episodic:01C", "ok",
                     tags=["python"])
        fake_store.set_fields("mem:episodic:01C", {
            "abandoned_approaches": "{broken", "effort_score": "loads",
        })
        store_memory(fake_store, fake_embedder, "mem:episodic:01D", "ok too",
                     tags=["python"])
        fake_store.set_fields("mem:episodic:01D", {
            "abandoned_approaches": '{"not": "a list"}',
        })

        pool = sk.gather_domain_pools(fake_store, ["python"])["python"]
        keys = {m["key"] for m in pool}
        assert keys == {"mem:episodic:01C", "mem:episodic:01D"}
        by_key = {m["key"]: m for m in pool}
        assert by_key["mem:episodic:01C"]["abandoned"] == []
        assert by_key["mem:episodic:01C"]["effort_score"] is None

    def test_promoted_scan_cap_and_state_filter(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        monkeypatch.setattr(sk, "_MAX_POOL_KEYS", 2)
        for i, state in enumerate(("active", "archived", "active")):
            fake_store.upsert("knowledge", f"mem:knowledge:01{i}", {
                "content": f"article {i}", "state": state,
                "skill_domains": json.dumps(["python"]),
            }, fake_embedder.embed(f"a{i}"))
        pools = sk.gather_promoted_knowledge(fake_store, ["python"])
        assert len(pools["python"]) == 1  # capped to 2 keys, one archived


class TestReferenceRules:
    def test_validate_rejects_each_shape(self):
        with pytest.raises(ValueError, match="must be a list"):
            sk.validate_reference_rules({"kind": "do"})
        with pytest.raises(ValueError, match="max"):
            sk.validate_reference_rules(
                [{"kind": "do", "text": "x"}] * (sk._MAX_REFERENCE_RULES + 1),
            )
        with pytest.raises(ValueError, match="objects"):
            sk.validate_reference_rules(["not a dict"])
        with pytest.raises(ValueError, match="kind must be one of"):
            sk.validate_reference_rules([{"kind": "maybe", "text": "x"}])
        with pytest.raises(ValueError, match="non-empty text"):
            sk.validate_reference_rules([{"kind": "do", "text": "  "}])
        with pytest.raises(ValueError, match="max 400"):
            sk.validate_reference_rules([{"kind": "do", "text": "x" * 401}])

    def test_parse_reference_rules_damage(self):
        assert sk.parse_reference_rules("{broken") == []
        assert sk.parse_reference_rules('[{"kind": "wat", "text": "x"}]') == []
        assert sk.parse_reference_rules(None) == []
        assert sk.parse_reference_rules(
            '[{"kind": "do", "text": "x"}]',
        ) == [{"kind": "do", "text": "x"}]


class TestLessonExtraction:
    def test_lesson_bearing_blessed_content_only(self):
        assert sk.lesson_bearing({
            "blessed": True, "content": "just content", "abandoned": [],
        }) is True
        assert sk.lesson_bearing({
            "blessed": True, "content": "", "abandoned": [],
        }) is False

    def test_abandoned_without_name_skipped(self):
        lessons = sk.extract_lessons([{
            "key": "mem:episodic:01A", "content": "c", "blessed": False,
            "updated_at": 1.0, "project": None,
            "breakthrough": None, "gotchas": None, "outcome": None,
            "abandoned": [{"reason": "nameless"}, {"name": "bad idea",
                                                   "reason": "why"}],
        }])
        assert len(lessons) == 1
        assert lessons[0].name == "bad idea"

    def test_blessed_content_fallback_lesson(self):
        lessons = sk.extract_lessons([{
            "key": "mem:episodic:01A", "content": "the whole memory",
            "blessed": True, "updated_at": 1.0, "project": "p",
            "breakthrough": None, "gotchas": None, "outcome": None,
            "abandoned": [],
        }])
        assert len(lessons) == 1
        assert lessons[0].kind == "do"
        assert lessons[0].blessed is True

    def test_cluster_indices_trivial_sizes(self):
        assert sk._cluster_indices([], 0.8) == []


class TestReferenceRuleRendering:
    def test_empty_article_contributes_nothing(self):
        rules = sk.build_reference_rules([{
            "key": "mem:knowledge:01A", "title": "", "content": "   ",
            "source_url": "", "skill_rules": [],
        }])
        assert rules == []


class TestSummariseChanges:
    def test_text_fallback_match_counts_as_reinforced(self):
        old = [{"kind": "do", "text": "pin builders",
                "sources": ["mem:episodic:01A"], "reinforcement": 1}]
        new = [{"kind": "do", "text": "pin builders",
                "sources": ["mem:episodic:01B"], "reinforcement": 1}]
        changes = sk.summarise_rule_changes(old, new)
        assert [c["change"] for c in changes] == ["reinforced"]

    def test_removed_rule_flagged_high(self):
        old = [{"kind": "watch", "text": "beware the cache",
                "sources": ["mem:episodic:01A"], "reinforcement": 1}]
        changes = sk.summarise_rule_changes(old, [])
        assert changes == [{
            "change": "removed", "risk": "high", "rule_kind": "watch",
            "rule": "beware the cache",
        }]


class TestKnownDomainsAndSuggest:
    def test_known_domains_cap_and_skill_counting(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        monkeypatch.setattr(sk, "_MAX_POOL_KEYS", 1)
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "m",
                     tags=["python"])
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "m",
                     tags=["rust"])
        fake_store.upsert("skill", "mem:skill:gen:go-local", {
            "domain": "go", "state": "active",
        }, fake_embedder.embed("go"))
        counts = sk.known_domains(fake_store)
        assert counts["go"] == 1
        assert len([d for d in counts if d in ("python", "rust")]) == 1

    def test_suggest_similar_domain_embedding_paths(self, fake_embedder, monkeypatch):
        # Dissimilar candidates below the threshold → no suggestion.
        assert sk.suggest_similar_domain(
            fake_embedder, "kubernetes-ops", ["quarterly-finance"],
        ) is None
        # Threshold floor of -1 always suggests the best match.
        monkeypatch.setenv("SKILL_DOMAIN_SUGGEST_THRESHOLD", "-1")
        suggestion = sk.suggest_similar_domain(
            fake_embedder, "kubernetes-ops", ["quarterly-finance"],
        )
        assert suggestion is not None and suggestion[0] == "quarterly-finance"

    def test_suggest_similar_domain_empty_pool(self, fake_embedder):
        assert sk.suggest_similar_domain(fake_embedder, "python", []) is None
        assert sk.suggest_similar_domain(
            fake_embedder, "python", ["python"],
        ) is None
