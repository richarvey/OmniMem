"""Tests for promoted knowledge feeding skills: promotion, ref rules,
the knowledge watch briefing surface, and pending-update detection."""

import json
import time

import pytest

import tools as tools_module
from tests.conftest import store_memory


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


from memory.skills import (
    build_reference_rules,
    gather_promoted_knowledge,
    generated_skill_key,
    summarise_rule_changes,
)
from tools.briefing import briefing
from tools.knowledge import promote_knowledge
from tools.skills import compile_skill, get_skill, knowledge_watch, pending_skill_updates

SKILL_ID = generated_skill_key("python", "local")


def store_article(store, embedder, key, content, title=None, source_url=None,
                  feed_name=None, state="active", expires_at=None,
                  age_seconds=0):
    store_memory(store, embedder, key, content, namespace="knowledge", state=state)
    if age_seconds:
        ts = str(time.time() - age_seconds)
        store.set_field(key, "created_at", ts)
        store.set_field(key, "updated_at", ts)
    if title:
        store.set_field(key, "title", title)
    if source_url:
        store.set_field(key, "source_url", source_url)
    if feed_name:
        store.set_field(key, "feed_name", feed_name)
    if expires_at:
        store.set_field(key, "expires_at", str(expires_at))


def _reinforced_pool(store, embedder, n=2, text="Use uv for python dependency management"):
    for i in range(n):
        store_memory(
            store, embedder, f"mem:episodic:01KPOOL{i:03d}",
            "Worked on a python task", tags=["python"],
            effort_score=3, outcome="succeeded", breakthrough=text,
        )


def _accept(domain="python", **kwargs):
    proposal = compile_skill(domain, mode="propose", **kwargs)
    assert proposal["status"] == "proposal", proposal
    written = compile_skill(domain, mode="write")
    assert written["status"] == "written", written
    return proposal, written


class TestPromoteToDomain:
    def test_sets_skill_domains_and_clears_expiry(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "Python 3.14 release notes", expires_at=time.time() + 86400)
        result = promote_knowledge("mem:knowledge:k01", domain="python")
        assert result["promoted"] is True
        assert result["skill_domains"] == ["python"]
        data = fake_store.get("mem:knowledge:k01")
        assert json.loads(data["skill_domains"]) == ["python"]
        assert data["expires_at"] == ""
        assert float(data["promoted_at"]) > 0

    def test_alias_resolves(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article")
        result = promote_knowledge("mem:knowledge:k01", domain="py")
        assert result["skill_domains"] == ["python"]

    def test_invalid_domain_errors(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article")
        result = promote_knowledge("mem:knowledge:k01", domain="!!bad!!")
        assert "error" in result

    def test_idempotent(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article")
        promote_knowledge("mem:knowledge:k01", domain="python")
        result = promote_knowledge("mem:knowledge:k01", domain="python")
        assert result["skill_domains"] == ["python"]

    def test_demote_removes_domain(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article")
        promote_knowledge("mem:knowledge:k01", domain="python")
        result = promote_knowledge("mem:knowledge:k01", domain="python", demote=True)
        assert result["demoted_from"] == "python"
        data = fake_store.get("mem:knowledge:k01")
        assert json.loads(data["skill_domains"]) == []

    def test_demote_not_promoted_errors(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article")
        result = promote_knowledge("mem:knowledge:k01", domain="python", demote=True)
        assert "error" in result

    def test_demote_without_domain_errors(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article")
        result = promote_knowledge("mem:knowledge:k01", demote=True)
        assert "error" in result

    def test_plain_promotion_unchanged(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article",
                      expires_at=time.time() + 86400)
        result = promote_knowledge("mem:knowledge:k01")
        assert result == {"key": "mem:knowledge:k01", "promoted": True}
        assert "skill_domains" not in fake_store.get("mem:knowledge:k01")


class TestGatherPromotedKnowledge:
    def test_only_promoted_and_active(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Promoted")
        store_article(fake_store, fake_embedder, "mem:knowledge:k02", "Not promoted")
        store_article(fake_store, fake_embedder, "mem:knowledge:k03", "Archived",
                      state="archived")
        promote_knowledge("mem:knowledge:k01", domain="python")
        promote_knowledge("mem:knowledge:k03", domain="python")  # errors, archived
        pools = gather_promoted_knowledge(fake_store, ["python"])
        assert [e["key"] for e in pools["python"]] == ["mem:knowledge:k01"]

    def test_reference_rules_use_title_and_url(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "The 3.14 release changes packaging defaults",
                      title="Python 3.14 released",
                      source_url="https://example.com/py314")
        promote_knowledge("mem:knowledge:k01", domain="python")
        pools = gather_promoted_knowledge(fake_store, ["python"])
        rules = build_reference_rules(pools["python"])
        assert len(rules) == 1
        assert rules[0].kind == "ref"
        assert rules[0].text.startswith("Python 3.14 released:")
        assert rules[0].url == "https://example.com/py314"
        assert rules[0].sources == ["mem:knowledge:k01"]


class TestCompileWithReferences:
    def test_ref_section_rendered(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "The 3.14 release changes packaging defaults",
                      title="Python 3.14 released",
                      source_url="https://example.com/py314")
        promote_knowledge("mem:knowledge:k01", domain="python")

        proposal, _ = _accept()
        assert proposal["rules"] == {"do": 1, "ref": 1}
        skill = get_skill(SKILL_ID)
        assert "## Reference  (promoted knowledge)" in skill["body"]
        assert "https://example.com/py314" in skill["body"]
        assert "mem:knowledge:k01" in skill["source_manifest"]
        manifest = json.loads(fake_store.get(SKILL_ID)["rule_manifest"])
        kinds = sorted(r["kind"] for r in manifest)
        assert kinds == ["do", "ref"]

    def test_no_refs_means_no_section(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        body = get_skill(SKILL_ID)["body"]
        assert "## Reference" not in body

    def test_pure_reference_skill_compiles(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "WCAG 2.2 contrast requirements", title="WCAG 2.2")
        promote_knowledge("mem:knowledge:k01", domain="accessibility")
        proposal, written = _accept(domain="accessibility")
        assert proposal["rules"] == {"ref": 1}
        assert written["new_skill"] is True

    def test_recompile_is_stable(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article one")
        promote_knowledge("mem:knowledge:k01", domain="python")
        _accept()
        result = compile_skill("python", mode="propose")
        assert result["status"] == "unchanged"

    def test_demote_then_recompile_flags_removal(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article one")
        promote_knowledge("mem:knowledge:k01", domain="python")
        _accept()
        promote_knowledge("mem:knowledge:k01", domain="python", demote=True)
        result = compile_skill("python", mode="propose")
        assert result["status"] == "proposal"
        removed = [c for c in result["changes"] if c["change"] == "removed"]
        assert removed and removed[0]["risk"] == "high"
        assert removed[0]["rule_kind"] == "ref"


class TestExtractedReferenceRules:
    FIVE = [
        {"kind": "dont", "text": "Rely on colour alone to convey state"},
        {"kind": "dont", "text": "Remove focus outlines without a replacement"},
        {"kind": "dont", "text": "Use placeholder text as the only label"},
        {"kind": "do", "text": "Meet 4.5:1 contrast for body text"},
        {"kind": "note", "text": "WCAG 2.2 AA is the baseline these map to"},
    ]

    def _article(self, fake_store, fake_embedder):
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "Five common accessibility mistakes and how to avoid them",
                      title="5 things to avoid for accessible websites",
                      source_url="https://example.com/a11y-5")

    def test_promotion_stores_and_returns_rules(self, fake_store, fake_embedder):
        self._article(fake_store, fake_embedder)
        result = promote_knowledge("mem:knowledge:k01", domain="accessibility",
                                   rules=self.FIVE)
        assert result["reference_rules"] == self.FIVE
        stored = json.loads(fake_store.get("mem:knowledge:k01")["skill_rules"])
        assert stored == self.FIVE

    def test_invalid_kind_errors(self, fake_store, fake_embedder):
        self._article(fake_store, fake_embedder)
        result = promote_knowledge("mem:knowledge:k01", domain="accessibility",
                                   rules=[{"kind": "never", "text": "x"}])
        assert "error" in result

    def test_rules_without_domain_errors(self, fake_store, fake_embedder):
        self._article(fake_store, fake_embedder)
        result = promote_knowledge("mem:knowledge:k01", rules=self.FIVE)
        assert "error" in result

    def test_rules_with_demote_errors(self, fake_store, fake_embedder):
        self._article(fake_store, fake_embedder)
        promote_knowledge("mem:knowledge:k01", domain="accessibility")
        result = promote_knowledge("mem:knowledge:k01", domain="accessibility",
                                   demote=True, rules=self.FIVE)
        assert "error" in result

    def test_compile_renders_one_bullet_per_rule(self, fake_store, fake_embedder):
        self._article(fake_store, fake_embedder)
        promote_knowledge("mem:knowledge:k01", domain="accessibility",
                          rules=self.FIVE)
        proposal, _ = _accept(domain="accessibility")
        assert proposal["rules"] == {"ref": 5}
        body = get_skill(generated_skill_key("accessibility", "local"))["body"]
        assert "- Avoid: Rely on colour alone to convey state." in body
        assert "- Do: Meet 4.5:1 contrast for body text." in body
        assert "- WCAG 2.2 AA is the baseline these map to." in body
        assert body.count("mem:knowledge:k01") >= 6  # 5 bullets + manifest

    def test_recompile_stable_with_extracted_rules(self, fake_store, fake_embedder):
        self._article(fake_store, fake_embedder)
        promote_knowledge("mem:knowledge:k01", domain="accessibility",
                          rules=self.FIVE)
        _accept(domain="accessibility")
        result = compile_skill("accessibility", mode="propose")
        assert result["status"] == "unchanged"

    def test_repromote_with_edited_rules_proposes_diff(
            self, fake_store, fake_embedder):
        self._article(fake_store, fake_embedder)
        promote_knowledge("mem:knowledge:k01", domain="accessibility",
                          rules=self.FIVE)
        _accept(domain="accessibility")
        promote_knowledge("mem:knowledge:k01", domain="accessibility",
                          rules=self.FIVE[:3])
        result = compile_skill("accessibility", mode="propose")
        assert result["status"] == "proposal"
        assert any(c["change"] == "removed" for c in result["changes"])

    def test_empty_rules_reverts_to_summary(self, fake_store, fake_embedder):
        self._article(fake_store, fake_embedder)
        promote_knowledge("mem:knowledge:k01", domain="accessibility",
                          rules=self.FIVE)
        result = promote_knowledge("mem:knowledge:k01", domain="accessibility",
                                   rules=[])
        assert "reverts" in result["note"]
        proposal = compile_skill("accessibility", mode="propose")
        assert proposal["rules"] == {"ref": 1}


class TestSummariseRefChanges:
    def test_added_ref_is_low_risk(self):
        changes = summarise_rule_changes([], [
            {"kind": "ref", "text": "Python 3.14 released",
             "sources": ["mem:knowledge:k01"], "reinforcement": 1},
        ])
        assert changes == [{
            "change": "added", "risk": "low", "rule_kind": "ref",
            "rule": "Python 3.14 released",
        }]

    def test_removed_ref_is_high_risk(self):
        changes = summarise_rule_changes([
            {"kind": "ref", "text": "Python 3.14 released",
             "sources": ["mem:knowledge:k01"], "reinforcement": 1},
        ], [])
        assert changes[0]["change"] == "removed"
        assert changes[0]["risk"] == "high"


class TestPendingUpdates:
    def test_newly_promoted_article_is_low_risk_new_reference(
            self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "Fresh python article", title="Fresh python article")
        promote_knowledge("mem:knowledge:k01", domain="python")

        updates = pending_skill_updates(fake_store)
        assert len(updates) == 1
        refs = [c for c in updates[0]["changes"] if c["change"] == "new_reference"]
        assert refs == [{
            "change": "new_reference", "risk": "low",
            "source": "mem:knowledge:k01", "gist": "Fresh python article",
        }]
        assert updates[0]["batch_accept_eligible"] is True

    def test_compiled_reference_is_quiet(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01", "Article")
        promote_knowledge("mem:knowledge:k01", domain="python")
        _accept()
        assert pending_skill_updates(fake_store) == []


class TestKnowledgeWatch:
    def _compiled_skill(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()

    def test_nearby_article_surfaces(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("SKILL_KNOWLEDGE_WATCH_THRESHOLD", "-1")
        self._compiled_skill(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "python packaging work and python dependency management experience",
                      feed_name="PyFeed")
        watch = knowledge_watch(fake_store)
        assert len(watch) == 1
        assert watch[0]["domain"] == "python"
        assert watch[0]["articles"][0]["key"] == "mem:knowledge:k01"
        assert "promote_knowledge" in watch[0]["note"]

    def test_below_threshold_is_quiet(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("SKILL_KNOWLEDGE_WATCH_THRESHOLD", "0.99")
        self._compiled_skill(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "gardening tips for spring tomatoes")
        assert knowledge_watch(fake_store) == []

    def test_promoted_article_excluded(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("SKILL_KNOWLEDGE_WATCH_THRESHOLD", "-1")
        self._compiled_skill(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "python packaging work and python dependency management experience")
        promote_knowledge("mem:knowledge:k01", domain="python")
        assert knowledge_watch(fake_store) == []

    def test_old_article_excluded(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("SKILL_KNOWLEDGE_WATCH_THRESHOLD", "-1")
        self._compiled_skill(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "python packaging work and python dependency management experience",
                      age_seconds=30 * 86400)
        assert knowledge_watch(fake_store) == []

    def test_negation_flags_possible_contradiction(
            self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("SKILL_KNOWLEDGE_WATCH_THRESHOLD", "-1")
        self._compiled_skill(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "uv deprecated: don't use uv for python dependency management")
        watch = knowledge_watch(fake_store)
        assert len(watch) == 1
        article = watch[0]["articles"][0]
        assert article["possible_contradiction"] is True
        assert article["conflicts_with_rules"]

    def test_disabled_by_env(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("SKILL_KNOWLEDGE_WATCH_DAYS", "0")
        self._compiled_skill(fake_store, fake_embedder)
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "python packaging work and python dependency management experience")
        assert knowledge_watch(fake_store) == []


class TestBriefingSurface:
    def test_briefing_carries_knowledge_watch(
            self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("SKILL_KNOWLEDGE_WATCH_THRESHOLD", "-1")
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        store_article(fake_store, fake_embedder, "mem:knowledge:k01",
                      "python packaging work and python dependency management experience")
        result = briefing(project="someproj")
        assert "skill_knowledge_watch" in result
        assert result["skill_knowledge_watch"][0]["domain"] == "python"
