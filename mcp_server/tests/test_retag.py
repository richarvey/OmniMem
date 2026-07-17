"""Tests for tag editing: memory.tags shared helper, the retag MCP tool, and the web UI path."""

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from tests.conftest import store_memory

import tools as tools_module

from memory.tags import MAX_TAGS, parse_tags_field, retag_memory


@pytest.fixture(autouse=True)
def inject_deps(fake_store, fake_embedder, lifecycle, pipeline):
    """Inject fake dependencies into the tools module for every test."""
    tools_module._store = fake_store
    tools_module._embedder = fake_embedder
    tools_module._lifecycle = lifecycle
    tools_module._pipeline = pipeline
    yield
    tools_module._store = None
    tools_module._embedder = None
    tools_module._lifecycle = None
    tools_module._pipeline = None


from tools.core import retag


def _tags_of(store, key):
    return parse_tags_field(store.get(key).get("tags"))


class TestRetagMemory:
    def test_replace(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t1",
                           "Fixed the docker build", tags=["docker", "bugfix"])
        result = retag_memory(fake_store, key, tags=["docker", "bug-fix", "working"])
        assert result["status"] == "updated"
        assert result["previous_tags"] == ["docker", "bugfix"]
        assert _tags_of(fake_store, key) == ["docker", "bug-fix", "working"]

    def test_replace_with_empty_list_clears(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t2",
                           "A memory", tags=["stale"])
        result = retag_memory(fake_store, key, tags=[])
        assert result["status"] == "updated"
        assert _tags_of(fake_store, key) == []

    def test_add_preserves_existing_and_dedupes(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t3",
                           "A memory", tags=["python"])
        result = retag_memory(fake_store, key, add=["python", "gotcha", "gotcha"])
        assert result["status"] == "updated"
        assert _tags_of(fake_store, key) == ["python", "gotcha"]

    def test_add_to_untagged_memory(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:knowledge:t4", "A fact")
        result = retag_memory(fake_store, key, add=["rust", "pattern"])
        assert result["status"] == "updated"
        assert _tags_of(fake_store, key) == ["rust", "pattern"]

    def test_remove(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t5",
                           "A memory", tags=["python", "deprecated", "gotcha"])
        result = retag_memory(fake_store, key, remove=["deprecated"])
        assert result["status"] == "updated"
        assert _tags_of(fake_store, key) == ["python", "gotcha"]

    def test_add_and_remove_together(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t6",
                           "A memory", tags=["revisit"])
        result = retag_memory(fake_store, key, add=["working"], remove=["revisit"])
        assert result["status"] == "updated"
        assert _tags_of(fake_store, key) == ["working"]

    def test_unchanged_when_no_effective_change(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t7",
                           "A memory", tags=["python"])
        result = retag_memory(fake_store, key, add=["python"])
        assert result["status"] == "unchanged"

    def test_unchanged_does_not_bump_updated_at(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t8",
                           "A memory", tags=["python"])
        before = fake_store.get(key)["updated_at"]
        retag_memory(fake_store, key, tags=["python"])
        assert fake_store.get(key)["updated_at"] == before

    def test_update_bumps_updated_at(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t9",
                           "A memory", tags=["python"])
        before = float(fake_store.get(key)["updated_at"])
        retag_memory(fake_store, key, add=["docker"])
        assert float(fake_store.get(key)["updated_at"]) >= before

    def test_not_found(self, fake_store):
        result = retag_memory(fake_store, "mem:episodic:missing", tags=["x"])
        assert result["status"] == "not_found"

    def test_whitespace_stripped_and_empties_dropped(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t10", "A memory")
        result = retag_memory(fake_store, key, tags=["  python ", "", "  "])
        assert result["tags"] == ["python"]

    def test_rejects_skill_keys(self, fake_store):
        with pytest.raises(ValueError, match="skill"):
            retag_memory(fake_store, "mem:skill:gen:python-local", tags=["x"])

    def test_rejects_non_mem_keys(self, fake_store):
        with pytest.raises(ValueError, match="Invalid memory key"):
            retag_memory(fake_store, "meta:maintenance:omnimem", tags=["x"])

    def test_rejects_replace_combined_with_add(self, fake_store):
        with pytest.raises(ValueError, match="not both"):
            retag_memory(fake_store, "mem:episodic:t", tags=["a"], add=["b"])

    def test_rejects_no_arguments(self, fake_store):
        with pytest.raises(ValueError, match="Nothing to do"):
            retag_memory(fake_store, "mem:episodic:t")

    def test_rejects_too_many_tags(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t11", "A memory")
        with pytest.raises(ValueError, match="Too many tags"):
            retag_memory(fake_store, key, tags=[f"t{i}" for i in range(MAX_TAGS + 1)])

    def test_rejects_overflow_via_add(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t12", "A memory",
                           tags=[f"t{i}" for i in range(MAX_TAGS)])
        with pytest.raises(ValueError, match="Too many tags"):
            retag_memory(fake_store, key, add=["one-more"])

    def test_rejects_non_string_tag(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t13", "A memory")
        with pytest.raises(ValueError, match="must be a string"):
            retag_memory(fake_store, key, tags=[123])

    def test_tolerates_malformed_stored_tags(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t14", "A memory")
        fake_store.set_fields(key, {"tags": "not json"})
        result = retag_memory(fake_store, key, add=["python"])
        assert result["status"] == "updated"
        assert result["previous_tags"] == []
        assert _tags_of(fake_store, key) == ["python"]


class TestRetagTool:
    def test_tool_updates_tags(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:tool1",
                           "A memory", tags=["old"])
        result = retag(key, tags=["fresh", "working"])
        assert result["status"] == "updated"
        assert json.loads(fake_store.get(key)["tags"]) == ["fresh", "working"]

    def test_tool_add_remove(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:preference:tool2",
                           "Always update the README", tags=["docs"])
        result = retag(key, add=["preference"], remove=["docs"])
        assert result["tags"] == ["preference"]


class TestWebRetag:
    def test_detail_template_renders_tag_form(self):
        from jinja2 import Environment, FileSystemLoader

        template_dir = Path(__file__).resolve().parent.parent.parent / "web_ui" / "templates"
        env = Environment(loader=FileSystemLoader(str(template_dir)), autoescape=True)
        env.globals["version"] = "0.0.0-test"
        html = env.get_template("detail.html").render(
            current_page="memories",
            tag_error="",
            memory={
                "key": "mem:episodic:x", "namespace": "episodic", "content": "hello",
                "state": "active", "project": "", "tags": ["python", "gotcha"],
                "surface_score": "1.0", "experience_weight": "1.0",
                "effort_score": None, "outcome": None, "iterations": None,
                "breakthrough": None, "gotchas": None, "abandoned_approaches": [],
                "contradictions": [], "reinstate_hints": [],
                "deprioritised_reason": "", "source_url": "", "feed_name": "",
                "recall_count": 0, "last_recalled": "Never",
                "created_at": "2026-07-11", "updated_at": "2026-07-11",
            },
        )
        assert 'action="/memory/mem:episodic:x/tags"' in html
        assert 'value="python, gotcha"' in html
        assert "Save tags" in html

    def test_detail_template_hides_form_for_skills(self):
        from jinja2 import Environment, FileSystemLoader

        template_dir = Path(__file__).resolve().parent.parent.parent / "web_ui" / "templates"
        env = Environment(loader=FileSystemLoader(str(template_dir)), autoescape=True)
        env.globals["version"] = "0.0.0-test"
        html = env.get_template("detail.html").render(
            current_page="memories",
            tag_error="",
            memory={
                "key": "mem:skill:gen:python-local", "namespace": "skill", "content": "body",
                "state": "active", "project": "", "tags": ["python"],
                "surface_score": "1.0", "experience_weight": "1.0",
                "effort_score": None, "outcome": None, "iterations": None,
                "breakthrough": None, "gotchas": None, "abandoned_approaches": [],
                "contradictions": [], "reinstate_hints": [],
                "deprioritised_reason": "", "source_url": "", "feed_name": "",
                "recall_count": 0, "last_recalled": "Never",
                "created_at": "2026-07-11", "updated_at": "2026-07-11",
            },
        )
        assert "Save tags" not in html
        assert "python" in html
