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


def _get_embedder() -> SentenceTransformer:
    """Lazy-load the embedding model."""
    global _embedder
    if _embedder is None:
        model_name = os.getenv("EMBEDDING_MODEL", "all-MiniLM-L6-v2")
        logger.info("Loading embedding model: %s", model_name)
        _embedder = SentenceTransformer(model_name)
    return _embedder


def _get_valkey() -> valkey.Valkey:
    """Connect to Valkey."""
    return valkey.Valkey(
        host=os.getenv("VALKEY_HOST", "valkey"),
        port=int(os.getenv("VALKEY_PORT", "6379")),
        password=os.getenv("VALKEY_PASSWORD", ""),
        decode_responses=True,
    )


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

    for entry in feed.entries[:max_articles]:
        try:
            article_url = entry.get("link", "")
            if not article_url:
                continue

            key = f"mem:knowledge:{_url_hash(article_url)}"

            # Dedup check
            if client.exists(key):
                stats["skipped"] += 1
                continue

            title = entry.get("title", "Untitled")
            content_raw = (
                entry.get("content", [{}])[0].get("value", "")
                if entry.get("content")
                else entry.get("summary", "")
            )
            content_text = _strip_html(content_raw)[:2000]

            # Summarise
            summary = summarise(title, article_url, content_text)

            # Embed
            vector = embedder.encode(summary, normalize_embeddings=True)
            vector_bytes = np.array(vector, dtype=np.float32).tobytes()

            # Published date
            published_at = ""
            if entry.get("published_parsed"):
                published_at = str(time.mktime(entry.published_parsed))

            now = str(time.time())

            # Store
            fields = {
                "content": summary,
                "title": title,
                "source_url": article_url,
                "feed_name": name,
                "published_at": published_at,
                "topics": json.dumps(topics),
                "state": "active",
                "surface_score": "1.0",
                "experience_weight": "1.0",
                "created_at": now,
                "updated_at": now,
                "vector": vector_bytes,
            }
            client.hset(key, mapping=fields)
            stats["added"] += 1
            logger.debug("Stored article: %s", title)

        except Exception as exc:
            logger.error("Error processing entry from %s: %s", name, exc)
            stats["errors"] += 1

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
        return {"status": "error", "message": str(exc)}

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
