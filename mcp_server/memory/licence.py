"""Licence / redistribution rights on memories (v6.6.1).

Every memory carries a ``licence`` field answering one question: may the
content be redistributed outside this instance? It is decided at ingest —
the RSS worker stamps every article from what its feed declares, and the
tool layer stamps every conversation-sourced write — because the worker
accrues records every night and the ones that can't be shipped are
impossible to pick out cheaply afterwards. "It's only a summary" is a weak
defence once a bundle has been sold, so the answer is recorded when the
source is still in front of us, not audited later.

The field is a small controlled vocabulary, not a licence identifier:

    own         authored here — a decision, a bug write-up, a preference.
                The default for conversation-sourced namespaces.
    open        third-party content under terms that permit redistribution
                (OGL, CC BY, CC0, public domain, permissive code licences).
    restricted  third-party content we may NOT redistribute — all rights
                reserved, paywalled, commercial, or a non-commercial /
                no-derivatives Creative Commons variant (a summary is a
                derivative).
    unknown     not determined. The honest backfill value for records that
                pre-date the field, and what a feed gets when it declares
                nothing. A queryable unknown beats a wrong value nobody
                revisits; recall points them out so a human can classify.

Human-facing inputs (feeds.yml, the MCP tools, the web UI) may also give a
recognised licence identifier — ``cc-by-4.0``, ``ogl-3.0``,
``all-rights-reserved`` — which resolves to a class and keeps the identifier
as ``licence_note`` so the specific terms survive. Unrecognised identifiers
are rejected rather than guessed at: a misclassified record is exactly the
liability the field exists to prevent.

This axis is about redistribution *rights*. It is deliberately not about
disclosure *scope* (who inside a network may see a memory), which is a
separate field with separate values. A summary of a paywalled standard can
legitimately be visible to everyone and still be non-redistributable — the
two answer different questions and one must never be read as the other.

The RSS worker image doesn't ship this package, so ``rss_worker/ingester.py``
carries a copy of the alias table and resolver. The test suite asserts the
two tables are identical; keep them in step.
"""

from __future__ import annotations

from .lineage import (  # noqa: F401 — is_classifiable_key/stamp_lineage re-exported
    is_classifiable_key,
    normalise_alias_key,
    stamp_lineage,
)

LICENCE_OWN = "own"
LICENCE_OPEN = "open"
LICENCE_RESTRICTED = "restricted"
LICENCE_UNKNOWN = "unknown"

LICENCE_CLASSES: tuple[str, ...] = (
    LICENCE_OWN, LICENCE_OPEN, LICENCE_RESTRICTED, LICENCE_UNKNOWN,
)

# Human-readable labels for the web UI and tool responses.
LICENCE_LABELS: dict[str, str] = {
    LICENCE_OWN: "Own work",
    LICENCE_OPEN: "Open (redistributable)",
    LICENCE_RESTRICTED: "Restricted (not redistributable)",
    LICENCE_UNKNOWN: "Unknown (needs classifying)",
}

# (value, label) pairs for form selects, in vocabulary order.
LICENCE_CHOICES: list[tuple[str, str]] = [
    (value, LICENCE_LABELS[value]) for value in LICENCE_CLASSES
]

MAX_LICENCE_NOTE = 200

# Recognised licence identifiers → (class, note). The note is the canonical
# spelling of the identifier, stored alongside the class so a buyer can see
# *which* open licence applies, not just that one does. Non-commercial and
# no-derivatives Creative Commons variants are restricted on purpose: a
# summary is a derivative, and a sold bundle is commercial use.
#
# Mirrored verbatim in rss_worker/ingester.py (_LICENCE_ALIASES) — the
# worker image doesn't ship this package. tests/test_licence.py asserts the
# two tables are equal.
LICENCE_ALIASES: dict[str, tuple[str, str | None]] = {
    # Bare classes
    "own": (LICENCE_OWN, None),
    "open": (LICENCE_OPEN, None),
    "restricted": (LICENCE_RESTRICTED, None),
    "unknown": (LICENCE_UNKNOWN, None),
    # Own-work synonyms
    "self": (LICENCE_OWN, None),
    "internal": (LICENCE_OWN, None),
    "in-house": (LICENCE_OWN, None),
    "original": (LICENCE_OWN, None),
    # Undetermined synonyms
    "": (LICENCE_UNKNOWN, None),
    "undetermined": (LICENCE_UNKNOWN, None),
    "unclassified": (LICENCE_UNKNOWN, None),
    "tbd": (LICENCE_UNKNOWN, None),
    # Open — government and public-sector
    "ogl": (LICENCE_OPEN, "OGL v3.0"),
    "ogl-3": (LICENCE_OPEN, "OGL v3.0"),
    "ogl-3.0": (LICENCE_OPEN, "OGL v3.0"),
    "ogl-uk-3.0": (LICENCE_OPEN, "OGL v3.0"),
    "open-government-licence": (LICENCE_OPEN, "OGL v3.0"),
    "public-domain": (LICENCE_OPEN, "Public domain"),
    "pd": (LICENCE_OPEN, "Public domain"),
    # Open — Creative Commons (attribution / share-alike permit redistribution)
    "cc0": (LICENCE_OPEN, "CC0 1.0"),
    "cc0-1.0": (LICENCE_OPEN, "CC0 1.0"),
    "cc-by": (LICENCE_OPEN, "CC BY 4.0"),
    "cc-by-4.0": (LICENCE_OPEN, "CC BY 4.0"),
    "cc-by-3.0": (LICENCE_OPEN, "CC BY 3.0"),
    "cc-by-sa": (LICENCE_OPEN, "CC BY-SA 4.0"),
    "cc-by-sa-4.0": (LICENCE_OPEN, "CC BY-SA 4.0"),
    "cc-by-sa-3.0": (LICENCE_OPEN, "CC BY-SA 3.0"),
    # Open — permissive documentation / code licences
    "mit": (LICENCE_OPEN, "MIT"),
    "apache-2.0": (LICENCE_OPEN, "Apache 2.0"),
    "apache": (LICENCE_OPEN, "Apache 2.0"),
    "bsd": (LICENCE_OPEN, "BSD"),
    "bsd-2-clause": (LICENCE_OPEN, "BSD 2-Clause"),
    "bsd-3-clause": (LICENCE_OPEN, "BSD 3-Clause"),
    "gfdl": (LICENCE_OPEN, "GFDL"),
    # Restricted — Creative Commons variants that forbid commercial or
    # derivative use
    "cc-by-nc": (LICENCE_RESTRICTED, "CC BY-NC 4.0"),
    "cc-by-nc-4.0": (LICENCE_RESTRICTED, "CC BY-NC 4.0"),
    "cc-by-nd": (LICENCE_RESTRICTED, "CC BY-ND 4.0"),
    "cc-by-nd-4.0": (LICENCE_RESTRICTED, "CC BY-ND 4.0"),
    "cc-by-nc-sa": (LICENCE_RESTRICTED, "CC BY-NC-SA 4.0"),
    "cc-by-nc-sa-4.0": (LICENCE_RESTRICTED, "CC BY-NC-SA 4.0"),
    "cc-by-nc-nd": (LICENCE_RESTRICTED, "CC BY-NC-ND 4.0"),
    "cc-by-nc-nd-4.0": (LICENCE_RESTRICTED, "CC BY-NC-ND 4.0"),
    # Restricted — everything else
    "all-rights-reserved": (LICENCE_RESTRICTED, "All rights reserved"),
    "arr": (LICENCE_RESTRICTED, "All rights reserved"),
    "copyright": (LICENCE_RESTRICTED, "All rights reserved"),
    "proprietary": (LICENCE_RESTRICTED, "Proprietary"),
    "commercial": (LICENCE_RESTRICTED, "Commercial"),
    "paywalled": (LICENCE_RESTRICTED, "Paywalled"),
    "crown-copyright": (LICENCE_RESTRICTED, "Crown copyright (not under OGL)"),
}

# Namespaces whose content is written by the agent or the human in
# conversation, and therefore ours by default.
_CONVERSATION_NAMESPACES = frozenset({"episodic", "project", "preference"})


def resolve_licence(raw: str | None) -> tuple[str, str | None]:
    """Resolve a declared licence to ``(class, note)``.

    Accepts a class name or any identifier in LICENCE_ALIASES, case- and
    separator-insensitively. Returns the class and the canonical identifier
    to store as ``licence_note`` (None when the input was already a bare
    class). Raises ValueError for anything unrecognised — a guess here is a
    misclassification waiting to be sold.
    """
    try:
        return LICENCE_ALIASES[normalise_alias_key(raw)]
    except KeyError:
        raise ValueError(
            f"Unrecognised licence {raw!r}. Use one of "
            f"{', '.join(LICENCE_CLASSES)}, or a known identifier such as "
            "'ogl-3.0', 'cc-by-4.0' or 'all-rights-reserved'."
        ) from None


def validate_licence_class(value: str) -> str:
    """Accept only a bare class (for stored values and filters)."""
    if value not in LICENCE_CLASSES:
        raise ValueError(
            f"Invalid licence class {value!r}. Must be one of "
            f"{', '.join(LICENCE_CLASSES)}."
        )
    return value


def validate_licence_note(note: str | None) -> str | None:
    """Trim and bound a free-text licence note; empty becomes None."""
    if note is None:
        return None
    note = " ".join(str(note).split())
    if not note:
        return None
    if len(note) > MAX_LICENCE_NOTE:
        raise ValueError(f"licence note must be {MAX_LICENCE_NOTE} characters or fewer")
    return note


def default_licence(namespace: str) -> str:
    """Licence stamped on a write that declares none.

    Conversation-sourced namespaces are our own work. Knowledge is the one
    namespace that routinely holds third-party material — an article, a
    summary of a page — so an undeclared write there is honestly unknown
    rather than optimistically ours.
    """
    return LICENCE_OWN if namespace in _CONVERSATION_NAMESPACES else LICENCE_UNKNOWN


def licence_for_write(raw: str | None, namespace: str) -> dict[str, str]:
    """Licence hash fields for a new write: the declared licence, or the
    namespace default. An empty string means "not given" — clients that
    serialise unset optionals as "" must get the default, not unknown.
    Shared by the remember() tools and the web UI's create form."""
    if not raw:
        return licence_fields(default_licence(namespace))
    return licence_fields(*resolve_licence(raw))


def effective_licence(doc: dict, namespace: str) -> str:
    """The licence a stored record has, or would have had.

    A read-time fallback for records the backfill has not reached — written
    by a worker image that predates the field during a rolling upgrade, or
    read between an upgrade and the restart that backfills. Mirrors
    migrate_licence exactly, so a record reports the same value before and
    after the backfill: an article or an imported memory is unknown, a
    conversation write or a fact extracted from one is own.
    An out-of-vocabulary stored value reads as absent rather than passing
    through, so a filter can never fail to match what recall reports.
    """
    stored = doc.get("licence")
    if stored in LICENCE_CLASSES:
        return stored
    if doc.get("feed_name") or doc.get("imported_at"):
        return LICENCE_UNKNOWN
    # An extracted fact's only possible source is a conversation write
    # (knowledge is never enriched), which is own — the backfill inherits
    # that, and falls back to own when the source is gone.
    if doc.get("enriched_from"):
        return LICENCE_OWN
    if namespace == "knowledge":
        return LICENCE_UNKNOWN
    return LICENCE_OWN


def note_for_reclassification(
    old_class: str, old_note: str | None, new_class: str, submitted: str | None,
) -> str | None:
    """Drop a pre-filled note that belonged to the class being replaced.

    The web forms pre-fill the current note, so switching Open → Restricted
    and pressing Save would otherwise carry "CC BY 4.0" onto the restricted
    record. A note the human actually typed (different from what was
    pre-filled) is kept, and so is the note when the class is unchanged.
    Both the memory detail form and the feed editor use this.
    """
    if submitted and new_class != (old_class or "") and submitted == (old_note or ""):
        return None
    return submitted


def licence_fields(
    licence: str, note: str | None = None
) -> dict[str, str]:
    """Hash fields for a resolved licence, ready to merge into a write."""
    fields = {"licence": validate_licence_class(licence)}
    note = validate_licence_note(note)
    if note:
        fields["licence_note"] = note
    return fields



def classify_memories(
    store, keys: list[str], licence: str, note: str | None = None,
) -> dict[str, list[str]]:
    """Record a licence on stored memories and cascade it to their facts.

    The single write path behind the set_licence tool and the web UI's
    detail form, so the two cannot drift. A reclassification must not leave
    the old licence's note behind (a record marked restricted with
    "CC BY 4.0" beside it), so an absent note is written as an empty string
    — a bulk HSET cannot drop a field. See memory/lineage.py for the cascade
    and the deliberate absence of an ``updated_at`` bump.
    """
    fields = licence_fields(licence, note)
    fields.setdefault("licence_note", "")
    return stamp_lineage(store, keys, fields)
