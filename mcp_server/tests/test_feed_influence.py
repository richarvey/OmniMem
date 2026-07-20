"""Tests for feed→skill influence: the config mirror, the compiler's
feed-knowledge gathering, feed rule rendering, and the compile flow."""

import json
import time

import pytest

from memory.feed_influence import (
    FEED_INFLUENCE_KEY,
    feeds_for_domain,
    load_feed_influences,
    normalise_feed_entry,
    sync_feed_influences,
    validate_feed_skills,
)
from memory.skill_compiler import compile_skill_flow
from memory.skills import (
    Rule,
    bodies_equivalent,
    build_feed_rules,
    gather_feed_knowledge,
    render_skill_md,
    summarise_rule_changes,
)
from tests.conftest import store_memory


class TestValidateFeedSkills:
    def test_valid_mapping(self):
        assert validate_feed_skills({"python": 8, "docker": "3"}) == {
            "python": 8, "docker": 3,
        }

    def test_alias_resolution(self):
        assert validate_feed_skills({"py": 7}) == {"python": 7}

    def test_not_a_dict(self):
        with pytest.raises(ValueError, match="mapping"):
            validate_feed_skills(["python"])

    def test_invalid_domain(self):
        with pytest.raises(ValueError, match="Invalid domain"):
            validate_feed_skills({"bad domain!!": 5})

    def test_non_numeric_score(self):
        with pytest.raises(ValueError, match="whole number"):
            validate_feed_skills({"python": "high"})

    @pytest.mark.parametrize("score", [0, 11, -3])
    def test_out_of_range_score(self, score):
        with pytest.raises(ValueError, match="between"):
            validate_feed_skills({"python": score})

    def test_duplicate_after_aliasing(self):
        with pytest.raises(ValueError, match="more than once"):
            validate_feed_skills({"py": 5, "python": 8})

    def test_too_many_domains(self):
        raw = {f"domain-{i}": 5 for i in range(21)}
        with pytest.raises(ValueError, match="max"):
            validate_feed_skills(raw)


class TestNormaliseFeedEntry:
    def test_full_entry(self):
        entry = normalise_feed_entry({
            "name": "Python Weekly", "url": "https://pw.example/feed",
            "topics": ["python", "news"], "mode": "digest",
            "project": "research", "skills": {"python": 9},
        })
        assert entry == {
            "url": "https://pw.example/feed",
            "topics": ["python", "news"],
            "skills": {"python": 9},
            "mode": "digest",
            "project": "research",
        }

    def test_invalid_shapes_are_none(self):
        assert normalise_feed_entry("not a dict") is None
        assert normalise_feed_entry({"name": "", "url": "https://x"}) is None
        assert normalise_feed_entry({"name": "X", "url": ""}) is None
        assert normalise_feed_entry({"name": "x" * 201, "url": "https://x"}) is None

    def test_bad_skills_dropped_leniently(self):
        entry = normalise_feed_entry({
            "name": "F", "url": "https://f.example",
            "skills": {"python": 8, "bad domain!!": 5, "rust": 99},
        })
        assert entry["skills"] == {"python": 8}

    def test_non_list_topics_become_empty(self):
        entry = normalise_feed_entry({
            "name": "F", "url": "https://f.example", "topics": "python",
        })
        assert entry["topics"] == []


class TestSyncAndLoad:
    FEEDS = [
        {"name": "Python Weekly", "url": "https://pw.example/feed",
         "topics": ["python"], "skills": {"python": 8}},
        {"name": "Docker Blog", "url": "https://docker.example/feed",
         "topics": ["docker"], "skills": {"docker": 3, "python": 1}},
        {"name": "No Skills", "url": "https://plain.example/feed"},
    ]

    def test_roundtrip(self, fake_store):
        count = sync_feed_influences(fake_store.client, self.FEEDS)
        assert count == 3
        loaded = load_feed_influences(fake_store.client)
        assert set(loaded) == {"Python Weekly", "Docker Blog", "No Skills"}
        assert loaded["Python Weekly"]["skills"] == {"python": 8}
        assert loaded["No Skills"]["skills"] == {}

    def test_full_replace_removes_stale(self, fake_store):
        sync_feed_influences(fake_store.client, self.FEEDS)
        sync_feed_influences(fake_store.client, self.FEEDS[:1])
        loaded = load_feed_influences(fake_store.client)
        assert set(loaded) == {"Python Weekly"}

    def test_sync_empty_clears(self, fake_store):
        sync_feed_influences(fake_store.client, self.FEEDS)
        assert sync_feed_influences(fake_store.client, []) == 0
        assert load_feed_influences(fake_store.client) == {}

    def test_unusable_entries_skipped(self, fake_store):
        count = sync_feed_influences(fake_store.client, [
            {"name": "OK", "url": "https://ok.example"},
            {"name": "", "url": "https://nameless.example"},
            "not a dict",
        ])
        assert count == 1

    def test_load_drops_damaged_json(self, fake_store):
        fake_store.client.hset(FEED_INFLUENCE_KEY, mapping={
            "Good": json.dumps({"url": "https://g.example", "skills": {"python": 5}}),
            "Bad": "{not json",
            "NoUrl": json.dumps({"skills": {"python": 5}}),
        })
        loaded = load_feed_influences(fake_store.client)
        assert set(loaded) == {"Good"}

    def test_load_survives_client_error(self):
        class Exploding:
            def hgetall(self, key):
                raise RuntimeError("valkey down")
        assert load_feed_influences(Exploding()) == {}

    def test_feeds_for_domain_sorted_strongest_first(self, fake_store):
        sync_feed_influences(fake_store.client, self.FEEDS)
        loaded = load_feed_influences(fake_store.client)
        feeds = feeds_for_domain(loaded, "python")
        assert [(f["feed_name"], f["influence"]) for f in feeds] == [
            ("Python Weekly", 8), ("Docker Blog", 1),
        ]
        assert feeds_for_domain(loaded, "rust") == []


def _store_article(store, embedder, key, content, *, feed_name=None,
                   title=None, source_url=None, state="active",
                   skill_domains=None, age_seconds=0):
    store_memory(store, embedder, key, content, namespace="knowledge", state=state)
    if feed_name:
        store.set_field(key, "feed_name", feed_name)
    if title:
        store.set_field(key, "title", title)
    if source_url:
        store.set_field(key, "source_url", source_url)
    if skill_domains:
        store.set_field(key, "skill_domains", json.dumps(skill_domains))
    if age_seconds:
        store.set_field(key, "created_at", str(time.time() - age_seconds))


PY_FEEDS = [{"feed_name": "Python Weekly", "influence": 2,
             "url": "https://pw.example/feed"}]


class TestGatherFeedKnowledge:
    def test_influence_caps_per_feed_count_newest_first(self, fake_store, fake_embedder):
        for i, age in enumerate([300, 100, 200]):
            _store_article(
                fake_store, fake_embedder, f"mem:knowledge:art{i}",
                f"Python article {i}", feed_name="Python Weekly",
                age_seconds=age,
            )
        picked = gather_feed_knowledge(fake_store, "python", PY_FEEDS)
        # influence 2 → the two newest (ages 100 and 200)
        assert [a["key"] for a in picked] == ["mem:knowledge:art1", "mem:knowledge:art2"]
        assert picked[0]["influence"] == 2

    def test_excludes_promoted_archived_and_other_feeds(self, fake_store, fake_embedder):
        _store_article(fake_store, fake_embedder, "mem:knowledge:a",
                       "Promoted", feed_name="Python Weekly",
                       skill_domains=["python"])
        _store_article(fake_store, fake_embedder, "mem:knowledge:b",
                       "Archived", feed_name="Python Weekly", state="archived")
        _store_article(fake_store, fake_embedder, "mem:knowledge:c",
                       "Other feed", feed_name="Rust Blog")
        _store_article(fake_store, fake_embedder, "mem:knowledge:d",
                       "Eligible", feed_name="Python Weekly")
        picked = gather_feed_knowledge(fake_store, "python", PY_FEEDS)
        assert [a["key"] for a in picked] == ["mem:knowledge:d"]

    def test_promoted_to_other_domain_still_eligible(self, fake_store, fake_embedder):
        _store_article(fake_store, fake_embedder, "mem:knowledge:a",
                       "Cross-promoted", feed_name="Python Weekly",
                       skill_domains=["rust"])
        picked = gather_feed_knowledge(fake_store, "python", PY_FEEDS)
        assert [a["key"] for a in picked] == ["mem:knowledge:a"]

    def test_total_cap_trims_weakest_influence_first(self, fake_store, fake_embedder, monkeypatch):
        feeds = [
            {"feed_name": "Strong", "influence": 2, "url": ""},
            {"feed_name": "Weak", "influence": 1, "url": ""},
        ]
        for i in range(2):
            _store_article(fake_store, fake_embedder, f"mem:knowledge:s{i}",
                           f"Strong {i}", feed_name="Strong", age_seconds=i)
        _store_article(fake_store, fake_embedder, "mem:knowledge:w0",
                       "Weak 0", feed_name="Weak")
        monkeypatch.setenv("SKILL_FEED_MAX_ARTICLES", "2")
        picked = gather_feed_knowledge(fake_store, "python", feeds)
        assert [a["feed_name"] for a in picked] == ["Strong", "Strong"]

    def test_zero_cap_disables(self, fake_store, fake_embedder, monkeypatch):
        _store_article(fake_store, fake_embedder, "mem:knowledge:a",
                       "Anything", feed_name="Python Weekly")
        monkeypatch.setenv("SKILL_FEED_MAX_ARTICLES", "0")
        assert gather_feed_knowledge(fake_store, "python", PY_FEEDS) == []

    def test_no_feeds_or_no_articles(self, fake_store, fake_embedder):
        assert gather_feed_knowledge(fake_store, "python", []) == []
        assert gather_feed_knowledge(fake_store, "python", PY_FEEDS) == []

    def test_scan_cap_limits_keys(self, fake_store, fake_embedder, monkeypatch):
        import memory.skills as skills_module
        for i in range(3):
            _store_article(fake_store, fake_embedder, f"mem:knowledge:cap{i}",
                           f"Article {i}", feed_name="Python Weekly")
        monkeypatch.setattr(skills_module, "_MAX_POOL_KEYS", 1)
        picked = gather_feed_knowledge(fake_store, "python", PY_FEEDS)
        assert len(picked) <= 1

    def test_bare_row_skipped(self, fake_store, fake_embedder):
        # A knowledge key carrying none of the pool fields reads as a None
        # row and must not crash the gather.
        fake_store.client.hset("mem:knowledge:bare", field="unrelated", value="x")
        _store_article(fake_store, fake_embedder, "mem:knowledge:real",
                       "Real article", feed_name="Python Weekly")
        picked = gather_feed_knowledge(fake_store, "python", PY_FEEDS)
        assert [a["key"] for a in picked] == ["mem:knowledge:real"]


class TestBuildFeedRules:
    def test_rule_shape(self):
        rules = build_feed_rules([{
            "key": "mem:knowledge:a", "title": "Big News",
            "content": "Python 3.14 ships free-threading by default",
            "feed_name": "Python Weekly", "influence": 8,
            "source_url": "https://pw.example/314",
        }])
        assert len(rules) == 1
        rule = rules[0]
        assert rule.kind == "feed"
        assert rule.text.startswith("Big News: Python 3.14")
        assert rule.sources == ["mem:knowledge:a"]
        assert rule.feed == "Python Weekly"
        assert rule.influence == 8
        assert rule.url == "https://pw.example/314"
        d = rule.to_dict()
        assert d["feed"] == "Python Weekly"
        assert d["influence"] == 8

    def test_title_only_and_empty_skipped(self):
        rules = build_feed_rules([
            {"key": "mem:knowledge:a", "title": "Only a title", "content": "",
             "feed_name": "F", "influence": 1, "source_url": ""},
            {"key": "mem:knowledge:b", "title": "", "content": "",
             "feed_name": "F", "influence": 1, "source_url": ""},
        ])
        assert [r.text for r in rules] == ["Only a title"]


def _feed_rule(key="mem:knowledge:a", text="Latest python news",
               feed="Python Weekly", influence=4):
    return Rule(kind="feed", text=text, sources=[key], reinforcement=1,
                feed=feed, influence=influence, url="https://pw.example/a")


class TestRendering:
    def test_feed_watch_section(self):
        body = render_skill_md(
            domain="python", user="local", description="Python procedure.",
            rules=[
                Rule(kind="do", text="Use uv", sources=["mem:episodic:01A"],
                     reinforcement=2),
                _feed_rule(),
            ],
            compiled_at=time.time(), min_reinforcement=2,
        )
        assert "## Feed watch  (influenced feeds)" in body
        assert "(via Python Weekly, influence 4/10) (https://pw.example/a) [mem:knowledge:a]" in body
        assert "mem:knowledge:a   # feed: Python Weekly (influence 4/10)" in body
        assert "1 feed-watch article from 1 influencing feed (Python Weekly)." in body

    def test_no_feed_rules_no_section(self):
        body = render_skill_md(
            domain="python", user="local", description="Python procedure.",
            rules=[Rule(kind="do", text="Use uv", sources=["mem:episodic:01A"],
                        reinforcement=2)],
            compiled_at=time.time(), min_reinforcement=2,
        )
        assert "Feed watch" not in body
        assert "feed-watch" not in body


class TestFeedChangeRisk:
    def test_feed_churn_is_always_low_risk(self):
        old = [_feed_rule().to_dict()]
        new = [_feed_rule(key="mem:knowledge:b", text="Fresher news").to_dict()]
        changes = summarise_rule_changes(old, new)
        assert {c["change"] for c in changes} == {"added", "removed"}
        assert all(c["risk"] == "low" for c in changes)


def _reinforced_pool(store, embedder, n=2):
    for i in range(n):
        store_memory(
            store, embedder, f"mem:episodic:01POOL{i:03d}",
            "Worked on a python task", tags=["python"],
            effort_score=3, outcome="succeeded",
            breakthrough="Use uv for python dependency management",
        )


class TestCompileFlowWithFeeds:
    def _seed(self, fake_store, fake_embedder, influence=2):
        _reinforced_pool(fake_store, fake_embedder)
        sync_feed_influences(fake_store.client, [
            {"name": "Python Weekly", "url": "https://pw.example/feed",
             "skills": {"python": influence}},
        ])
        _store_article(fake_store, fake_embedder, "mem:knowledge:feedart",
                       "Free-threading lands", feed_name="Python Weekly",
                       title="Python 3.14", source_url="https://pw.example/314")

    def test_propose_includes_feed_rules(self, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        result = compile_skill_flow(fake_store, fake_embedder, "python")
        assert result["status"] == "proposal"
        assert result["rules"].get("feed") == 1
        assert "Feed watch" in result["draft"]
        assert "mem:knowledge:feedart" in result["draft"]

    def test_write_commits_feed_rules_into_manifests(self, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        compile_skill_flow(fake_store, fake_embedder, "python")
        written = compile_skill_flow(fake_store, fake_embedder, "python", mode="write")
        assert written["status"] == "written"
        skill = fake_store.get(written["skill_id"])
        manifest = json.loads(skill["rule_manifest"])
        feed_rules = [r for r in manifest if r["kind"] == "feed"]
        assert feed_rules and feed_rules[0]["feed"] == "Python Weekly"
        assert "mem:knowledge:feedart" in json.loads(skill["source_manifest"])

    def test_feeds_do_not_bootstrap_a_skill(self, fake_store, fake_embedder):
        # Influence + articles but no experience pool and nothing promoted.
        sync_feed_influences(fake_store.client, [
            {"name": "Python Weekly", "url": "https://pw.example/feed",
             "skills": {"python": 5}},
        ])
        _store_article(fake_store, fake_embedder, "mem:knowledge:feedart",
                       "Free-threading lands", feed_name="Python Weekly")
        result = compile_skill_flow(fake_store, fake_embedder, "python")
        assert result["status"] == "no_candidates"

    def test_unassociated_feed_never_reaches_skill(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _store_article(fake_store, fake_embedder, "mem:knowledge:feedart",
                       "Free-threading lands", feed_name="Python Weekly")
        result = compile_skill_flow(fake_store, fake_embedder, "python")
        assert result["status"] == "proposal"
        assert "feed" not in result["rules"]
        assert "mem:knowledge:feedart" not in result["draft"]

    def test_new_article_proposes_low_risk_change(self, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        compile_skill_flow(fake_store, fake_embedder, "python")
        compile_skill_flow(fake_store, fake_embedder, "python", mode="write")

        _store_article(fake_store, fake_embedder, "mem:knowledge:feedart2",
                       "Pattern matching deep dive", feed_name="Python Weekly")
        result = compile_skill_flow(fake_store, fake_embedder, "python")
        assert result["status"] == "proposal"
        feed_changes = [c for c in result["changes"] if c["rule_kind"] == "feed"]
        assert feed_changes
        assert all(c["risk"] == "low" for c in feed_changes)

    def test_compile_is_deterministic(self, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        first = compile_skill_flow(fake_store, fake_embedder, "python")
        second = compile_skill_flow(fake_store, fake_embedder, "python")
        assert bodies_equivalent(first["draft"], second["draft"])
