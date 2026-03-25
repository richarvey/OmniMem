"""Tests for project context tools: set, get, list, update, compile."""

import json
import time

import pytest

from tests.conftest import store_memory

import tools as tools_module


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


from tools.project import (
    _validate_project_name,
    compile_project_context,
    get_project_context,
    list_projects,
    set_project_context,
    update_project_state,
)


class TestValidateProjectName:
    def test_valid_simple_name(self):
        _validate_project_name("my-project")

    def test_valid_with_spaces_and_dots(self):
        _validate_project_name("my project.v2")

    def test_empty_raises(self):
        with pytest.raises(ValueError, match="1-200 characters"):
            _validate_project_name("")

    def test_too_long_raises(self):
        with pytest.raises(ValueError, match="1-200 characters"):
            _validate_project_name("x" * 201)

    def test_invalid_chars_raises(self):
        with pytest.raises(ValueError, match="invalid characters"):
            _validate_project_name("proj<script>")


class TestSetProjectContext:
    def test_basic_set(self):
        result = set_project_context(
            "testproj", "A test project", "python", "ship v1", "starting"
        )
        assert result["project_name"] == "testproj"

    def test_stores_all_fields(self, fake_store):
        set_project_context(
            "testproj", "A test project", "python,docker",
            "ship v1", "in progress", notes="check CI"
        )
        data = fake_store.get("mem:project:testproj")
        assert data["description"] == "A test project"
        assert data["stack"] == "python,docker"
        assert data["goals"] == "ship v1"
        assert data["current_state"] == "in progress"
        assert data["notes"] == "check CI"
        assert data["state"] == "active"

    def test_without_notes(self, fake_store):
        set_project_context("testproj", "desc", "py", "goals", "state")
        data = fake_store.get("mem:project:testproj")
        assert "notes" not in data

    def test_overwrite_existing(self, fake_store):
        set_project_context("testproj", "v1", "py", "goals", "state")
        set_project_context("testproj", "v2", "py", "goals", "state")
        data = fake_store.get("mem:project:testproj")
        assert data["description"] == "v2"


class TestGetProjectContext:
    def test_found(self):
        set_project_context("testproj", "desc", "py", "goals", "state")
        result = get_project_context("testproj")
        assert result["status"] == "found"
        assert result["project_name"] == "testproj"
        assert result["description"] == "desc"

    def test_not_found(self):
        result = get_project_context("nonexistent")
        assert result["status"] == "not_found"

    def test_returns_all_fields(self):
        set_project_context(
            "testproj", "desc", "py,docker", "ship it", "wip", notes="hello"
        )
        result = get_project_context("testproj")
        assert result["stack"] == "py,docker"
        assert result["goals"] == "ship it"
        assert result["current_state"] == "wip"
        assert result["notes"] == "hello"


class TestListProjects:
    def test_empty(self):
        result = list_projects()
        assert result["projects"] == []

    def test_single_project(self):
        set_project_context("alpha", "desc", "py", "goals", "state")
        result = list_projects()
        assert len(result["projects"]) == 1
        assert result["projects"][0]["project_name"] == "alpha"

    def test_multiple_sorted(self):
        set_project_context("zulu", "desc", "py", "goals", "state")
        set_project_context("alpha", "desc", "py", "goals", "state")
        result = list_projects()
        names = [p["project_name"] for p in result["projects"]]
        assert names == ["alpha", "zulu"]

    def test_dedup_with_ulid_memories(self, fake_store, fake_embedder):
        set_project_context("testproj", "desc", "py", "goals", "state")
        # Simulate a ULID-keyed project memory (old format)
        store_memory(
            fake_store, fake_embedder,
            "mem:project:01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "some decision",
            namespace="project",
            project="testproj",
        )
        result = list_projects()
        # Should still show as one project, not two
        proj_names = [p["project_name"] for p in result["projects"]]
        assert proj_names.count("testproj") == 1


class TestUpdateProjectState:
    def test_basic_update(self):
        set_project_context("testproj", "desc", "py", "goals", "starting")
        result = update_project_state("testproj", "halfway done")
        assert result["project_name"] == "testproj"

    def test_state_changed(self, fake_store):
        set_project_context("testproj", "desc", "py", "goals", "starting")
        update_project_state("testproj", "halfway done")
        data = fake_store.get("mem:project:testproj")
        assert data["current_state"] == "halfway done"

    def test_with_notes(self, fake_store):
        set_project_context("testproj", "desc", "py", "goals", "starting")
        update_project_state("testproj", "done", notes="ship it")
        data = fake_store.get("mem:project:testproj")
        assert data["notes"] == "ship it"

    def test_not_found(self):
        result = update_project_state("nonexistent", "new state")
        assert result["status"] == "not_found"

    def test_preserves_other_fields(self, fake_store):
        set_project_context("testproj", "desc", "py,docker", "goals", "starting")
        update_project_state("testproj", "done")
        data = fake_store.get("mem:project:testproj")
        assert data["description"] == "desc"
        assert data["stack"] == "py,docker"
        assert data["goals"] == "goals"


class TestCompileProjectContext:
    def test_no_memories(self):
        result = compile_project_context("emptyproj")
        assert result["memory_count"] == 0

    def test_with_memories(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:episodic:01A", "Built the login page",
            project="testproj", tags=["frontend", "react"],
        )
        result = compile_project_context("testproj")
        assert result["memory_count"] == 1
        assert result["memories"][0]["content"] == "Built the login page"

    def test_collects_tags(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:episodic:01A", "content",
            project="testproj", tags=["python", "docker"],
        )
        result = compile_project_context("testproj")
        assert "python" in result["top_tags"]
        assert "docker" in result["top_tags"]

    def test_collects_breakthroughs_and_gotchas(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:episodic:01A", "content",
            project="testproj",
            breakthrough="mtime polling works",
            gotchas="needs openblas",
        )
        result = compile_project_context("testproj")
        assert "mtime polling works" in result["breakthroughs"]
        assert "needs openblas" in result["gotchas"]

    def test_collects_abandoned(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:episodic:01A", "content",
            project="testproj",
            abandoned_approaches=[{"name": "Alpine", "reason": "no PyTorch wheels"}],
        )
        result = compile_project_context("testproj")
        assert len(result["abandoned_approaches"]) == 1
        assert result["abandoned_approaches"][0]["name"] == "Alpine"

    def test_auto_save(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:episodic:01A", "Built the API",
            project="testproj",
        )
        result = compile_project_context("testproj", auto_save=True)
        assert result.get("auto_saved") is True
        # Verify context was saved
        data = fake_store.get("mem:project:testproj")
        assert data is not None
        assert data["project_name"] == "testproj"

    def test_skips_non_active(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:episodic:01A", "active memory",
            project="testproj",
        )
        store_memory(
            fake_store, fake_embedder,
            "mem:episodic:01B", "archived memory",
            project="testproj", state="archived",
        )
        result = compile_project_context("testproj")
        assert result["memory_count"] == 1

    def test_filters_by_project(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:episodic:01A", "proj A memory",
            project="projA",
        )
        store_memory(
            fake_store, fake_embedder,
            "mem:episodic:01B", "proj B memory",
            project="projB",
        )
        result = compile_project_context("projA")
        assert result["memory_count"] == 1
