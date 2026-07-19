"""Fetch, summarise, embed, and store RSS articles."""

import hashlib
import json
import logging
import os
import re
import time
import urllib.parse
import urllib.request
from typing import Any

import feedparser
import numpy as np
import valkey
import yaml
from sentence_transformers import SentenceTransformer

from summariser import extract_items, summarise

logger = logging.getLogger(__name__)

_MIN_CONTENT_LENGTH = 500  # chars — below this we fetch the full page
_PAGE_FETCH_TIMEOUT = 30
_MAX_PAGE_BYTES = int(os.getenv("RSS_MAX_PAGE_BYTES", str(10 * 1024 * 1024)))  # 10 MB cap
_EMBED_CHUNK_SIZE = 32  # embed + store in chunks to limit thread pressure
_PAGE_USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
)

# Every ingested article is labelled with a project so it can be separated
# from conversation-sourced knowledge. Feeds can override the default with a
# `project:` key in feeds.yml. Must match RSS_PROJECT_LABEL in
# mcp_server/memory/migrations.py, which backfills pre-existing articles.
_DEFAULT_PROJECT = "RSS"
# Same allowlist as the MCP tools' project-name validation and recall's
# _TAG_VALUE_SAFE_RE — a label outside it would silently skip the FT.SEARCH
# tag-filter push-down, so fall back to the default instead.
_PROJECT_SAFE_RE = re.compile(r"^[a-zA-Z0-9_\-. ]+$")


def _resolve_project(feed_config: dict[str, Any]) -> str:
    """Project label for a feed's articles: feeds.yml override or the default."""
    project = str(feed_config.get("project") or "").strip()
    if not project:
        return _DEFAULT_PROJECT
    if not _PROJECT_SAFE_RE.match(project):
        logger.warning(
            "Feed %s: project label %r has unsupported characters, using %r",
            feed_config.get("name", feed_config.get("url", "?")),
            project, _DEFAULT_PROJECT,
        )
        return _DEFAULT_PROJECT
    return project

_embedder: SentenceTransformer | None = None
_valkey_client: valkey.Valkey | None = None


def _get_embedder() -> SentenceTransformer:
    """Lazy-load the embedding model."""
    global _embedder
    if _embedder is None:
        model_name = os.getenv("EMBEDDING_MODEL", "all-MiniLM-L6-v2")
        logger.info("Loading embedding model: %s", model_name)
        _embedder = SentenceTransformer(model_name)
    return _embedder


def _get_valkey() -> valkey.Valkey:
    """Get a reusable pooled Valkey connection instead of creating a new one each call."""
    global _valkey_client
    if _valkey_client is None:
        pool = valkey.ConnectionPool(
            host=os.getenv("VALKEY_HOST", "valkey"),
            port=int(os.getenv("VALKEY_PORT", "6379")),
            password=os.getenv("VALKEY_PASSWORD", ""),
            decode_responses=True,
            max_connections=int(os.getenv("VALKEY_MAX_CONNECTIONS", "50")),
        )
        _valkey_client = valkey.Valkey(connection_pool=pool)
    return _valkey_client


def _strip_html(text: str) -> str:
    """Remove HTML tags from text."""
    clean = re.sub(r"<[^>]+>", "", text)
    clean = re.sub(r"\s+", " ", clean).strip()
    return clean


def _url_hash(url: str) -> str:
    """Generate a stable hash for dedup."""
    return hashlib.sha256(url.encode()).hexdigest()[:16]


def _fetch_page_content(url: str) -> str | None:
    """Fetch a web page and extract plain text content."""
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme not in ("http", "https"):
        logger.warning("Refusing to fetch non-HTTP URL: %s", url)
        return None
    try:
        req = urllib.request.Request(url, headers={
            "User-Agent": _PAGE_USER_AGENT,
            "Accept": "text/html,application/xhtml+xml",
            "Accept-Language": "en-GB,en;q=0.9",
        })
        with urllib.request.urlopen(req, timeout=_PAGE_FETCH_TIMEOUT) as resp:  # scheme validated above  # nosec B310
            # Cap the read so a huge or endless response can't exhaust memory.
            raw = resp.read(_MAX_PAGE_BYTES + 1)
            if len(raw) > _MAX_PAGE_BYTES:
                logger.warning(
                    "Page %s exceeded %d bytes, truncating", url, _MAX_PAGE_BYTES,
                )
                raw = raw[:_MAX_PAGE_BYTES]
            html = raw.decode("utf-8", errors="replace")

        # Strip script and style blocks, then all tags
        text = re.sub(r"<(script|style)[^>]*>.*?</\1>", "", html, flags=re.DOTALL)
        text = re.sub(r"<[^>]+>", " ", text)
        text = re.sub(r"\s+", " ", text).strip()
        return text
    except Exception as exc:
        logger.warning("Failed to fetch page content from %s: %s", url, exc)
        return None


def _format_item(item: dict[str, str]) -> str:
    """Format an extracted item as readable content for storage."""
    return (
        f"# {item['title']}\n\n"
        f"**Who:** {item['who']}\n"
        f"**What:** {item['what']}\n"
        f"**Why:** {item['why']}"
    )


def _get_entry_content(entry: Any) -> str:
    """Extract plain text content from an RSS feed entry."""
    content_raw = (
        entry.get("content", [{}])[0].get("value", "")
        if entry.get("content")
        else entry.get("summary", "")
    )
    return _strip_html(content_raw)


def _process_digest_entry(
    entry: Any, article_url: str, feed_name: str,
) -> list[dict[str, Any]] | None:
    """Process a digest-mode entry: get full content and extract items.

    Returns a list of article dicts (same shape as summary mode) or None.
    """
    title = entry.get("title", "Untitled")
    content_text = _get_entry_content(entry)

    # If RSS content is too short (teaser/paywall), fetch the full page
    if len(content_text) < _MIN_CONTENT_LENGTH:
        logger.info(
            "RSS content too short for '%s' (%d chars), fetching page",
            title, len(content_text),
        )
        page_text = _fetch_page_content(article_url)
        if page_text:
            content_text = page_text
        elif len(content_text) < 50:
            logger.warning("No usable content for '%s' — skipping", title)
            return None

    items = extract_items(title, article_url, content_text)
    if not items:
        return None

    published_at = ""
    if entry.get("published_parsed"):
        published_at = str(time.mktime(entry.published_parsed))

    articles = []
    for i, item in enumerate(items):
        key = f"mem:knowledge:{_url_hash(article_url + ':' + str(i))}"
        articles.append({
            "key": key,
            "title": item["title"],
            "summary": _format_item(item),
            "article_url": article_url,
            "published_at": published_at,
        })

    return articles


def ingest_feed(feed_config: dict[str, Any]) -> dict[str, int]:
    """Fetch and ingest a single RSS feed.

    Uses batch dedup checks and batch embedding for efficiency.

    Args:
        feed_config: Dict with url, name, and topics.

    Returns:
        Stats dict with added, skipped, errors counts.
    """
    url = feed_config["url"]
    name = feed_config.get("name", url)
    topics = feed_config.get("topics", [])
    mode = feed_config.get("mode", "summary")
    project = _resolve_project(feed_config)
    max_articles = int(os.getenv("RSS_MAX_ARTICLES_PER_FEED", "20"))
    max_digest = int(os.getenv("RSS_MAX_DIGEST_ENTRIES", "2"))

    stats = {"added": 0, "skipped": 0, "errors": 0}

    try:
        feed = feedparser.parse(url)
    except Exception as exc:
        logger.error("Failed to fetch feed %s: %s", name, exc)
        stats["errors"] += 1
        return stats

    if not feed.entries:
        logger.warning("No entries in feed: %s", name)
        return stats

    client = _get_valkey()
    embedder = _get_embedder()

    limit = max_digest if mode == "digest" else max_articles
    entries = feed.entries[:limit]

    # Phase 1: Extract URLs and dedup keys for all entries.
    # Digest mode uses a different key so one article can produce many items.
    entry_keys: list[tuple[Any, str, str]] = []  # (feed_entry, article_url, dedup_key)
    for entry in entries:
        article_url = entry.get("link", "")
        if not article_url:
            continue
        if mode == "digest":
            dedup_key = f"mem:knowledge:{_url_hash(article_url + ':0')}"
        else:
            dedup_key = f"mem:knowledge:{_url_hash(article_url)}"
        entry_keys.append((entry, article_url, dedup_key))

    if not entry_keys:
        return stats

    # Phase 2: Batch dedup check using a pipeline (one round-trip instead of N)
    pipe = client.pipeline(transaction=False)
    for _, _, key in entry_keys:
        pipe.exists(key)
    exists_results = pipe.execute()

    # Phase 3: Process new articles — summary or digest mode
    new_articles: list[dict[str, Any]] = []
    for (entry, article_url, dedup_key), exists in zip(entry_keys, exists_results):
        if exists:
            stats["skipped"] += 1
            continue

        try:
            if mode == "digest":
                items = _process_digest_entry(entry, article_url, name)
                if items is None:
                    logger.info(
                        "Skipping '%s' — digest extraction failed",
                        entry.get("title", "Untitled"),
                    )
                    stats["skipped"] += 1
                    continue
                new_articles.extend(items)
            else:
                title = entry.get("title", "Untitled")
                content_text = _get_entry_content(entry)[:2000]

                summary = summarise(title, article_url, content_text)
                if summary is None:
                    logger.info("Skipping '%s' — summarisation refused", title)
                    stats["skipped"] += 1
                    continue

                published_at = ""
                if entry.get("published_parsed"):
                    published_at = str(time.mktime(entry.published_parsed))

                new_articles.append({
                    "key": dedup_key,
                    "title": title,
                    "summary": summary,
                    "article_url": article_url,
                    "published_at": published_at,
                })
        except Exception as exc:
            logger.error("Error processing entry from %s: %s", name, exc)
            stats["errors"] += 1

    if not new_articles:
        logger.info(
            "Feed %s: added=%d, skipped=%d, errors=%d",
            name, stats["added"], stats["skipped"], stats["errors"],
        )
        return stats

    # Phase 4+5: Embed and store in chunks to limit thread pressure
    # and ensure earlier chunks persist even if a later chunk fails.
    now = str(time.time())
    topics_json = json.dumps(topics)
    max_age_days = int(os.getenv("MAX_KNOWLEDGE_AGE_DAYS", "30"))
    expires_at = str(float(now) + max_age_days * 86400)

    for chunk_start in range(0, len(new_articles), _EMBED_CHUNK_SIZE):
        chunk = new_articles[chunk_start:chunk_start + _EMBED_CHUNK_SIZE]
        summaries = [a["summary"] for a in chunk]

        try:
            vectors = embedder.encode(
                summaries, normalize_embeddings=True, batch_size=_EMBED_CHUNK_SIZE,
            )
        except Exception as exc:
            logger.error(
                "Embedding failed for chunk %d–%d of feed %s: %s",
                chunk_start, chunk_start + len(chunk), name, exc,
            )
            stats["errors"] += len(chunk)
            continue

        store_pipe = client.pipeline(transaction=False)
        for article, vector in zip(chunk, vectors):
            vector_bytes = np.array(vector, dtype=np.float32).tobytes()
            fields = {
                "content": article["summary"],
                "title": article["title"],
                "source_url": article["article_url"],
                "feed_name": name,
                "project": project,
                "published_at": article["published_at"],
                "topics": topics_json,
                "state": "active",
                "surface_score": "1.0",
                "experience_weight": "1.0",
                "created_at": now,
                "updated_at": now,
                "expires_at": expires_at,
                "vector": vector_bytes,
            }
            store_pipe.hset(article["key"], mapping=fields)

        try:
            store_pipe.execute()
            stats["added"] += len(chunk)
        except Exception as exc:
            logger.error(
                "Store failed for chunk %d–%d of feed %s: %s",
                chunk_start, chunk_start + len(chunk), name, exc,
            )
            stats["errors"] += len(chunk)

    logger.info(
        "Feed %s: added=%d, skipped=%d, errors=%d",
        name, stats["added"], stats["skipped"], stats["errors"],
    )
    return stats


def ingest_all_feeds(feeds_config_path: str = "/app/feeds.yml") -> dict[str, Any]:
    """Load feed config and ingest all feeds.

    Args:
        feeds_config_path: Path to the feeds YAML file.

    Returns:
        Overall stats dict.
    """
    try:
        with open(feeds_config_path, "r", encoding="utf-8") as f:
            config = yaml.safe_load(f)
    except (OSError, yaml.YAMLError) as exc:
        logger.error("Failed to load feeds config: %s", exc)
        return {"status": "error", "message": "Failed to load feeds configuration"}

    feeds = config.get("feeds", [])
    if not feeds:
        logger.warning("No feeds configured in %s", feeds_config_path)
        return {"status": "no_feeds", "feeds_processed": 0}

    total_stats: dict[str, int] = {"added": 0, "skipped": 0, "errors": 0}

    for feed_config in feeds:
        try:
            stats = ingest_feed(feed_config)
            for k, v in stats.items():
                total_stats[k] = total_stats.get(k, 0) + v
        except Exception as exc:
            logger.error("Failed to ingest feed %s: %s", feed_config.get("name", "?"), exc)
            total_stats["errors"] += 1

    logger.info(
        "Ingestion complete: feeds=%d, added=%d, skipped=%d, errors=%d",
        len(feeds), total_stats["added"], total_stats["skipped"], total_stats["errors"],
    )

    return {
        "status": "complete",
        "feeds_processed": len(feeds),
        **total_stats,
    }
