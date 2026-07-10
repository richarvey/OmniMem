"""Tests for the web UI version check against Codeberg's latest-release API."""

import io
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from web_ui.routes import version_check


class _FakeResponse(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


@pytest.fixture(autouse=True)
def _reset_cache():
    version_check._cache["latest"] = None
    version_check._cache["checked_at"] = 0.0
    yield


def test_parses_latest_release_object(monkeypatch):
    # /releases/latest returns a single object (not a list) and already
    # excludes drafts and pre-releases server-side.
    payload = json.dumps({"tag_name": "v5.5.3", "prerelease": False}).encode()
    monkeypatch.setattr(
        version_check, "urlopen", lambda req, timeout=5: _FakeResponse(payload)
    )
    assert version_check._fetch_latest_version() == "5.5.3"


def test_result_is_cached(monkeypatch):
    calls = []

    def fake_urlopen(req, timeout=5):
        calls.append(1)
        return _FakeResponse(json.dumps({"tag_name": "v5.5.3"}).encode())

    monkeypatch.setattr(version_check, "urlopen", fake_urlopen)
    assert version_check._fetch_latest_version() == "5.5.3"
    assert version_check._fetch_latest_version() == "5.5.3"
    assert len(calls) == 1, "second call within TTL must come from cache"


def test_malformed_payload_returns_none(monkeypatch):
    monkeypatch.setattr(
        version_check, "urlopen", lambda req, timeout=5: _FakeResponse(b"[]")
    )
    assert version_check._fetch_latest_version() is None


def test_network_error_returns_none(monkeypatch):
    def fake_urlopen(req, timeout=5):
        raise OSError("no network")

    monkeypatch.setattr(version_check, "urlopen", fake_urlopen)
    assert version_check._fetch_latest_version() is None


def test_parse_version_orders_correctly():
    assert version_check._parse_version("6.1.0") > version_check._parse_version("5.5.3")
    assert version_check._parse_version("6.10.0") > version_check._parse_version("6.9.9")
