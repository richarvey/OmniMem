"""Shared tag editing: validate and rewrite the tags field on a memory.

Used by both the MCP retag tool and the web UI detail page, so the two
paths can't drift. Retagging never re-embeds — tags are metadata, the
vector is derived from content alone.
"""

import json
import time
from typing import Any

MAX_TAGS = 20
MAX_TAG_LENGTH = 100

# Skills are derived documents behind the compile gate — their metadata is
# owned by the compiler, so they are deliberately not retaggable.
RETAGGABLE_NAMESPACES = {"episodic", "project", "knowledge", "preference"}


def validate_tags(tags: list[str] | None) -> None:
    """Validate tags list."""
    if tags is None:
        return
    if len(tags) > MAX_TAGS:
        raise ValueError(f"Too many tags ({len(tags)}). Maximum is {MAX_TAGS}.")
    for tag in tags:
        if not isinstance(tag, str) or len(tag) > MAX_TAG_LENGTH:
            raise ValueError(f"Each tag must be a string of at most {MAX_TAG_LENGTH} characters")


def parse_tags_field(raw: Any) -> list[str]:
    """Parse the stored JSON tags field, tolerating missing or malformed data."""
    if not raw:
        return []
    try:
        tags = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return []
    if not isinstance(tags, list):
        return []
    return [str(t) for t in tags]


def _clean(tags: list[str]) -> list[str]:
    """Strip whitespace, drop empties, dedupe preserving order."""
    seen: set[str] = set()
    out: list[str] = []
    for tag in tags:
        t = tag.strip()
        if t and t not in seen:
            seen.add(t)
            out.append(t)
    return out


def retag_memory(
    store: Any,
    key: str,
    tags: list[str] | None = None,
    add: list[str] | None = None,
    remove: list[str] | None = None,
) -> dict[str, Any]:
    """Replace or adjust the tags on a memory in place.

    Pass ``tags`` for a full replacement (an empty list clears all tags),
    or ``add``/``remove`` to adjust the existing set. The two styles are
    mutually exclusive. Returns a status dict; never touches the vector.
    """
    if tags is not None and (add or remove):
        raise ValueError("Pass either tags (full replacement) or add/remove, not both")
    if tags is None and not add and not remove:
        raise ValueError("Nothing to do — pass tags, add, or remove")

    parts = key.split(":")
    if not key.startswith("mem:") or len(parts) < 3:
        raise ValueError(f"Invalid memory key: {key[:50]}")
    namespace = parts[1]
    if namespace not in RETAGGABLE_NAMESPACES:
        raise ValueError(
            f"Cannot retag '{namespace}' entries. Only "
            f"{', '.join(sorted(RETAGGABLE_NAMESPACES))} memories carry editable tags."
        )

    validate_tags(tags)
    validate_tags(add)
    validate_tags(remove)

    data = store.get(key)
    if data is None:
        return {"status": "not_found", "key": key}

    current = parse_tags_field(data.get("tags"))

    if tags is not None:
        new_tags = _clean(tags)
    else:
        new_tags = list(current)
        if remove:
            gone = {t.strip() for t in remove}
            new_tags = [t for t in new_tags if t not in gone]
        if add:
            new_tags = _clean(new_tags + add)

    validate_tags(new_tags)

    if new_tags == current:
        return {"status": "unchanged", "key": key, "tags": new_tags}

    store.set_fields(key, {
        "tags": json.dumps(new_tags),
        "updated_at": str(time.time()),
    })
    return {
        "status": "updated",
        "key": key,
        "tags": new_tags,
        "previous_tags": current,
    }
