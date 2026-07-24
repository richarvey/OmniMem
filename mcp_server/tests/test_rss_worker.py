"""Tests for the RSS worker: summariser and ingester, with faked HTTP + API.

The worker modules import each other as top-level modules (they run with
/app as the working directory in Docker), so rss_worker goes on sys.path.
"""

import io
import json
import sys
import time
import types
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "rss_worker"))

# The Docker test image builds from mcp_server/ only — no rss_worker there.
ingester = pytest.importorskip("ingester", reason="rss_worker not present in this test image")
summariser = pytest.importorskip("summariser", reason="rss_worker not present in this test image")

import anthropic
import httpx
import numpy as np
import yaml


def _message(text):
    return types.SimpleNamespace(content=[types.SimpleNamespace(text=text)])


class FakeAnthropicClient:
    """Queue of responses; an Exception instance in the queue is raised."""

    def __init__(self, responses):
        self._responses = list(responses)
        self.calls = 0
        self.messages = types.SimpleNamespace(create=self._create)

    def _create(self, **kwargs):
        self.calls += 1
        item = self._responses.pop(0)
        if isinstance(item, Exception):
            raise item
        return _message(item)


@pytest.fixture
def no_sleep(monkeypatch):
    monkeypatch.setattr(time, "sleep", lambda s: None)


@pytest.fixture
def anthropic_client(monkeypatch):
    """Install a FakeAnthropicClient factory into the summariser module."""
    def install(responses):
        client = FakeAnthropicClient(responses)
        monkeypatch.setattr(summariser, "_anthropic_client", client)
        return client
    yield install
    monkeypatch.setattr(summariser, "_anthropic_client", None)


def _timeout_error():
    return anthropic.APITimeoutError(request=httpx.Request("GET", "http://api"))


class TestGetClient:
    def test_no_key_returns_none(self, monkeypatch):
        monkeypatch.setattr(summariser, "_anthropic_client", None)
        monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
        assert summariser._get_client() is None

    def test_placeholder_key_returns_none(self, monkeypatch):
        monkeypatch.setattr(summariser, "_anthropic_client", None)
        monkeypatch.setenv("ANTHROPIC_API_KEY", "your_key_here")
        assert summariser._get_client() is None

    def test_client_cached(self, monkeypatch):
        monkeypatch.setattr(summariser, "_anthropic_client", None)
        monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test")
        first = summariser._get_client()
        assert first is not None
        assert summariser._get_client() is first
        monkeypatch.setattr(summariser, "_anthropic_client", None)


class TestRefusalAndFallback:
    def test_refusal_phrases(self):
        assert summariser._is_refusal("I don't have access to that URL")
        assert summariser._is_refusal("I'm unable to browse the web")
        assert not summariser._is_refusal("Rust 1.80 ships new features")

    def test_fallback_truncates_at_800(self):
        long_text = "x" * 900
        out = summariser._fallback_summary("Title", long_text)
        assert out.startswith("Title. ")
        assert out.endswith("...")
        assert len(out) < 900

    def test_fallback_short_text_untouched(self):
        assert summariser._fallback_summary("T", "short") == "T. short"


class TestSummarise:
    def test_no_client_falls_back(self, monkeypatch):
        monkeypatch.setattr(summariser, "_anthropic_client", None)
        monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
        assert summariser.summarise("T", "http://a", "body") == "T. body"

    def test_success(self, anthropic_client):
        anthropic_client(["A concise summary."])
        assert summariser.summarise("T", "http://a", "body") == "A concise summary."

    def test_refusal_returns_none(self, anthropic_client):
        anthropic_client(["I don't have access to external URLs."])
        assert summariser.summarise("T", "http://a", "body") is None

    def test_retryable_then_success(self, anthropic_client, no_sleep):
        client = anthropic_client([_timeout_error(), "Recovered summary."])
        assert summariser.summarise("T", "http://a", "body") == "Recovered summary."
        assert client.calls == 2

    def test_retries_exhausted_fall_back(self, anthropic_client, no_sleep):
        anthropic_client([_timeout_error(), _timeout_error()])
        assert summariser.summarise("T", "http://a", "body") == "T. body"

    def test_non_retryable_falls_back(self, anthropic_client):
        anthropic_client([ValueError("bad request")])
        assert summariser.summarise("T", "http://a", "body") == "T. body"


class TestExtractItems:
    _ITEM = {"title": "Rust 1.80", "who": "Rust team", "what": "released",
             "why": "faster builds"}

    def test_no_client_returns_none(self, monkeypatch):
        monkeypatch.setattr(summariser, "_anthropic_client", None)
        monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
        assert summariser.extract_items("T", "http://a", "body") is None

    def test_success(self, anthropic_client):
        anthropic_client([json.dumps([self._ITEM])])
        items = summariser.extract_items("T", "http://a", "body")
        assert items == [self._ITEM]

    def test_markdown_fences_stripped(self, anthropic_client):
        anthropic_client(["```json\n" + json.dumps([self._ITEM]) + "\n```"])
        assert summariser.extract_items("T", "http://a", "body") == [self._ITEM]

    def test_refusal_returns_none(self, anthropic_client):
        anthropic_client(["I cannot access that article."])
        assert summariser.extract_items("T", "http://a", "body") is None

    def test_non_list_returns_none(self, anthropic_client):
        anthropic_client([json.dumps({"title": "not a list"})])
        assert summariser.extract_items("T", "http://a", "body") is None

    def test_invalid_json_returns_none(self, anthropic_client):
        anthropic_client(["not json at all"])
        assert summariser.extract_items("T", "http://a", "body") is None

    def test_items_missing_keys_filtered(self, anthropic_client):
        anthropic_client([json.dumps([self._ITEM, {"title": "incomplete"}, "junk"])])
        assert summariser.extract_items("T", "http://a", "body") == [self._ITEM]

    def test_all_items_invalid_returns_none(self, anthropic_client):
        anthropic_client([json.dumps([{"title": "incomplete"}])])
        assert summariser.extract_items("T", "http://a", "body") is None

    def test_retries_exhausted_return_none(self, anthropic_client, no_sleep):
        anthropic_client([_timeout_error(), _timeout_error()])
        assert summariser.extract_items("T", "http://a", "body") is None

    def test_generic_exception_returns_none(self, anthropic_client):
        anthropic_client([ValueError("boom")])
        assert summariser.extract_items("T", "http://a", "body") is None


class TestIngesterHelpers:
    def test_strip_html(self):
        assert ingester._strip_html("<p>Hello  <b>world</b></p>") == "Hello world"

    def test_url_hash_stable(self):
        assert ingester._url_hash("http://a") == ingester._url_hash("http://a")
        assert len(ingester._url_hash("http://a")) == 16

    def test_resolve_project_default(self):
        assert ingester._resolve_project({}) == "RSS"
        assert ingester._resolve_project({"project": "  "}) == "RSS"

    def test_resolve_project_override(self):
        assert ingester._resolve_project({"project": "my proj.1"}) == "my proj.1"

    def test_resolve_project_bad_charset_falls_back(self):
        assert ingester._resolve_project({"project": "bad|label"}) == "RSS"

    def test_format_item(self):
        text = ingester._format_item({"title": "T", "who": "W", "what": "X", "why": "Y"})
        assert "# T" in text and "**Who:** W" in text

    def test_get_entry_content_prefers_content(self):
        entry = {"content": [{"value": "<p>full</p>"}], "summary": "short"}
        assert ingester._get_entry_content(entry) == "full"

    def test_get_entry_content_falls_back_to_summary(self):
        assert ingester._get_entry_content({"summary": "<i>short</i>"}) == "short"


class _FakeResponse:
    def __init__(self, body: bytes):
        self._body = io.BytesIO(body)

    def read(self, n=-1):
        return self._body.read(n)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class TestFetchPageContent:
    def test_rejects_non_http(self):
        assert ingester._fetch_page_content("ftp://example.org/x") is None
        assert ingester._fetch_page_content("file:///etc/passwd") is None

    def test_strips_scripts_and_tags(self, monkeypatch):
        html = b"<html><script>evil()</script><style>.x{}</style><p>Real content</p></html>"
        monkeypatch.setattr(ingester.urllib.request, "urlopen",
                            lambda req, timeout: _FakeResponse(html))
        assert ingester._fetch_page_content("http://example.org") == "Real content"

    def test_truncates_huge_pages(self, monkeypatch):
        monkeypatch.setattr(ingester, "_MAX_PAGE_BYTES", 100)
        monkeypatch.setattr(ingester.urllib.request, "urlopen",
                            lambda req, timeout: _FakeResponse(b"a" * 200))
        text = ingester._fetch_page_content("http://example.org")
        assert len(text) == 100

    def test_fetch_error_returns_none(self, monkeypatch):
        def boom(req, timeout):
            raise OSError("connection refused")
        monkeypatch.setattr(ingester.urllib.request, "urlopen", boom)
        assert ingester._fetch_page_content("http://example.org") is None


class FakeIngestPipeline:
    def __init__(self, client):
        self._client = client
        self._commands = []

    def exists(self, key):
        self._commands.append(("exists", key))
        return self

    def hset(self, key, mapping=None):
        self._commands.append(("hset", key, mapping))
        return self

    def delete(self, key):
        self._commands.append(("delete", key))
        return self

    def execute(self):
        commands, self._commands = self._commands, []
        # fail_store only breaks the store pipeline (hset), not the dedup one
        if self._client.fail_store and any(c[0] == "hset" for c in commands):
            raise RuntimeError("valkey down")
        results = []
        for cmd in commands:
            if cmd[0] == "exists":
                results.append(1 if cmd[1] in self._client.data else 0)
            elif cmd[0] == "delete":
                results.append(1 if self._client.data.pop(cmd[1], None) else 0)
            else:
                existing = self._client.data.setdefault(cmd[1], {})
                existing.update(dict(cmd[2]))
                results.append(True)
        return results


class FakeIngestValkey:
    def __init__(self):
        self.data: dict[str, dict] = {}
        self.fail_store = False

    def pipeline(self, transaction=False):
        return FakeIngestPipeline(self)


class FakeSentenceTransformer:
    def encode(self, texts, normalize_embeddings=True, batch_size=32):
        return [np.ones(8, dtype=np.float32) for _ in texts]


class FeedEntry(dict):
    """feedparser entries answer to both entry.get('x') and entry.x."""

    def __getattr__(self, name):
        try:
            return self[name]
        except KeyError:
            raise AttributeError(name)


def _entry(link="https://example.org/post", title="A Post", summary="<p>text</p>",
           published=None):
    e = FeedEntry(link=link, title=title, summary=summary)
    if published:
        e["published_parsed"] = time.struct_time((2026, 7, 1, 12, 0, 0, 0, 0, 0))
    return e


@pytest.fixture
def ingest_env(monkeypatch):
    """Wire ingest_feed to fakes; returns the fake valkey client."""
    client = FakeIngestValkey()
    monkeypatch.setattr(ingester, "_get_valkey", lambda: client)
    monkeypatch.setattr(ingester, "_get_embedder", lambda: FakeSentenceTransformer())
    return client


def _patch_feed(monkeypatch, entries):
    feed = types.SimpleNamespace(entries=entries)
    monkeypatch.setattr(ingester.feedparser, "parse", lambda url: feed)


class TestIngestFeed:
    CONFIG = {"url": "https://example.org/feed.xml", "name": "Example",
              "topics": ["rust"]}

    def test_summary_mode_adds_article(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry(published=True)])
        monkeypatch.setattr(ingester, "summarise", lambda t, u, c: "A summary.")

        stats = ingester.ingest_feed(self.CONFIG)
        assert stats == {"added": 1, "skipped": 0, "errors": 0}
        stored = list(ingest_env.data.values())[0]
        assert stored["content"] == "A summary."
        assert stored["feed_name"] == "Example"
        assert stored["project"] == "RSS"
        assert "vector" in stored
        assert float(stored["expires_at"]) > float(stored["created_at"])

    def test_project_override(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry()])
        monkeypatch.setattr(ingester, "summarise", lambda t, u, c: "S.")
        stats = ingester.ingest_feed({**self.CONFIG, "project": "myproj"})
        assert stats["added"] == 1
        assert list(ingest_env.data.values())[0]["project"] == "myproj"

    def test_dedup_skips_existing(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry()])
        monkeypatch.setattr(ingester, "summarise", lambda t, u, c: "S.")
        assert ingester.ingest_feed(self.CONFIG)["added"] == 1
        stats = ingester.ingest_feed(self.CONFIG)
        assert stats == {"added": 0, "skipped": 1, "errors": 0}

    def test_summarise_refusal_skips(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry()])
        monkeypatch.setattr(ingester, "summarise", lambda t, u, c: None)
        stats = ingester.ingest_feed(self.CONFIG)
        assert stats == {"added": 0, "skipped": 1, "errors": 0}

    def test_entries_without_links_ignored(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [{"title": "no link", "summary": "x"}])
        stats = ingester.ingest_feed(self.CONFIG)
        assert stats == {"added": 0, "skipped": 0, "errors": 0}

    def test_empty_feed(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [])
        stats = ingester.ingest_feed(self.CONFIG)
        assert stats == {"added": 0, "skipped": 0, "errors": 0}

    def test_processing_error_counted(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry()])
        def boom(t, u, c):
            raise RuntimeError("summariser exploded")
        monkeypatch.setattr(ingester, "summarise", boom)
        stats = ingester.ingest_feed(self.CONFIG)
        assert stats["errors"] == 1

    def test_embedding_failure_counts_errors(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry()])
        monkeypatch.setattr(ingester, "summarise", lambda t, u, c: "S.")

        class BrokenEmbedder:
            def encode(self, *a, **kw):
                raise RuntimeError("model died")
        monkeypatch.setattr(ingester, "_get_embedder", lambda: BrokenEmbedder())
        stats = ingester.ingest_feed(self.CONFIG)
        assert stats == {"added": 0, "skipped": 0, "errors": 1}

    def test_store_failure_counts_errors(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry()])
        monkeypatch.setattr(ingester, "summarise", lambda t, u, c: "S.")
        ingest_env.fail_store = True
        stats = ingester.ingest_feed(self.CONFIG)
        assert stats == {"added": 0, "skipped": 0, "errors": 1}

    def test_digest_mode(self, ingest_env, monkeypatch):
        long_summary = "word " * 200  # over _MIN_CONTENT_LENGTH once stripped
        _patch_feed(monkeypatch, [_entry(summary=long_summary, published=True)])
        items = [
            {"title": "Item A", "who": "A", "what": "did", "why": "matters"},
            {"title": "Item B", "who": "B", "what": "did", "why": "matters"},
        ]
        monkeypatch.setattr(ingester, "extract_items", lambda t, u, c: items)
        stats = ingester.ingest_feed({**self.CONFIG, "mode": "digest"})
        assert stats["added"] == 2
        titles = {v["title"] for v in ingest_env.data.values()}
        assert titles == {"Item A", "Item B"}

    def test_digest_extraction_failure_skips(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry(summary="word " * 200)])
        monkeypatch.setattr(ingester, "extract_items", lambda t, u, c: None)
        stats = ingester.ingest_feed({**self.CONFIG, "mode": "digest"})
        assert stats == {"added": 0, "skipped": 1, "errors": 0}

    def test_digest_short_content_fetches_page(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry(summary="tiny but over fifty characters of teaser text here")])
        fetched = []
        def fake_fetch(url):
            fetched.append(url)
            return "full page " * 100
        monkeypatch.setattr(ingester, "_fetch_page_content", fake_fetch)
        monkeypatch.setattr(ingester, "extract_items",
                            lambda t, u, c: [{"title": "I", "who": "w",
                                              "what": "x", "why": "y"}])
        stats = ingester.ingest_feed({**self.CONFIG, "mode": "digest"})
        assert fetched == ["https://example.org/post"]
        assert stats["added"] == 1

    def test_digest_no_usable_content_skips(self, ingest_env, monkeypatch):
        _patch_feed(monkeypatch, [_entry(summary="tiny")])
        monkeypatch.setattr(ingester, "_fetch_page_content", lambda url: None)
        stats = ingester.ingest_feed({**self.CONFIG, "mode": "digest"})
        assert stats == {"added": 0, "skipped": 1, "errors": 0}


class TestIngestAllFeeds:
    @pytest.fixture(autouse=True)
    def _fake_valkey(self, monkeypatch):
        self.client = FakeIngestValkey()
        monkeypatch.setattr(ingester, "_get_valkey", lambda: self.client)

    def test_missing_config(self, tmp_path):
        result = ingester.ingest_all_feeds(str(tmp_path / "missing.yml"))
        assert result["status"] == "error"

    def test_no_feeds(self, tmp_path):
        path = tmp_path / "feeds.yml"
        path.write_text(yaml.dump({"feeds": []}))
        result = ingester.ingest_all_feeds(str(path))
        assert result["status"] == "no_feeds"

    def test_aggregates_stats_and_errors(self, tmp_path, monkeypatch):
        path = tmp_path / "feeds.yml"
        path.write_text(yaml.dump({"feeds": [
            {"url": "https://a.example", "name": "A"},
            {"url": "https://b.example", "name": "B"},
        ]}))

        def fake_ingest(config):
            if config["name"] == "A":
                return {"added": 2, "skipped": 1, "errors": 0}
            raise RuntimeError("feed b broke")
        monkeypatch.setattr(ingester, "ingest_feed", fake_ingest)

        result = ingester.ingest_all_feeds(str(path))
        assert result["status"] == "complete"
        assert result["feeds_processed"] == 2
        assert result["added"] == 2
        assert result["skipped"] == 1
        assert result["errors"] == 1

    def test_mirrors_feed_influence(self, tmp_path, monkeypatch):
        path = tmp_path / "feeds.yml"
        path.write_text(yaml.dump({"feeds": [
            {"url": "https://a.example", "name": "A",
             "topics": ["python"], "skills": {"python": 7}},
        ]}))
        monkeypatch.setattr(
            ingester, "ingest_feed",
            lambda config: {"added": 0, "skipped": 0, "errors": 0},
        )
        ingester.ingest_all_feeds(str(path))
        mirrored = self.client.data[ingester._FEED_INFLUENCE_KEY]
        entry = json.loads(mirrored["A"])
        assert entry["url"] == "https://a.example"
        assert entry["skills"] == {"python": 7}
        assert entry["topics"] == ["python"]

    def test_sync_failure_does_not_break_ingestion(self, tmp_path, monkeypatch):
        path = tmp_path / "feeds.yml"
        path.write_text(yaml.dump({"feeds": [
            {"url": "https://a.example", "name": "A"},
        ]}))
        monkeypatch.setattr(
            ingester, "_sync_feed_influence",
            lambda client, feeds: (_ for _ in ()).throw(RuntimeError("down")),
        )
        monkeypatch.setattr(
            ingester, "ingest_feed",
            lambda config: {"added": 1, "skipped": 0, "errors": 0},
        )
        result = ingester.ingest_all_feeds(str(path))
        assert result["status"] == "complete"
        assert result["added"] == 1


class TestSyncFeedInfluence:
    def test_writes_full_entries(self):
        client = FakeIngestValkey()
        count = ingester._sync_feed_influence(client, [
            {"url": "https://a.example", "name": "A", "topics": ["x"],
             "mode": "digest", "project": "research", "skills": {"python": 9}},
            {"url": "https://b.example", "name": "B"},
        ])
        assert count == 2
        entry = json.loads(client.data[ingester._FEED_INFLUENCE_KEY]["A"])
        assert entry == {"url": "https://a.example", "topics": ["x"],
                         "skills": {"python": 9}, "mode": "digest",
                         "project": "research"}
        assert json.loads(client.data[ingester._FEED_INFLUENCE_KEY]["B"])["skills"] == {}

    def test_full_replace_and_invalid_entries(self):
        client = FakeIngestValkey()
        ingester._sync_feed_influence(client, [
            {"url": "https://old.example", "name": "Old"},
        ])
        count = ingester._sync_feed_influence(client, [
            {"url": "https://new.example", "name": "New"},
            {"url": "", "name": "NoUrl"},
            "not a dict",
        ])
        assert count == 1
        assert set(client.data[ingester._FEED_INFLUENCE_KEY]) == {"New"}

    def test_empty_list_clears_key(self):
        client = FakeIngestValkey()
        ingester._sync_feed_influence(client, [
            {"url": "https://a.example", "name": "A"},
        ])
        assert ingester._sync_feed_influence(client, []) == 0
        assert ingester._FEED_INFLUENCE_KEY not in client.data
