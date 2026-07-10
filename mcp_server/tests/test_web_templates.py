"""Render smoke tests for the web UI templates touched by the v6 UI rework.

No HTTP layer — just the Jinja environment the app builds, with plausible
context. Catches template syntax errors and missing-key regressions that
route-level unit tests (which never render) would miss.
"""

import sys
from pathlib import Path

import pytest
from jinja2 import Environment, FileSystemLoader

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

_TEMPLATE_DIR = Path(__file__).resolve().parent.parent.parent / "web_ui" / "templates"


@pytest.fixture
def env():
    environment = Environment(
        loader=FileSystemLoader(str(_TEMPLATE_DIR)),
        autoescape=True,
    )
    environment.globals["version"] = "0.0.0-test"
    return environment


def _skill_row(**overrides):
    row = {
        "key": "mem:skill:gen:python-local",
        "name": "python-local",
        "description": "Distilled python procedure",
        "domain": "python",
        "state": "active",
        "generated": True,
        "compiled_at": "2026-07-10 12:00",
        "recall_count": 3,
    }
    row.update(overrides)
    return row


class TestDashboardTemplate:
    def test_renders_projects_and_skills_cards(self, env):
        html = env.get_template("dashboard.html").render(
            current_page="dashboard",
            ns_stats={
                "episodic": {"total": 5, "states": {"active": 4, "deprioritised": 1, "archived": 0}},
                "project": {
                    "total": 3, "distinct": 2,
                    "states": {"active": 2, "deprioritised": 1, "archived": 0},
                    "projects": {"active": 1, "deprioritised": 1, "archived": 0},
                },
                "knowledge": {"total": 0, "states": {"active": 0, "deprioritised": 0, "archived": 0}},
                "preference": {"total": 1, "states": {"active": 1, "deprioritised": 0, "archived": 0}},
            },
            total=9,
            skills={
                "total": 2,
                "states": {"active": 2, "deprioritised": 0, "archived": 0},
                "proposals": 1,
            },
            recent=[
                {"key": "mem:episodic:01A", "namespace": "episodic", "state": "active",
                 "content": "a memory", "project": "omnimem", "updated_at_fmt": "2026-07-10 12:00"},
                {"key": "mem:skill:gen:python-local", "namespace": "skill", "state": "active",
                 "content": "python-local — distilled procedure", "project": "",
                 "updated_at_fmt": "2026-07-10 12:01"},
            ],
            stats_age=12,
            health={"valkey": True, "model": True},
            enrichment_pending=0,
        )
        assert "projects" in html
        assert "1 proposal pending" in html
        assert "/skills/mem:skill:gen:python-local" in html
        assert "/memory/mem:episodic:01A" in html
        # Per-project state breakdown, not record states, on the projects card
        assert "1 active" in html


class TestSkillsTemplates:
    def test_list_renders_rows_and_proposals(self, env):
        html = env.get_template("skills/list.html").render(
            current_page="skills",
            total=2,
            states={"active": 1, "deprioritised": 0, "archived": 1},
            proposals=[{"key": "meta:skill:proposal:rust-local", "domain": "rust",
                        "created_at": "2026-07-10 11:00"}],
            skills=[_skill_row(), _skill_row(key="mem:skill:gen:ansible-local",
                                             name="ansible-local", domain="ansible",
                                             state="archived", generated=False)],
        )
        assert "python-local" in html
        assert "ansible-local" in html
        assert "pending proposal" in html
        assert "/skills/mem:skill:gen:python-local" in html

    def test_list_empty_state(self, env):
        html = env.get_template("skills/list.html").render(
            current_page="skills", total=0,
            states={"active": 0, "deprioritised": 0, "archived": 0},
            proposals=[], skills=[],
        )
        assert "No skills compiled yet" in html

    def test_detail_renders_rules_body_and_sources(self, env):
        html = env.get_template("skills/detail.html").render(
            current_page="skills",
            skill={
                "key": "mem:skill:gen:python-local",
                "name": "python-local",
                "description": "Distilled python procedure",
                "domain": "python",
                "user": "local",
                "state": "active",
                "generated": True,
                "contract_version": "1",
                "compiled_at": "2026-07-10 12:00:00",
                "created_at": "2026-07-10 12:00:00",
                "updated_at": "2026-07-10 12:00:00",
                "recall_count": 3,
                "last_recalled": "Never",
                "body": "---\nname: python-local\n---\n\n## Do\n\n- Use uv.\n",
                "sources": ["mem:episodic:01A"],
                "rules": [
                    {"kind": "do", "text": "Use uv for deps",
                     "sources": ["mem:episodic:01A"], "reinforcement": 2},
                    {"kind": "dont", "text": "hangs on startup", "name": "singleton loader",
                     "sources": ["mem:episodic:01B"], "reinforcement": 1, "blessed": True},
                ],
                "rule_counts": {"do": 1, "watch": 0, "dont": 1},
            },
        )
        assert "SKILL.md" in html
        assert "Avoid singleton loader" in html
        assert "blessed" in html
        assert "/memory/mem:episodic:01A" in html


class TestNavigation:
    def test_sidebar_groups_present(self, env):
        html = env.get_template("base.html").render(current_page="dashboard")
        for label in ("Memory", "Skills", "Management", "Knowledge Management", "System Management"):
            assert label in html
        for href in ("/skills", "/experience/graveyard", "/memories?namespace=preference",
                     "/memories?namespace=knowledge", "/telemetry", "/backups"):
            assert href in html

    def test_active_states_highlight_filtered_views(self, env):
        html = env.get_template("base.html").render(current_page="preferences")
        assert 'href="/memories?namespace=preference" class="nav-link active"' in html
        html = env.get_template("base.html").render(current_page="graveyard")
        assert 'href="/experience/graveyard" class="nav-link active"' in html


class TestTelemetryPartial:
    def test_entries_link_via_url(self, env):
        entry = {
            "key": "mem:skill:gen:python-local", "namespace": "skill",
            "url": "/skills/mem:skill:gen:python-local",
            "content": "python-local — distilled procedure", "project": "",
            "recall_count": 5, "last_recalled": "2026-07-10 12:00",
            "last_recalled_raw": 1.0, "created_at_raw": 1.0,
        }
        html = env.get_template("partials/telemetry_content.html").render(
            total_memories=1, total_recalls=5, unique_recalled=1, never_recalled=0,
            most_recalled=[entry], gone_cold=[], never_recalled_list=[],
            cold_days=60, project_filter="",
        )
        assert 'href="/skills/mem:skill:gen:python-local"' in html
