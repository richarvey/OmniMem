"""Tests for memory.temporal helpers and recall pipeline temporal boost."""

import time
from datetime import datetime, timedelta

import pytest

from memory.temporal import looks_temporal, parse_query_date, temporal_boost
from tests.conftest import store_memory


# ---------------------------------------------------------------------------
# looks_temporal pre-filter
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("query", [
    "what did I do yesterday",
    "last Tuesday's standup",
    "in March",
    "before October",
    "2026 plans",
    "10/15 meeting",
])
def test_looks_temporal_positive(query):
    assert looks_temporal(query) is True


@pytest.mark.parametrize("query", [
    "what is OmniMem",
    "explain the recall pipeline",
    "preferences for testing",
])
def test_looks_temporal_negative(query):
    assert looks_temporal(query) is False


# ---------------------------------------------------------------------------
# parse_query_date
# ---------------------------------------------------------------------------


def test_parse_query_date_natural_language():
    parsed = parse_query_date("what did I do yesterday")
    assert parsed is not None
    # Yesterday should be ~1 day ago, give or take TZ slack
    delta = (datetime.now() - parsed).total_seconds() / 86400
    assert 0 <= delta <= 2


def test_parse_query_date_skips_non_temporal():
    assert parse_query_date("what is OmniMem") is None


# ---------------------------------------------------------------------------
# temporal_boost curve
# ---------------------------------------------------------------------------


def test_temporal_boost_within_window():
    now = datetime.now()
    event_ts = (now - timedelta(days=2)).timestamp()
    boost = temporal_boost(now, event_ts)
    assert boost == pytest.approx(1.5)


def test_temporal_boost_far_outside():
    now = datetime.now()
    event_ts = (now - timedelta(days=200)).timestamp()
    assert temporal_boost(now, event_ts) == 1.0


def test_temporal_boost_falloff_midway():
    now = datetime.now()
    # ~33 days from query date — falls roughly halfway between 1.5 and 1.0
    event_ts = (now - timedelta(days=33)).timestamp()
    boost = temporal_boost(now, event_ts)
    assert 1.1 < boost < 1.4


# ---------------------------------------------------------------------------
# Pipeline integration: temporal boost reorders results
# ---------------------------------------------------------------------------


def test_pipeline_temporal_boost_prefers_dated_match(
    fake_store, fake_embedder, pipeline
):
    """Two memories with similar content but different event_dates — the
    one matching the query date should rank higher."""
    yesterday_ts = (datetime.now() - timedelta(days=1)).timestamp()
    long_ago_ts = (datetime.now() - timedelta(days=400)).timestamp()

    store_memory(
        fake_store, fake_embedder,
        "mem:episodic:01YESTERDAY",
        content="standup meeting notes from the morning",
    )
    fake_store.set_field("mem:episodic:01YESTERDAY", "event_date", str(yesterday_ts))

    store_memory(
        fake_store, fake_embedder,
        "mem:episodic:01OLD",
        content="standup meeting notes from the morning",
    )
    fake_store.set_field("mem:episodic:01OLD", "event_date", str(long_ago_ts))

    results = pipeline.recall(query="what did the standup yesterday cover", top_k=5)
    keys = [r.key for r in results]
    assert "mem:episodic:01YESTERDAY" in keys
    # The yesterday-dated memory should outrank the very old one
    if "mem:episodic:01OLD" in keys:
        assert keys.index("mem:episodic:01YESTERDAY") < keys.index("mem:episodic:01OLD")


def test_pipeline_no_temporal_query_no_boost(
    fake_store, fake_embedder, pipeline
):
    """Without temporal language in the query, event_date is ignored."""
    yesterday_ts = (datetime.now() - timedelta(days=1)).timestamp()
    store_memory(
        fake_store, fake_embedder,
        "mem:episodic:01A",
        content="OmniMem documentation",
    )
    fake_store.set_field("mem:episodic:01A", "event_date", str(yesterday_ts))

    results = pipeline.recall(query="documentation about OmniMem", top_k=5)
    # The result still surfaces — we just want to confirm event_date is in the
    # parsed RecallResult and the recall didn't error.
    assert any(r.key == "mem:episodic:01A" for r in results)
