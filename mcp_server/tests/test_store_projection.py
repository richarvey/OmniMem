"""Tests for the projected batch-fetch helper get_fields_multi."""


def test_returns_only_requested_fields(fake_store, fake_embedder):
    from tests.conftest import store_memory

    store_memory(
        fake_store, fake_embedder, "mem:episodic:aaa",
        content="hello world", effort_score=3, outcome="succeeded",
    )
    rows = fake_store.get_fields_multi(["mem:episodic:aaa"], ("content", "outcome"))
    assert rows == [{"content": "hello world", "outcome": "succeeded"}]
    # effort_score was not requested, so it must not appear.
    assert "effort_score" not in rows[0]
    # the binary vector must never come back.
    assert "vector" not in rows[0]


def test_missing_key_and_absent_fields_yield_none(fake_store, fake_embedder):
    from tests.conftest import store_memory

    store_memory(fake_store, fake_embedder, "mem:episodic:bbb", content="x")
    rows = fake_store.get_fields_multi(
        ["mem:episodic:missing", "mem:episodic:bbb"],
        ("breakthrough",),  # present on neither
    )
    assert rows == [None, None]


def test_alignment_and_empty_input(fake_store, fake_embedder):
    from tests.conftest import store_memory

    store_memory(fake_store, fake_embedder, "mem:episodic:c1", content="one")
    store_memory(fake_store, fake_embedder, "mem:episodic:c2", content="two")
    keys = ["mem:episodic:c1", "mem:episodic:missing", "mem:episodic:c2"]
    rows = fake_store.get_fields_multi(keys, ("content",))
    assert rows == [{"content": "one"}, None, {"content": "two"}]
    assert fake_store.get_fields_multi([], ("content",)) == []
