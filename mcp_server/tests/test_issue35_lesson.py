"""Issue #35: record_experience() separates the generalisable lesson from the
record of what happened, and the skill compiler prefers the lesson.

The field is additive: a memory without a lesson must compile exactly as it
did before, and a lesson must pass the same outcome gate a breakthrough does.
"""

import pytest

from tests.conftest import store_memory

import tools as tools_module
from memory import skills as sk


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


from tools.audit import explain_memory
from tools.core import recall, recall_detail, remember
from tools.experience import get_experience, record_experience

LESSON = "a test fake must reproduce the real server behaviour, not the docs"
NARRATIVE = "made the fake dropindex raise the way valkey-search does"


def _pool_mem(**overrides):
    mem = {
        "key": "mem:episodic:01A", "content": "c", "blessed": False,
        "updated_at": 1.0, "project": "p", "outcome": "succeeded",
        "breakthrough": None, "lesson": None, "gotchas": None,
        "abandoned": [],
    }
    mem.update(overrides)
    return mem


def _memory_with_lesson(fake_store, fake_embedder, key, content, **kwargs):
    store_memory(
        fake_store, fake_embedder, key, content,
        effort_score=4, outcome="succeeded", breakthrough=NARRATIVE, **kwargs,
    )
    fake_store.set_field(key, "lesson", LESSON)
    return key


class TestRecordExperience:
    def test_lesson_is_stored_beside_the_breakthrough(self, fake_store):
        key = remember("fixed the index migration that never ran")["key"]
        record_experience(
            key, effort_score=4, outcome="succeeded",
            breakthrough=NARRATIVE, lesson=LESSON,
        )
        data = fake_store.get(key)
        assert data["lesson"] == LESSON
        assert data["breakthrough"] == NARRATIVE

    def test_lesson_is_optional(self, fake_store):
        key = remember("a routine change that taught nothing new")["key"]
        record_experience(
            key, effort_score=1, outcome="succeeded", breakthrough=NARRATIVE,
        )
        assert "lesson" not in fake_store.get(key)

    def test_get_experience_returns_the_lesson(self):
        key = remember("work with a transferable lesson")["key"]
        record_experience(key, effort_score=3, outcome="succeeded", lesson=LESSON)
        assert get_experience(key)["lesson"] == LESSON


class TestReadSurfaces:
    def test_recall_returns_the_lesson(self, fake_store, fake_embedder):
        key = _memory_with_lesson(
            fake_store, fake_embedder, "mem:episodic:les01",
            "index migration silently failed on every upgrade",
        )
        results = recall("index migration silently failed on every upgrade")
        matched = [r for r in results if r.get("key") == key]
        assert matched, "the memory should match its own content"
        assert matched[0]["lesson"] == LESSON

    def test_recall_detail_returns_the_lesson(self, fake_store, fake_embedder):
        key = _memory_with_lesson(
            fake_store, fake_embedder, "mem:episodic:les02", "detail memory",
        )
        assert recall_detail([key])[0]["lesson"] == LESSON

    def test_explain_memory_returns_the_lesson(self, fake_store, fake_embedder):
        key = _memory_with_lesson(
            fake_store, fake_embedder, "mem:episodic:les03", "explained memory",
        )
        assert explain_memory(key)["lesson"] == LESSON

    def test_domain_pool_projection_carries_the_lesson(
        self, fake_store, fake_embedder,
    ):
        """The pool reads a fixed field projection; a field missing from it
        silently reads as None and the compiler would never see a lesson."""
        _memory_with_lesson(
            fake_store, fake_embedder, "mem:episodic:les04", "pooled memory",
            tags=["valkey"],
        )
        pool = sk.gather_domain_pool(fake_store, "valkey")
        assert pool[0]["lesson"] == LESSON

    def test_detail_page_shows_the_lesson(
        self, web_client, fake_store, fake_embedder,
    ):
        key = _memory_with_lesson(
            fake_store, fake_embedder, "mem:episodic:les05", "web memory",
        )
        resp = web_client.get(f"/memory/{key}")
        assert resp.status_code == 200
        assert ">Lesson<" in resp.text
        assert LESSON in resp.text


class TestCompilerPrefersTheLesson:
    def test_lesson_becomes_the_do_rule(self):
        lessons = sk.extract_lessons([
            _pool_mem(breakthrough=NARRATIVE, lesson=LESSON),
        ])
        assert [(l.kind, l.text) for l in lessons] == [("do", LESSON)]

    def test_breakthrough_still_contributes_without_a_lesson(self):
        """Memories written before the field existed compile as before."""
        lessons = sk.extract_lessons([_pool_mem(breakthrough=NARRATIVE)])
        assert [(l.kind, l.text) for l in lessons] == [("do", NARRATIVE)]

    def test_a_lesson_passes_the_same_outcome_gate(self):
        assert sk.extract_lessons([
            _pool_mem(outcome="pivoted", lesson=LESSON),
        ]) == []

    def test_blessing_promotes_the_lesson_regardless_of_outcome(self):
        lessons = sk.extract_lessons([
            _pool_mem(
                outcome="abandoned", blessed=True,
                breakthrough=NARRATIVE, lesson=LESSON,
            ),
        ])
        assert [(l.kind, l.text, l.blessed) for l in lessons] == [
            ("do", LESSON, True),
        ]

    def test_lesson_bearing_counts_a_lesson(self):
        assert sk.lesson_bearing(_pool_mem(lesson=LESSON)) is True
        assert sk.lesson_bearing(_pool_mem(lesson=LESSON, outcome="pivoted")) is False
        assert sk.lesson_bearing(_pool_mem(
            lesson=LESSON, outcome=None, blessed=True, content="",
        )) is True
