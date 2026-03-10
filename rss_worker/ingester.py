"""Fetch, summarise, embed, and store RSS articles."""

import hashlib
import json
import logging
import os
import re
import time
from typing import Any

import feedparser
import numpy as np
import valkey
import yaml
from sentence_transformers import SentenceTransformer

from summariser import summarise

logger = logging.getLogger(__name__)

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
            max_connections=10,
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
    max_articles = int(os.getenv("RSS_MAX_ARTICLES_PER_FEED", "20"))

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

    entries = feed.entries[:max_articles]

    # Phase 1: Extract URLs and keys for all entries
    entry_keys: list[tuple[Any, str, str]] = []  # (feed_entry, article_url, valkey_key)
    for entry in entries:
        article_url = entry.get("link", "")
        if not article_url:
            continue
        key = f"mem:knowledge:{_url_hash(article_url)}"
        entry_keys.append((entry, article_url, key))

    if not entry_keys:
        return stats

    # Phase 2: Batch dedup check using a pipeline (one round-trip instead of N)
    pipe = client.pipeline(transaction=False)
    for _, _, key in entry_keys:
        pipe.exists(key)
    exists_results = pipe.execute()

    # Phase 3: Summarise new articles (this is the API-bound step)
    new_articles: list[dict[str, Any]] = []
    for (entry, article_url, key), exists in zip(entry_keys, exists_results):
        if exists:
            stats["skipped"] += 1
            continue

        try:
            title = entry.get("title", "Untitled")
            content_raw = (
                entry.get("content", [{}])[0].get("value", "")
                if entry.get("content")
                else entry.get("summary", "")
            )
            content_text = _strip_html(content_raw)[:2000]

            summary = summarise(title, article_url, content_text)

            published_at = ""
            if entry.get("published_parsed"):
                published_at = str(time.mktime(entry.published_parsed))

            new_articles.append({
                "key": key,
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

    # Phase 4: Batch embed all summaries at once instead of one-at-a-time
    summaries = [a["summary"] for a in new_articles]
    try:
        vectors = embedder.encode(summaries, normalize_embeddings=True, batch_size=32)
    except Exception as exc:
        logger.error("Batch embedding failed for feed %s: %s", name, exc)
        stats["errors"] += len(new_articles)
        return stats

    # Phase 5: Batch store all articles using a pipeline
    now = str(time.time())
    topics_json = json.dumps(topics)

    store_pipe = client.pipeline(transaction=False)
    for article, vector in zip(new_articles, vectors):
        vector_bytes = np.array(vector, dtype=np.float32).tobytes()
        fields = {
            "content": article["summary"],
            "title": article["title"],
            "source_url": article["article_url"],
            "feed_name": name,
            "published_at": article["published_at"],
            "topics": topics_json,
            "state": "active",
            "surface_score": "1.0",
            "experience_weight": "1.0",
            "created_at": now,
            "updated_at": now,
            "vector": vector_bytes,
        }
        store_pipe.hset(article["key"], mapping=fields)

    try:
        store_pipe.execute()
        stats["added"] += len(new_articles)
    except Exception as exc:
        logger.error("Batch store failed for feed %s: %s", name, exc)
        stats["errors"] += len(new_articles)

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
