"""Tests for project work-type domains (v6.6): the cross-project routing layer.

Covers the shared vocabulary (project domains normalise exactly like skill
domains), the multi-project recall filter they resolve to, the honest
reporting when a requested domain matches no project, and the stack-derived
backfill that stops the feature being empty after an upgrade.
"""

import json
import time

import pytest

from tests.conftest import store_memory

import tools as tools_module

from memory import project_domains as pd
from memory.migrations import migrate_project_domains
from memory.recall import _build_filter_expr, _candidate_k, normalise_project_filter


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


def make_project(store, embedder, name, domains=None, stack="", goals="ship it",
                 description="", state="active"):
    """Store a project context entry the way set_project_context would."""
    now = str(time.time())
    fields = {
        "content": description or name,
        "project_name": name,
        "description": description or f"{name} description",
        "stack": stack,
        "goals": goals,
        "current_state": "in progress",
        "state": state,
        "surface_score": "1.0",
        "created_at": now,
        "updated_at": now,
    }
    if domains is not None:
        fields["domains"] = pd.serialise_domains(domains)
    store.upsert("project", f"mem:project:{name}", fields, embedder.embed(name))
    pd.invalidate_domain_cache()


# --- vocabulary --------------------------------------------------------------


class TestParseDomains:
    def test_comma_separated(self):
        assert pd.parse_domains("python, docker") == ["python", "docker"]

    def test_json_array_still_reads(self):
        # Backups and hand edits may carry the JSON shape other list fields use.
        assert pd.parse_domains('["python", "docker"]') == ["python", "docker"]

    def test_malformed_json_falls_back_to_splitting(self):
        assert pd.parse_domains('["python", "docker"') == ['["python"', '"docker"']

    def test_alternative_separators(self):
        assert pd.parse_domains("python / docker | css") == [
            "python", "docker", "css"
        ]

    def test_list_input(self):
        assert pd.parse_domains(["python", "docker"]) == ["python", "docker"]

    def test_deduplicates_preserving_order(self):
        assert pd.parse_domains("b, a, b") == ["b", "a"]

    def test_empty_and_none(self):
        assert pd.parse_domains(None) == []
        assert pd.parse_domains("") == []
        assert pd.parse_domains("   ") == []

    def test_unusable_type(self):
        assert pd.parse_domains(42) == []

    def test_non_string_list_members_skipped(self):
        assert pd.parse_domains(["python", 7, None]) == ["python"]


class TestNormaliseDomains:
    def test_shares_the_skill_alias_table(self):
        # The whole point: `py` on a project and `py` on a skill agree.
        domains, aliased, rejected = pd.normalise_domains("py, k8s")
        assert domains == ["python", "kubernetes"]
        assert aliased == {"py": "python", "k8s": "kubernetes"}
        assert rejected == []

    def test_lowercases_and_kebabs(self):
        domains, _, _ = pd.normalise_domains(["WCAG Accessibility"])
        assert domains == ["wcag-accessibility"]

    def test_rejects_unusable_values(self):
        domains, _, rejected = pd.normalise_domains("python, <script>, ok")
        assert domains == ["python", "ok"]
        assert rejected == ["<script>"]

    def test_alias_and_canonical_collapse_to_one(self):
        domains, _, _ = pd.normalise_domains(["py", "python"])
        assert domains == ["python"]

    def test_caps_the_list(self):
        domains, _, _ = pd.normalise_domains(
            [f"d{i}" for i in range(pd.MAX_PROJECT_DOMAINS + 10)]
        )
        assert len(domains) == pd.MAX_PROJECT_DOMAINS

    def test_too_long_rejected(self):
        domains, _, rejected = pd.normalise_domains("x" * 65)
        assert domains == []
        assert rejected == ["x" * 65]


class TestSerialiseDomains:
    def test_comma_separated_not_json(self):
        # A TAG field splits on commas — the JSON form used by `tags` and
        # `topics` indexes as unusable tokens, which is why they are never
        # filtered on.
        assert pd.serialise_domains(["python", "docker"]) == "python,docker"

    def test_round_trips(self):
        stored = pd.serialise_domains(["python", "docker"])
        assert pd.read_project_domains({"domains": stored}) == ["python", "docker"]

    def test_empty(self):
        assert pd.serialise_domains([]) == ""
        assert pd.read_project_domains({"domains": ""}) == []
        assert pd.read_project_domains(None) == []


class TestAliasHint:
    def test_known_alias(self):
        assert pd.alias_hint("py") == "python"

    def test_not_an_alias(self):
        assert pd.alias_hint("python") is None


class TestProjectNameFrom:
    def test_prefers_project_name(self):
        assert pd.project_name_from(
            "mem:project:x", {"project_name": "a", "project": "b"}
        ) == "a"

    def test_falls_back_to_project(self):
        assert pd.project_name_from("mem:project:x", {"project": "b"}) == "b"

    def test_falls_back_to_key_suffix(self):
        assert pd.project_name_from("mem:project:x", {}) == "x"
        assert pd.project_name_from("mem:project:x", None) == "x"


# --- resolution --------------------------------------------------------------


class TestDomainMap:
    def test_groups_projects_by_domain(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python", "docker"])
        make_project(fake_store, fake_embedder, "beta", ["python"])
        mapping = pd.domain_map(fake_store)
        assert mapping["python"] == ["alpha", "beta"]
        assert mapping["docker"] == ["alpha"]

    def test_skips_projects_with_no_domains(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", [])
        assert pd.domain_map(fake_store) == {}

    def test_skips_archived_projects(self, fake_store, fake_embedder):
        make_project(
            fake_store, fake_embedder, "old", ["python"], state="archived"
        )
        assert pd.domain_map(fake_store) == {}

    def test_empty_store(self, fake_store):
        assert pd.domain_map(fake_store) == {}

    def test_cache_serves_repeat_calls(self, fake_store, fake_embedder, monkeypatch):
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        pd.domain_map(fake_store)

        calls = []
        original = fake_store.scan_prefix
        monkeypatch.setattr(
            fake_store, "scan_prefix",
            lambda prefix: (calls.append(prefix), original(prefix))[1],
        )
        pd.domain_map(fake_store)
        assert calls == []

    def test_cache_can_be_bypassed(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        pd.domain_map(fake_store)
        make_project(fake_store, fake_embedder, "beta", ["python"])
        pd.invalidate_domain_cache()
        assert pd.domain_map(fake_store, use_cache=False)["python"] == [
            "alpha", "beta"
        ]

    def test_bad_ttl_env_falls_back(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("PROJECT_DOMAIN_CACHE_TTL_SECONDS", "not-a-number")
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        assert pd.domain_map(fake_store)["python"] == ["alpha"]

    def test_ttl_zero_disables_cache(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("PROJECT_DOMAIN_CACHE_TTL_SECONDS", "0")
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        pd.domain_map(fake_store)
        make_project(fake_store, fake_embedder, "beta", ["python"])
        assert pd.domain_map(fake_store)["python"] == ["alpha", "beta"]

    def test_ulid_project_memory_without_domains_ignored(
        self, fake_store, fake_embedder
    ):
        store_memory(
            fake_store, fake_embedder, "mem:project:01ABC", "loose note",
            namespace="project", project="alpha",
        )
        assert pd.domain_map(fake_store) == {}


class TestResolveProjectsForDomains:
    def test_matched(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        res = pd.resolve_projects_for_domains(fake_store, "python")
        assert res.projects == ["alpha"]
        assert res.unmatched == []
        assert res.fully_unmatched is False

    def test_alias_resolves_before_lookup(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        res = pd.resolve_projects_for_domains(fake_store, "py")
        assert res.projects == ["alpha"]
        assert res.aliased == {"py": "python"}

    def test_partial_match_reports_the_gap(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        res = pd.resolve_projects_for_domains(fake_store, ["python", "rust"])
        assert res.projects == ["alpha"]
        assert res.unmatched == ["rust"]
        assert res.fully_unmatched is False

    def test_fully_unmatched(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        res = pd.resolve_projects_for_domains(fake_store, "rust")
        assert res.projects == []
        assert res.fully_unmatched is True

    def test_union_across_domains(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        make_project(fake_store, fake_embedder, "beta", ["docker"])
        res = pd.resolve_projects_for_domains(fake_store, ["python", "docker"])
        assert res.projects == ["alpha", "beta"]

    def test_unusable_values_dropped(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        res = pd.resolve_projects_for_domains(fake_store, ["python", "<bad>"])
        assert res.requested == ["python"]
        assert res.projects == ["alpha"]

    def test_no_request_is_not_unmatched(self, fake_store):
        res = pd.resolve_projects_for_domains(fake_store, [])
        assert res.fully_unmatched is False

    def test_as_dict(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python"])
        res = pd.resolve_projects_for_domains(fake_store, ["py", "rust"])
        payload = res.as_dict()
        assert payload["projects"] == ["alpha"]
        assert payload["unmatched_domains"] == ["rust"]
        assert payload["resolved_aliases"] == {"py": "python"}


class TestKnownProjectDomains:
    def test_counts_projects_per_domain(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", ["python", "docker"])
        make_project(fake_store, fake_embedder, "beta", ["python"])
        assert pd.known_project_domains(fake_store) == {"python": 2, "docker": 1}


# --- suggestion --------------------------------------------------------------


class TestSuggestDomainsForProject:
    def test_reads_the_stack_field(self, fake_store, fake_embedder):
        make_project(
            fake_store, fake_embedder, "alpha", stack="Python, Docker, Valkey"
        )
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["suggested_domains"] == ["python", "docker", "valkey"]
        assert out["evidence"]["python"] == ["stack"]

    def test_drops_stack_filler_words(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", stack="Python and Docker")
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["suggested_domains"] == ["python", "docker"]

    def test_drops_comma_separated_stopwords(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", stack="Python, etc, misc")
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["suggested_domains"] == ["python"]

    def test_drops_unusable_stack_entries(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", stack="Python, <script>")
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["suggested_domains"] == ["python"]

    def test_drops_unusable_tag_values(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha")
        for i in range(2):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:bad{i}", f"m{i}",
                project="alpha", tags=["<script>"],
            )
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["suggested_domains"] == []

    def test_recurring_tags_become_domains(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha")
        for i in range(2):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:tag{i}", f"lesson {i}",
                project="alpha", tags=["htmx"],
            )
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert "htmx" in out["suggested_domains"]
        assert out["evidence"]["htmx"] == ["tagged on 2 memories"]

    def test_single_occurrence_tag_is_not_evidence(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:one", "one off",
            project="alpha", tags=["bugfix"],
        )
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["suggested_domains"] == []

    def test_other_projects_tags_ignored(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha")
        for i in range(3):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:o{i}", f"other {i}",
                project="beta", tags=["rust"],
            )
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["suggested_domains"] == []

    def test_archived_memories_ignored(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha")
        for i in range(3):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:a{i}", f"old {i}",
                project="alpha", tags=["legacy"], state="archived",
            )
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["suggested_domains"] == []

    def test_existing_domains_are_never_dropped(self, fake_store, fake_embedder):
        make_project(
            fake_store, fake_embedder, "alpha", domains=["design"], stack="Python"
        )
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["existing_domains"] == ["design"]
        assert out["suggested_domains"] == ["python"]
        assert out["merged_domains"] == ["design", "python"]

    def test_already_declared_not_re_suggested(self, fake_store, fake_embedder):
        make_project(
            fake_store, fake_embedder, "alpha", domains=["python"], stack="Python"
        )
        out = pd.suggest_domains_for_project(fake_store, "alpha")
        assert out["suggested_domains"] == []

    def test_missing_project(self, fake_store):
        out = pd.suggest_domains_for_project(fake_store, "ghost")
        assert out["merged_domains"] == []

    def test_respects_limit(self, fake_store, fake_embedder):
        make_project(
            fake_store, fake_embedder, "alpha",
            stack=", ".join(f"tech{i}" for i in range(20)),
        )
        out = pd.suggest_domains_for_project(fake_store, "alpha", limit=3)
        assert len(out["suggested_domains"]) == 3

    def test_scan_cap_logged(self, fake_store, fake_embedder, monkeypatch, caplog):
        make_project(fake_store, fake_embedder, "alpha")
        monkeypatch.setattr(pd, "_MAX_SUGGEST_SCAN_KEYS", 1)
        for i in range(3):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:c{i}", f"m{i}",
                project="alpha", tags=["python"],
            )
        with caplog.at_level("WARNING"):
            pd.suggest_domains_for_project(fake_store, "alpha")
        assert "capped" in caplog.text


class TestParseTagField:
    def test_json_list(self):
        assert pd._parse_tag_field(json.dumps(["a", "b"])) == ["a", "b"]

    def test_plain_list(self):
        assert pd._parse_tag_field(["a", "b"]) == ["a", "b"]

    def test_comma_string(self):
        assert pd._parse_tag_field("a, b") == ["a", "b"]

    def test_empty(self):
        assert pd._parse_tag_field(None) == []
        assert pd._parse_tag_field("") == []

    def test_json_object_is_not_a_tag_list(self):
        assert pd._parse_tag_field('{"a": 1}') == []

    def test_unusable_type(self):
        assert pd._parse_tag_field(3.5) == []


# --- recall filter -----------------------------------------------------------


class TestNormaliseProjectFilter:
    def test_single_string(self):
        assert normalise_project_filter("alpha") == ["alpha"]

    def test_list(self):
        assert normalise_project_filter(["alpha", "beta"]) == ["alpha", "beta"]

    def test_deduplicates(self):
        assert normalise_project_filter(["a", "a"]) == ["a"]

    def test_drops_blank_and_non_strings(self):
        assert normalise_project_filter(["a", "", "  ", 7, None]) == ["a"]

    def test_empty(self):
        assert normalise_project_filter(None) == []
        assert normalise_project_filter([]) == []


class TestBuildFilterExpr:
    def test_single_project(self):
        expr = _build_filter_expr("episodic", "alpha")
        assert "@project:{alpha}" in expr

    def test_multiple_projects_use_clause_level_or(self):
        # In-brace alternation {a|b} returns nothing on valkey-search.
        expr = _build_filter_expr("episodic", ["alpha", "beta"])
        assert "(@project:{alpha} | @project:{beta})" in expr
        assert "{alpha|beta}" not in expr

    def test_raw_values_including_spaces(self):
        # Escaped or quoted tag values match nothing on valkey-search.
        expr = _build_filter_expr("episodic", ["hugo theme"])
        assert "@project:{hugo theme}" in expr
        assert "\\" not in expr

    def test_one_unsafe_value_drops_pushdown_entirely(self):
        # A partial filter would silently exclude the safe project's matches
        # from the unsafe one's, which is wrong — leave it to Python.
        expr = _build_filter_expr("episodic", ["alpha", "b@d"])
        assert "@project:" not in expr

    def test_project_namespace_has_no_pushdown(self):
        expr = _build_filter_expr("project", ["alpha"])
        assert "@project" not in expr

    def test_state_filter_always_present(self):
        assert "@state:{active}" in _build_filter_expr("episodic", None)


class TestCandidateK:
    def test_unfiltered(self):
        assert _candidate_k(5, None) == 20

    def test_respects_large_top_k(self):
        assert _candidate_k(40, None) == 40

    def test_single_project_overfetches(self):
        assert _candidate_k(5, "alpha") == 50

    def test_widens_with_the_project_set(self):
        assert _candidate_k(5, ["a", "b", "c"]) == 70

    def test_capped_at_one_hundred(self):
        assert _candidate_k(5, [f"p{i}" for i in range(30)]) == 100


class TestRecallWithProjectList:
    def test_filters_to_the_set(self, fake_store, fake_embedder, pipeline):
        for name in ("alpha", "beta", "gamma"):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:{name}",
                "valkey tag filter gotcha", project=name,
            )
        results = pipeline.recall(
            "valkey tag filter gotcha", top_k=10,
            project_filter=["alpha", "beta"],
        )
        assert {r.project for r in results} == {"alpha", "beta"}

    def test_empty_list_is_unscoped(self, fake_store, fake_embedder, pipeline):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:a", "docker build cache",
            project="alpha",
        )
        results = pipeline.recall("docker build cache", top_k=10, project_filter=[])
        assert results


# --- MCP tools ---------------------------------------------------------------


class TestSetProjectContextDomains:
    def test_stores_normalised_domains(self, fake_store):
        from tools.project import set_project_context

        result = set_project_context(
            "alpha", "desc", "python", "goals", "state", domains=["py", "Docker"]
        )
        assert result["domains"] == ["python", "docker"]
        assert result["resolved_aliases"] == {"py": "python"}
        assert fake_store.get("mem:project:alpha")["domains"] == "python,docker"

    def test_omitting_domains_preserves_existing(self, fake_store):
        from tools.project import set_project_context

        set_project_context("alpha", "d", "s", "g", "st", domains=["python"])
        set_project_context("alpha", "new desc", "s", "g", "st")
        assert fake_store.get("mem:project:alpha")["domains"] == "python"

    def test_empty_list_clears(self, fake_store):
        from tools.project import set_project_context

        set_project_context("alpha", "d", "s", "g", "st", domains=["python"])
        set_project_context("alpha", "d", "s", "g", "st", domains=[])
        assert fake_store.get("mem:project:alpha")["domains"] == ""

    def test_rejected_values_reported(self):
        from tools.project import set_project_context

        result = set_project_context(
            "alpha", "d", "s", "g", "st", domains=["python", "<bad>"]
        )
        assert result["rejected_domains"] == ["<bad>"]

    def test_comma_string_accepted(self, fake_store):
        from tools.project import set_project_context

        set_project_context("alpha", "d", "s", "g", "st", domains="python, docker")
        assert fake_store.get("mem:project:alpha")["domains"] == "python,docker"

    def test_new_project_without_domains_stores_none(self, fake_store):
        from tools.project import set_project_context

        result = set_project_context("alpha", "d", "s", "g", "st")
        assert "domains" not in result


class TestGetProjectContextDomains:
    def test_returns_domains(self, fake_store, fake_embedder):
        from tools.project import get_project_context

        make_project(fake_store, fake_embedder, "alpha", ["python"])
        assert get_project_context("alpha")["domains"] == ["python"]

    def test_absent_when_none(self, fake_store, fake_embedder):
        from tools.project import get_project_context

        make_project(fake_store, fake_embedder, "alpha", [])
        assert "domains" not in get_project_context("alpha")


class TestListProjectsDomainFilter:
    def test_lists_domains(self, fake_store, fake_embedder):
        from tools.project import list_projects

        make_project(fake_store, fake_embedder, "alpha", ["python"])
        assert list_projects()["projects"][0]["domains"] == ["python"]

    def test_filters(self, fake_store, fake_embedder):
        from tools.project import list_projects

        make_project(fake_store, fake_embedder, "alpha", ["python"])
        make_project(fake_store, fake_embedder, "beta", ["design"])
        out = list_projects(domain="python")
        assert [p["project_name"] for p in out["projects"]] == ["alpha"]
        assert out["domain"] == "python"

    def test_alias_filter(self, fake_store, fake_embedder):
        from tools.project import list_projects

        make_project(fake_store, fake_embedder, "alpha", ["python"])
        assert len(list_projects(domain="py")["projects"]) == 1

    def test_unmatched_filter_explains_itself(self, fake_store, fake_embedder):
        from tools.project import list_projects

        make_project(fake_store, fake_embedder, "alpha", ["python"])
        out = list_projects(domain="rust")
        assert out["projects"] == []
        assert "compile_project_domains" in out["note"]

    def test_invalid_filter_raises(self):
        from tools.project import list_projects

        with pytest.raises(ValueError, match="Invalid domain filter"):
            list_projects(domain="<bad>")


class TestCompileProjectDomains:
    def test_proposes_without_saving(self, fake_store, fake_embedder):
        from tools.project import compile_project_domains

        make_project(fake_store, fake_embedder, "alpha", stack="Python, Docker")
        out = compile_project_domains("alpha")
        assert out["suggested_domains"] == ["python", "docker"]
        assert "auto_saved" not in out
        assert "domains" not in fake_store.get("mem:project:alpha")
        assert "auto_save=True" in out["note"]

    def test_auto_save_writes(self, fake_store, fake_embedder):
        from tools.project import compile_project_domains

        make_project(fake_store, fake_embedder, "alpha", stack="Python, Docker")
        out = compile_project_domains("alpha", auto_save=True)
        assert out["auto_saved"] is True
        assert fake_store.get("mem:project:alpha")["domains"] == "python,docker"

    def test_evidence_included(self, fake_store, fake_embedder):
        from tools.project import compile_project_domains

        make_project(fake_store, fake_embedder, "alpha", stack="Python")
        assert compile_project_domains("alpha")["evidence"]["python"] == ["stack"]

    def test_missing_project(self):
        from tools.project import compile_project_domains

        assert compile_project_domains("ghost")["status"] == "not_found"

    def test_nothing_to_suggest(self, fake_store, fake_embedder):
        from tools.project import compile_project_domains

        make_project(fake_store, fake_embedder, "alpha", stack="")
        out = compile_project_domains("alpha")
        assert out["suggested_domains"] == []
        assert "Nothing new to suggest" in out["note"]

    def test_auto_save_noop_when_unchanged(self, fake_store, fake_embedder):
        from tools.project import compile_project_domains

        make_project(fake_store, fake_embedder, "alpha", domains=["python"], stack="")
        out = compile_project_domains("alpha", auto_save=True)
        assert "auto_saved" not in out

    def test_reports_the_vocabulary_in_use(self, fake_store, fake_embedder):
        from tools.project import compile_project_domains

        make_project(fake_store, fake_embedder, "alpha", stack="Python")
        make_project(fake_store, fake_embedder, "beta", ["design"])
        assert compile_project_domains("alpha")["domains_in_use"] == {"design": 1}

    def test_invalid_name_raises(self):
        from tools.project import compile_project_domains

        with pytest.raises(ValueError):
            compile_project_domains("bad<name>")


class TestCompileProjectContextDomains:
    def test_draft_carries_domains(self, fake_store, fake_embedder):
        from tools.project import compile_project_context

        make_project(fake_store, fake_embedder, "alpha", stack="Python, Docker")
        out = compile_project_context("alpha")
        assert out["draft"]["domains"] == ["python", "docker"]
        assert out["domain_evidence"]["python"] == ["stack"]

    def test_auto_save_persists_domains(self, fake_store, fake_embedder):
        from tools.project import compile_project_context

        make_project(fake_store, fake_embedder, "alpha", stack="Python")
        compile_project_context("alpha", auto_save=True)
        assert fake_store.get("mem:project:alpha")["domains"] == "python"

    def test_existing_domains_shown(self, fake_store, fake_embedder):
        from tools.project import compile_project_context

        make_project(
            fake_store, fake_embedder, "alpha", domains=["design"], stack="Python"
        )
        out = compile_project_context("alpha")
        assert out["existing_context"]["domains"] == ["design"]


# --- recall tool scoping -----------------------------------------------------


def seed_cross_project(store, embedder):
    make_project(store, embedder, "alpha", ["python"])
    make_project(store, embedder, "beta", ["python"])
    make_project(store, embedder, "gamma", ["design"])
    for name in ("alpha", "beta", "gamma"):
        store_memory(
            store, embedder, f"mem:episodic:{name}",
            "pytest fixtures leak module level state", project=name,
        )


class TestRecallDomainFilter:
    def test_searches_every_project_in_the_domain(self, fake_store, fake_embedder):
        from tools.core import recall

        seed_cross_project(fake_store, fake_embedder)
        results = recall(
            "pytest fixtures leak module level state", top_k=10,
            domain_filter="python",
        )
        assert {r["project"] for r in results} == {"alpha", "beta"}

    def test_alias_resolves(self, fake_store, fake_embedder):
        from tools.core import recall

        seed_cross_project(fake_store, fake_embedder)
        results = recall(
            "pytest fixtures leak module level state", top_k=10, domain_filter="py"
        )
        assert {r["project"] for r in results} == {"alpha", "beta"}

    def test_unmatched_domain_says_so_and_does_not_pretend(
        self, fake_store, fake_embedder
    ):
        from tools.core import recall

        seed_cross_project(fake_store, fake_embedder)
        results = recall(
            "pytest fixtures leak module level state", top_k=10,
            domain_filter="rust",
        )
        notice = results[0]
        assert notice["result_type"] == "domain_filter_notice"
        assert notice["applied"] is False
        assert notice["unmatched_domains"] == ["rust"]
        # The results themselves are unscoped, which is exactly what the
        # notice warns about.
        assert {r.get("project") for r in results[1:]} == {"alpha", "beta", "gamma"}

    def test_partial_match_filters_and_reports(self, fake_store, fake_embedder):
        from tools.core import recall

        seed_cross_project(fake_store, fake_embedder)
        results = recall(
            "pytest fixtures leak module level state", top_k=10,
            domain_filter=["python", "rust"],
        )
        notice = results[0]
        assert notice["applied"] is True
        assert notice["unmatched_domains"] == ["rust"]
        assert {r.get("project") for r in results[1:]} == {"alpha", "beta"}

    def test_no_notice_when_everything_matched(self, fake_store, fake_embedder):
        from tools.core import recall

        seed_cross_project(fake_store, fake_embedder)
        results = recall(
            "pytest fixtures leak module level state", top_k=10,
            domain_filter="python",
        )
        assert all(
            r.get("result_type") != "domain_filter_notice" for r in results
        )

    def test_intersects_with_project_filter(self, fake_store, fake_embedder):
        from tools.core import recall

        seed_cross_project(fake_store, fake_embedder)
        results = recall(
            "pytest fixtures leak module level state", top_k=10,
            project_filter="alpha", domain_filter="python",
        )
        assert {r["project"] for r in results} == {"alpha"}

    def test_project_outside_the_domain_yields_nothing(
        self, fake_store, fake_embedder
    ):
        from tools.core import recall

        seed_cross_project(fake_store, fake_embedder)
        results = recall(
            "pytest fixtures leak module level state", top_k=10,
            project_filter="gamma", domain_filter="python",
        )
        # An empty scope must return nothing, not widen to a global search —
        # the pipeline reads an empty project list as "search everything".
        assert len(results) == 1
        notice = results[0]
        assert notice["result_type"] == "domain_filter_notice"
        assert notice["applied"] is True
        assert notice["projects"] == []
        assert "does not declare" in notice["note"]

    def test_empty_intersection_in_recall_index(self, fake_store, fake_embedder):
        from tools.core import recall_index

        seed_cross_project(fake_store, fake_embedder)
        out = recall_index(
            "pytest fixtures leak module level state", top_k=10,
            project_filter="gamma", domain_filter="python",
        )
        assert out["results"] == []
        assert out["domain_filter"]["projects"] == []

    def test_empty_intersection_across_several_domains(
        self, fake_store, fake_embedder
    ):
        from tools.core import recall

        seed_cross_project(fake_store, fake_embedder)
        results = recall(
            "pytest fixtures leak module level state", top_k=10,
            project_filter="gamma", domain_filter=["python", "docker"],
        )
        assert "any of these domains" in results[0]["note"]

    def test_plain_project_filter_still_works(self, fake_store, fake_embedder):
        from tools.core import recall

        seed_cross_project(fake_store, fake_embedder)
        results = recall(
            "pytest fixtures leak module level state", top_k=10,
            project_filter="alpha",
        )
        assert {r["project"] for r in results} == {"alpha"}

    def test_invalid_project_filter_still_raises(self):
        from tools.core import recall

        with pytest.raises(ValueError, match="invalid characters"):
            recall("anything", project_filter="bad<name>")


class TestRecallIndexDomainFilter:
    def test_reports_applied_filter(self, fake_store, fake_embedder):
        from tools.core import recall_index

        seed_cross_project(fake_store, fake_embedder)
        out = recall_index(
            "pytest fixtures leak module level state", top_k=10,
            domain_filter="python",
        )
        assert out["domain_filter"]["applied"] is True
        assert out["domain_filter"]["projects"] == ["alpha", "beta"]
        assert {r["project"] for r in out["results"]} == {"alpha", "beta"}

    def test_reports_unmatched(self, fake_store, fake_embedder):
        from tools.core import recall_index

        seed_cross_project(fake_store, fake_embedder)
        out = recall_index(
            "pytest fixtures leak module level state", top_k=10,
            domain_filter="rust",
        )
        assert out["domain_filter"]["applied"] is False

    def test_no_key_without_a_domain_filter(self, fake_store, fake_embedder):
        from tools.core import recall_index

        seed_cross_project(fake_store, fake_embedder)
        out = recall_index("pytest fixtures leak module level state", top_k=10)
        assert "domain_filter" not in out


# --- migration ---------------------------------------------------------------


class TestMigrateProjectDomains:
    def test_seeds_from_stack(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", stack="Python, Docker")
        migrate_project_domains(fake_store)
        assert fake_store.get("mem:project:alpha")["domains"] == "python,docker"

    def test_never_overwrites_existing(self, fake_store, fake_embedder):
        make_project(
            fake_store, fake_embedder, "alpha", domains=["design"], stack="Python"
        )
        migrate_project_domains(fake_store)
        assert fake_store.get("mem:project:alpha")["domains"] == "design"

    def test_empty_marker_stops_a_rescan(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", stack="")
        migrate_project_domains(fake_store)
        assert fake_store.get("mem:project:alpha")["domains"] == ""
        # Second pass leaves the marker alone rather than re-deriving.
        fake_store.set_field("mem:project:alpha", "stack", "Python")
        migrate_project_domains(fake_store)
        assert fake_store.get("mem:project:alpha")["domains"] == ""

    def test_skips_records_with_neither_stack_nor_goals(
        self, fake_store, fake_embedder
    ):
        # Only real context entries carry a stack or goals — a record with
        # both blank isn't a project and must not grow a domains field.
        make_project(fake_store, fake_embedder, "alpha", stack="", goals="")
        migrate_project_domains(fake_store)
        assert "domains" not in fake_store.get("mem:project:alpha")

    def test_skips_ulid_project_memories(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:project:01ABC", "a loose note",
            namespace="project", project="alpha",
        )
        migrate_project_domains(fake_store)
        assert "domains" not in fake_store.get("mem:project:01ABC")

    def test_drops_filler_words(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", stack="Python and Docker")
        migrate_project_domains(fake_store)
        assert fake_store.get("mem:project:alpha")["domains"] == "python,docker"

    def test_empty_store(self, fake_store):
        migrate_project_domains(fake_store)

    def test_idempotent(self, fake_store, fake_embedder):
        make_project(fake_store, fake_embedder, "alpha", stack="Python")
        migrate_project_domains(fake_store)
        migrate_project_domains(fake_store)
        assert fake_store.get("mem:project:alpha")["domains"] == "python"

    def test_makes_the_domain_filter_work_after_an_upgrade(
        self, fake_store, fake_embedder
    ):
        # The point of the migration: not empty on day one.
        make_project(fake_store, fake_embedder, "alpha", stack="Python")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:a", "a python lesson",
            project="alpha",
        )
        migrate_project_domains(fake_store)
        pd.invalidate_domain_cache()
        assert pd.resolve_projects_for_domains(fake_store, "python").projects == [
            "alpha"
        ]


# --- skill scan wiring -------------------------------------------------------


class TestSkillScanProjectBoost:
    def test_declared_domains_rank_higher(self, fake_store, fake_embedder):
        from memory.skill_scan import _boost_by_project_domains

        make_project(fake_store, fake_embedder, "alpha", ["python"])
        make_project(fake_store, fake_embedder, "beta", ["python"])
        counts = {"python": 3, "css": 5}
        _boost_by_project_domains(fake_store, counts)
        assert counts["python"] == 7  # 3 + 2 projects x weight 2
        assert counts["css"] == 5
        assert sorted(counts, key=lambda d: -counts[d])[0] == "python"

    def test_never_introduces_a_domain_with_no_pool(
        self, fake_store, fake_embedder
    ):
        from memory.skill_scan import _boost_by_project_domains

        make_project(fake_store, fake_embedder, "alpha", ["docker"])
        counts = {"python": 1}
        _boost_by_project_domains(fake_store, counts)
        assert "docker" not in counts
