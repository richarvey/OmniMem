"""Tests for the auto skill scan: discovery, noise gate, briefing surface."""

import time

import pytest

from tests.conftest import store_memory

import tools as tools_module

from memory.skill_compiler import compile_skill_flow, proposal_key
from memory.skill_scan import _LAST_RUN_KEY, run_skill_scan, scan_due


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


def _seed_domain(
    fake_store, fake_embedder, domain="deploy", *,
    projects=("alpha", "beta"), breakthrough="pin buildx to the multiarch builder",
):
    """Three lesson-bearing memories: a reinforced cross-project pattern
    (identical breakthroughs cluster at similarity 1.0) plus one gotcha."""
    store_memory(
        fake_store, fake_embedder, f"mem:episodic:01{domain[:3].upper()}A",
        f"{domain} lesson one", tags=[domain], project=projects[0],
        breakthrough=breakthrough, outcome="succeeded",
    )
    store_memory(
        fake_store, fake_embedder, f"mem:episodic:01{domain[:3].upper()}B",
        f"{domain} lesson two", tags=[domain],
        project=projects[1 % len(projects)],
        breakthrough=breakthrough, outcome="succeeded",
    )
    store_memory(
        fake_store, fake_embedder, f"mem:episodic:01{domain[:3].upper()}C",
        f"{domain} lesson three", tags=[domain], project=projects[0],
        gotchas="cache mounts don't survive builder restarts",
    )


class TestScanDue:
    def test_due_on_fresh_store(self, fake_store):
        assert scan_due(fake_store) is True

    def test_not_due_after_a_run(self, fake_store, fake_embedder):
        run_skill_scan(fake_store, fake_embedder)
        assert scan_due(fake_store) is False

    def test_due_again_after_interval(self, fake_store, fake_embedder):
        run_skill_scan(fake_store, fake_embedder)
        fake_store.client.set(_LAST_RUN_KEY, str(time.time() - 25 * 3600))
        assert scan_due(fake_store) is True

    def test_interval_zero_disables(self, fake_store, monkeypatch):
        monkeypatch.setenv("SKILL_SCAN_INTERVAL_HOURS", "0")
        assert scan_due(fake_store) is False

    def test_garbage_values_fail_open(self, fake_store, monkeypatch):
        fake_store.client.set(_LAST_RUN_KEY, "not a timestamp")
        assert scan_due(fake_store) is True
        monkeypatch.setenv("SKILL_SCAN_INTERVAL_HOURS", "not a number")
        assert scan_due(fake_store) is True


class TestNewSkillDiscovery:
    def test_cross_project_pattern_earns_a_proposal(self, fake_store, fake_embedder):
        _seed_domain(fake_store, fake_embedder)
        result = run_skill_scan(fake_store, fake_embedder)

        assert len(result["proposals"]) == 1
        entry = result["proposals"][0]
        assert entry["domain"] == "deploy"
        assert entry["new_skill"] is True
        assert entry["skill_id"] == "mem:skill:gen:deploy-local"
        assert "compile_skill" in entry["review"]
        # The stash is exactly what a manual propose would leave behind.
        stash = fake_store.get(proposal_key("deploy", "local"))
        assert stash and stash["body"].startswith("---")

    def test_single_project_pattern_held_back_by_default(
        self, fake_store, fake_embedder,
    ):
        _seed_domain(fake_store, fake_embedder, projects=("alpha",))
        result = run_skill_scan(fake_store, fake_embedder)
        assert result["proposals"] == []
        assert fake_store.get(proposal_key("deploy", "local")) is None

    def test_cross_project_gate_can_be_disabled(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        monkeypatch.setenv("SKILL_SCAN_CROSS_PROJECT", "false")
        _seed_domain(fake_store, fake_embedder, projects=("alpha",))
        result = run_skill_scan(fake_store, fake_embedder)
        assert len(result["proposals"]) == 1

    def test_small_pools_are_not_even_checked(self, fake_store, fake_embedder):
        # Two lesson-bearing memories < SKILL_SCAN_MIN_POOL (3).
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01TFA", "tf one",
            tags=["terraform"], project="alpha",
            breakthrough="keep state remote", outcome="succeeded",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01TFB", "tf two",
            tags=["terraform"], project="beta",
            breakthrough="keep state remote", outcome="succeeded",
        )
        result = run_skill_scan(fake_store, fake_embedder)
        assert result["proposals"] == []
        assert result["new_skill_candidates_checked"] == 0

    def test_min_pool_is_tunable(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("SKILL_SCAN_MIN_POOL", "2")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01TFA", "tf one",
            tags=["terraform"], project="alpha",
            breakthrough="keep state remote", outcome="succeeded",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01TFB", "tf two",
            tags=["terraform"], project="beta",
            breakthrough="keep state remote", outcome="succeeded",
        )
        result = run_skill_scan(fake_store, fake_embedder)
        assert len(result["proposals"]) == 1

    def test_domains_with_a_skill_or_pending_proposal_skipped(
        self, fake_store, fake_embedder,
    ):
        _seed_domain(fake_store, fake_embedder, domain="deploy")
        _seed_domain(fake_store, fake_embedder, domain="ansible")

        # deploy already has a compiled skill; ansible has a live proposal.
        fake_store.upsert("skill", "mem:skill:gen:deploy-local", {
            "domain": "deploy", "name": "deploy-local", "state": "active",
            "generated": "true", "body": "---\n---\n",
        }, fake_embedder.embed("deploy"))
        fake_store.client.hset(proposal_key("ansible", "local"), mapping={
            "domain": "ansible", "user": "local", "body": "draft",
        })

        result = run_skill_scan(fake_store, fake_embedder)
        assert result["proposals"] == []

    def test_max_proposals_cap(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("SKILL_SCAN_MAX_PROPOSALS", "1")
        _seed_domain(fake_store, fake_embedder, domain="deploy")
        _seed_domain(fake_store, fake_embedder, domain="ansible",
                     breakthrough="always gather facts once")
        result = run_skill_scan(fake_store, fake_embedder)
        assert len(result["proposals"]) == 1


class TestNoiseGate:
    def test_ignored_draft_is_not_reproposed(self, fake_store, fake_embedder):
        _seed_domain(fake_store, fake_embedder)
        first = run_skill_scan(fake_store, fake_embedder)
        assert len(first["proposals"]) == 1

        # Human ignores it: the stash expires; the interval elapses.
        fake_store.delete(proposal_key("deploy", "local"))
        fake_store.client.delete(_LAST_RUN_KEY)

        second = run_skill_scan(fake_store, fake_embedder)
        assert second["proposals"] == []
        # And the scan withdrew its own re-created stash.
        assert fake_store.get(proposal_key("deploy", "local")) is None

    def test_new_lessons_repropose(self, fake_store, fake_embedder):
        _seed_domain(fake_store, fake_embedder)
        run_skill_scan(fake_store, fake_embedder)
        fake_store.delete(proposal_key("deploy", "local"))
        fake_store.client.delete(_LAST_RUN_KEY)

        # The pool changes, so the compiled output changes too.
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01DEPD", "deploy four",
            tags=["deploy"], project="gamma",
            breakthrough="pin buildx to the multiarch builder",
            outcome="succeeded",
        )
        result = run_skill_scan(fake_store, fake_embedder)
        assert len(result["proposals"]) == 1


class TestUpdateProposals:
    def _compile_skill(self, fake_store, fake_embedder, domain="deploy"):
        assert compile_skill_flow(
            fake_store, fake_embedder, domain, mode="propose",
        )["status"] == "proposal"
        assert compile_skill_flow(
            fake_store, fake_embedder, domain, mode="write",
        )["status"] == "written"

    def test_changed_skill_gets_a_draft(self, fake_store, fake_embedder):
        _seed_domain(fake_store, fake_embedder)
        self._compile_skill(fake_store, fake_embedder)

        # The lesson is rewritten after the compile (in both memories, so
        # the rule keeps its reinforcement but its wording changes).
        for key in ("mem:episodic:01DEPA", "mem:episodic:01DEPB"):
            fake_store.set_fields(key, {
                "breakthrough": "pin buildx to the multiarch builder always",
                "updated_at": str(time.time() + 1),
            })

        result = run_skill_scan(
            fake_store, fake_embedder, update_domains=["deploy"],
        )
        assert len(result["proposals"]) == 1
        entry = result["proposals"][0]
        assert entry["new_skill"] is False
        assert entry["changes"]  # risk-classified summary travels with it

    def test_unchanged_skill_stays_quiet(self, fake_store, fake_embedder):
        _seed_domain(fake_store, fake_embedder)
        self._compile_skill(fake_store, fake_embedder)
        result = run_skill_scan(
            fake_store, fake_embedder, update_domains=["deploy"],
        )
        assert result["proposals"] == []

    def test_unknown_update_domain_ignored(self, fake_store, fake_embedder):
        result = run_skill_scan(
            fake_store, fake_embedder, update_domains=["nonexistent"],
        )
        assert result["proposals"] == []


class TestBriefingSurface:
    def test_briefing_carries_auto_proposals(self, fake_store, fake_embedder):
        from tools.briefing import briefing

        _seed_domain(fake_store, fake_embedder)
        result = briefing(project="alpha")

        section = result["auto_proposed_skills"]
        assert section["proposals"][0]["domain"] == "deploy"
        assert "reviews and accepts" in section["note"]
        assert fake_store.get(proposal_key("deploy", "local")) is not None

    def test_briefing_respects_time_gate(self, fake_store, fake_embedder):
        from tools.briefing import briefing

        _seed_domain(fake_store, fake_embedder)
        fake_store.client.set(_LAST_RUN_KEY, str(time.time()))
        result = briefing(project="alpha")
        assert "auto_proposed_skills" not in result

    def test_briefing_scan_disabled_by_env(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.briefing import briefing

        monkeypatch.setenv("SKILL_SCAN_INTERVAL_HOURS", "0")
        _seed_domain(fake_store, fake_embedder)
        result = briefing(project="alpha")
        assert "auto_proposed_skills" not in result

    def test_briefing_omits_section_when_nothing_proposed(
        self, fake_store, fake_embedder,
    ):
        from tools.briefing import briefing

        result = briefing(project="alpha")
        assert "auto_proposed_skills" not in result
        # The gate still stamps, so quiet scans don't repeat every briefing.
        assert fake_store.client.get(_LAST_RUN_KEY) is not None
