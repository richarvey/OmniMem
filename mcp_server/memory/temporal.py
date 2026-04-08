"""Temporal query detection and event_date scoring helpers.

Used by RecallPipeline to boost memories whose event_date is close to a
date the user mentioned in their query (e.g. "what did I do last Tuesday").

The dateparser dep handles natural language and absolute dates uniformly.
"""

from __future__ import annotations

import logging
import re
from datetime import datetime

logger = logging.getLogger(__name__)

# Cheap pre-filter — only run dateparser if the query contains something
# that looks date-shaped. Avoids paying the dateparser cost on every recall.
_TEMPORAL_HINT_RE = re.compile(
    r"(?ix)\b("
    r"yesterday|today|tomorrow|tonight|"
    r"last|this|next|past|previous|recent|recently|"
    r"ago|earlier|later|before|after|since|until|"
    r"monday|tuesday|wednesday|thursday|friday|saturday|sunday|"
    r"january|february|march|april|may|june|july|"
    r"august|september|october|november|december|"
    r"jan|feb|mar|apr|jun|jul|aug|sep|oct|nov|dec|"
    r"\d{4}|\d{1,2}/\d{1,2}|\d{1,2}-\d{1,2}"
    r")\b"
)

# How many days of slack to allow when matching a parsed query date against
# stored event_dates. Within ±7 days gets the full boost; falls off linearly
# after that to zero at ±60 days.
_FULL_MATCH_WINDOW_DAYS = 7
_FALLOFF_WINDOW_DAYS = 60
_MAX_BOOST = 1.5


def looks_temporal(query: str) -> bool:
    """Cheap check before invoking dateparser."""
    if not query:
        return False
    return bool(_TEMPORAL_HINT_RE.search(query))


def parse_query_date(query: str) -> datetime | None:
    """Best-effort parse of a date mentioned in a free-text query.

    Returns None if no parseable date is found. Wraps dateparser so the rest
    of the pipeline doesn't need to know about it.
    """
    if not looks_temporal(query):
        return None
    try:
        import dateparser
        from dateparser.search import search_dates
    except ImportError:
        logger.debug("dateparser not installed — temporal boost disabled")
        return None
    settings = {
        "PREFER_DATES_FROM": "past",
        "RETURN_AS_TIMEZONE_AWARE": False,
    }
    # Try whole-string parse first (cheap, handles "March 2026" or "10/15")
    try:
        parsed = dateparser.parse(query, settings=settings)
        if parsed is not None:
            return parsed
    except Exception:
        pass
    # Fall back to search_dates with explicit English so it handles
    # dates embedded in prose like "what did I do yesterday".
    try:
        found = search_dates(query, languages=["en"], settings=settings)
    except Exception as exc:
        logger.debug("search_dates failed on %r: %s", query[:80], exc)
        return None
    if not found:
        return None
    return found[0][1]


def temporal_boost(query_date: datetime, event_date_ts: float) -> float:
    """Multiplier (1.0 - _MAX_BOOST) based on how close event_date is to query_date.

    - Within ±7 days of the query date → full boost
    - Linearly falls off to 1.0 at ±60 days
    - Beyond that → 1.0 (no penalty, just no boost)
    """
    try:
        event_dt = datetime.fromtimestamp(event_date_ts)
    except (OSError, OverflowError, ValueError):
        return 1.0
    delta_days = abs((event_dt - query_date).total_seconds()) / 86400
    if delta_days <= _FULL_MATCH_WINDOW_DAYS:
        return _MAX_BOOST
    if delta_days >= _FALLOFF_WINDOW_DAYS:
        return 1.0
    # Linear interp between full boost and 1.0
    span = _FALLOFF_WINDOW_DAYS - _FULL_MATCH_WINDOW_DAYS
    progress = (delta_days - _FULL_MATCH_WINDOW_DAYS) / span
    return _MAX_BOOST - (_MAX_BOOST - 1.0) * progress
