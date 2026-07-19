"""Skill transfer: export a compiled skill plus its source memories as a zip,
and import such a bundle into another instance without touching what it
already holds.

The bundle is self-describing and integrity-checked: a manifest carries the
format version and a sha256 for every payload file, the skill travels both as
machine fields (skill.json) and as the human-readable SKILL.md, and each
source memory is one JSON file. Import is strictly additive — an existing key
is never overwritten, so replaying a bundle (or importing into a store that
already has some of the memories) can only add what's missing. Vectors never
travel: embeddings are regenerated on the importing instance, so bundles stay
portable across embedding models and instances.

Shared by the web UI export/import routes; lives in the memory package beside
skill_compiler.py so the logic is testable without the web stack.
"""

import hashlib
import io
import json
import logging
import re
import time
import zipfile
from typing import Any

from .skills import (
    SKILL_KEY_PREFIX,
    discovery_text,
    generated_skill_key,
    validate_domain,
)
from .version import __version__

logger = logging.getLogger(__name__)

EXPORT_FORMAT = "omnimem-skill-export"
EXPORT_FORMAT_VERSION = 1

# Bounds for an acceptable bundle. Generous against real skills (a body caps
# at 100KB and a memory at 50KB) while keeping a hostile zip from ballooning.
_MAX_ZIP_BYTES = 20 * 1024 * 1024
_MAX_TOTAL_UNCOMPRESSED = 50 * 1024 * 1024
_MAX_ENTRY_UNCOMPRESSED = 1024 * 1024
_MAX_MEMORIES = 500
_MAX_FIELDS_PER_MEMORY = 64
_MAX_FIELD_NAME = 64
_MAX_FIELD_VALUE = 100_000
_MAX_CONTENT = 50_000
_MAX_SKILL_BODY = 100_000

# Source memories a skill can cite: episodic experience/graveyard entries and
# promoted knowledge articles. Nothing else belongs in a skill bundle.
_MEMORY_KEY_RE = re.compile(
    r"^mem:(episodic|knowledge):[0-9A-Za-z][0-9A-Za-z._\-]{0,80}$"
)

# Per-instance telemetry and binary data never travel in a bundle.
_INSTANCE_LOCAL_FIELDS = ("vector", "recall_count", "last_recalled")

_MEMORY_ENTRY_RE = re.compile(r"^memories/[0-9]{4}\.json$")


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _strip_instance_local(fields: dict[str, Any]) -> dict[str, str]:
    return {
        k: v for k, v in fields.items()
        if k not in _INSTANCE_LOCAL_FIELDS and isinstance(v, str)
    }


def _parse_source_manifest(raw: Any) -> list[str]:
    try:
        parsed = json.loads(raw) if raw else []
    except (json.JSONDecodeError, TypeError):
        return []
    if not isinstance(parsed, list):
        return []
    return [k for k in parsed if isinstance(k, str)]


def build_skill_export(store, key: str) -> tuple[dict[str, Any] | None, str | None]:
    """Bundle a skill and its source memories into zip bytes.

    Returns (bundle, error) where bundle is {"data", "filename",
    "memory_count", "missing_sources"} on success.
    """
    if not key.startswith(SKILL_KEY_PREFIX):
        return None, "Not a skill key"
    skill = store.get(key)
    if skill is None:
        return None, "Skill not found"
    if skill.get("generated") != "true":
        return None, "Only generated skills can be exported"

    skill_fields = _strip_instance_local(skill)
    source_keys = sorted(set(_parse_source_manifest(skill.get("source_manifest"))))

    memories: list[dict[str, Any]] = []
    missing: list[str] = []
    if source_keys:
        rows = store.get_multi(source_keys)
        for src_key, row in zip(source_keys, rows):
            if row is None:
                missing.append(src_key)
                continue
            if not _MEMORY_KEY_RE.match(src_key):
                # A manifest citing an unexpected namespace is a store anomaly;
                # leave it out rather than produce an unimportable bundle.
                missing.append(src_key)
                continue
            memories.append({"key": src_key, "fields": _strip_instance_local(row)})

    files: dict[str, bytes] = {
        "skill.json": json.dumps(
            skill_fields, indent=2, ensure_ascii=False, sort_keys=True
        ).encode("utf-8"),
        "SKILL.md": skill_fields.get("body", "").encode("utf-8"),
    }
    for i, mem in enumerate(memories):
        files[f"memories/{i:04d}.json"] = json.dumps(
            mem, indent=2, ensure_ascii=False, sort_keys=True
        ).encode("utf-8")

    manifest = {
        "format": EXPORT_FORMAT,
        "format_version": EXPORT_FORMAT_VERSION,
        "exported_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "omnimem_version": __version__,
        "skill_key": key,
        "name": skill_fields.get("name", ""),
        "domain": skill_fields.get("domain", ""),
        "user": skill_fields.get("user", ""),
        "memory_count": len(memories),
        "missing_sources": missing,
        "checksums": {name: _sha256(data) for name, data in files.items()},
    }

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(
            "manifest.json",
            json.dumps(manifest, indent=2, ensure_ascii=False),
        )
        for name, data in files.items():
            zf.writestr(name, data)

    safe_name = re.sub(r"[^0-9A-Za-z._\-]", "_", skill_fields.get("name") or "skill")
    filename = f"omnimem_skill_{safe_name}_{time.strftime('%Y%m%d_%H%M%S')}.zip"
    logger.info(
        "Exported skill %s (%d source memories, %d missing)",
        key, len(memories), len(missing),
    )
    return {
        "data": buf.getvalue(),
        "filename": filename,
        "memory_count": len(memories),
        "missing_sources": missing,
    }, None


def _read_entry(zf: zipfile.ZipFile, name: str) -> bytes | None:
    try:
        return zf.read(name)
    except (KeyError, RuntimeError, zipfile.BadZipFile, OSError):
        return None


def _validate_memory(raw: bytes, entry_name: str) -> tuple[dict[str, Any] | None, str | None]:
    try:
        parsed = json.loads(raw.decode("utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError):
        return None, f"{entry_name} is not valid JSON"
    if not isinstance(parsed, dict):
        return None, f"{entry_name} must be a JSON object"

    key = parsed.get("key")
    fields = parsed.get("fields")
    if not isinstance(key, str) or not _MEMORY_KEY_RE.match(key):
        return None, f"{entry_name} has an invalid memory key"
    if not isinstance(fields, dict):
        return None, f"{entry_name} has no fields object"
    if len(fields) > _MAX_FIELDS_PER_MEMORY:
        return None, f"{entry_name} has too many fields (max {_MAX_FIELDS_PER_MEMORY})"

    for fname, value in fields.items():
        if not isinstance(fname, str) or not fname or len(fname) > _MAX_FIELD_NAME:
            return None, f"{entry_name} has an invalid field name"
        if not isinstance(value, str):
            return None, f"{entry_name} field '{fname}' is not a string"
        if len(value) > _MAX_FIELD_VALUE:
            return None, f"{entry_name} field '{fname}' is too large"

    content = fields.get("content", "")
    if not content.strip():
        return None, f"{entry_name} has no content — nothing to embed on import"
    if len(content) > _MAX_CONTENT:
        return None, f"{entry_name} content exceeds {_MAX_CONTENT} chars"

    return {"key": key, "fields": _strip_instance_local(fields)}, None


def validate_skill_import(data: bytes) -> dict[str, Any]:
    """Validate uploaded bundle bytes without touching the store.

    Returns {"ok": True, "skill_key", "skill_fields", "memories", "manifest",
    "warnings"} or {"ok": False, "error"}. Every check runs before anything
    is trusted: structure, size bounds, checksums, key shapes, field types.
    """
    def fail(error: str) -> dict[str, Any]:
        return {"ok": False, "error": error}

    if not data:
        return fail("The uploaded file is empty")
    if len(data) > _MAX_ZIP_BYTES:
        return fail(f"Bundle too large (max {_MAX_ZIP_BYTES // (1024 * 1024)} MB)")

    try:
        zf = zipfile.ZipFile(io.BytesIO(data))
    except zipfile.BadZipFile:
        return fail("Not a valid zip file")

    with zf:
        infos = [i for i in zf.infolist() if not i.is_dir()]
        names = [i.filename for i in infos]
        if len(names) != len(set(names)):
            return fail("Bundle contains duplicate entries")

        total = 0
        for info in infos:
            if info.flag_bits & 0x1:
                return fail("Encrypted zip entries are not supported")
            if info.file_size > _MAX_ENTRY_UNCOMPRESSED:
                return fail(f"Bundle entry {info.filename} is too large")
            total += info.file_size
        if total > _MAX_TOTAL_UNCOMPRESSED:
            return fail("Bundle expands too large")

        payload_names = []
        for name in names:
            if name == "manifest.json":
                continue
            if name in ("skill.json", "SKILL.md") or _MEMORY_ENTRY_RE.match(name):
                payload_names.append(name)
            else:
                return fail(f"Unexpected entry in bundle: {name}")

        if "manifest.json" not in names:
            return fail("Bundle has no manifest.json — not an OmniMem skill export")
        if "skill.json" not in names or "SKILL.md" not in names:
            return fail("Bundle is missing skill.json or SKILL.md")

        raw_manifest = _read_entry(zf, "manifest.json")
        if raw_manifest is None:
            return fail("Could not read manifest.json")
        try:
            manifest = json.loads(raw_manifest.decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError):
            return fail("manifest.json is not valid JSON")
        if not isinstance(manifest, dict):
            return fail("manifest.json must be a JSON object")

        if manifest.get("format") != EXPORT_FORMAT:
            return fail("Not an OmniMem skill export (wrong format marker)")
        if manifest.get("format_version") != EXPORT_FORMAT_VERSION:
            return fail(
                "Unsupported export format version "
                f"{manifest.get('format_version')!r} "
                f"(this instance reads version {EXPORT_FORMAT_VERSION})"
            )

        checksums = manifest.get("checksums")
        if not isinstance(checksums, dict):
            return fail("manifest.json has no checksums")
        for name in payload_names:
            expected = checksums.get(name)
            raw = _read_entry(zf, name)
            if raw is None:
                return fail(f"Could not read {name}")
            if not isinstance(expected, str) or _sha256(raw) != expected:
                return fail(f"Checksum mismatch on {name} — the bundle is corrupt "
                            "or was modified after export")

        raw_skill = _read_entry(zf, "skill.json")
        try:
            skill_fields = json.loads(raw_skill.decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError):
            return fail("skill.json is not valid JSON")
        if not isinstance(skill_fields, dict):
            return fail("skill.json must be a JSON object")
        for fname, value in skill_fields.items():
            if not isinstance(fname, str) or not isinstance(value, str):
                return fail("skill.json fields must all be strings")
            if fname != "body" and len(value) > _MAX_FIELD_VALUE:
                return fail(f"skill.json field '{fname}' is too large")

        domain = skill_fields.get("domain", "")
        user = skill_fields.get("user", "")
        try:
            validate_domain(domain)
            validate_domain(user)
        except ValueError:
            return fail("skill.json has an invalid domain or user")
        if not skill_fields.get("name"):
            return fail("skill.json has no name")
        if skill_fields.get("generated") != "true":
            return fail("Only generated skills can be imported — the bundle's "
                        "skill is not flagged generated:true")

        body = skill_fields.get("body", "")
        if not body.strip():
            return fail("skill.json has an empty body")
        if len(body) > _MAX_SKILL_BODY:
            return fail(f"Skill body exceeds {_MAX_SKILL_BODY} chars")

        skill_md = _read_entry(zf, "SKILL.md")
        if skill_md is None or skill_md.decode("utf-8", errors="replace") != body:
            return fail("SKILL.md does not match the skill body in skill.json")

        skill_key = generated_skill_key(domain, user)
        if manifest.get("skill_key") != skill_key:
            return fail("manifest skill_key does not match the skill's "
                        "domain and user")

        for manifest_field in ("rule_manifest", "source_manifest"):
            raw_field = skill_fields.get(manifest_field)
            if raw_field:
                try:
                    if not isinstance(json.loads(raw_field), list):
                        return fail(f"skill.json {manifest_field} is not a list")
                except json.JSONDecodeError:
                    return fail(f"skill.json {manifest_field} is not valid JSON")

        memory_names = sorted(n for n in payload_names if n.startswith("memories/"))
        if len(memory_names) > _MAX_MEMORIES:
            return fail(f"Bundle carries too many memories (max {_MAX_MEMORIES})")

        memories: list[dict[str, Any]] = []
        seen_keys: set[str] = set()
        for name in memory_names:
            mem, err = _validate_memory(_read_entry(zf, name), name)
            if err:
                return fail(err)
            if mem["key"] in seen_keys:
                return fail(f"Bundle contains memory key {mem['key']} twice")
            seen_keys.add(mem["key"])
            memories.append(mem)

    warnings: list[str] = []
    if manifest.get("memory_count") != len(memories):
        return {"ok": False, "error": "manifest memory_count does not match the "
                                      "bundled memories"}
    declared_sources = set(_parse_source_manifest(skill_fields.get("source_manifest")))
    unlisted = seen_keys - declared_sources
    if unlisted:
        return {"ok": False, "error": "Bundle carries memories the skill's "
                                      "source_manifest does not cite"}
    absent = sorted(declared_sources - seen_keys)
    if absent:
        warnings.append(
            f"{len(absent)} cited source memor"
            f"{'y is' if len(absent) == 1 else 'ies are'} not in the bundle "
            "(missing on the exporting instance); the skill will cite "
            "keys that may not resolve here"
        )
    missing_sources = manifest.get("missing_sources")
    if isinstance(missing_sources, list) and missing_sources:
        warnings.append(
            f"The exporting instance already reported {len(missing_sources)} "
            "source memories missing at export time"
        )

    return {
        "ok": True,
        "skill_key": skill_key,
        "skill_fields": skill_fields,
        "memories": memories,
        "manifest": {
            "exported_at": manifest.get("exported_at", ""),
            "omnimem_version": manifest.get("omnimem_version", ""),
            "name": skill_fields.get("name", ""),
            "domain": domain,
            "user": user,
        },
        "warnings": warnings,
    }


def plan_skill_import(store, bundle: dict[str, Any]) -> dict[str, Any]:
    """Preview what applying a validated bundle would do — read-only."""
    skill_key = bundle["skill_key"]
    skill_exists = store.get(skill_key) is not None

    new_memories: list[str] = []
    existing_memories: list[str] = []
    for mem in bundle["memories"]:
        if store.get(mem["key"]) is not None:
            existing_memories.append(mem["key"])
        else:
            new_memories.append(mem["key"])

    return {
        "skill_key": skill_key,
        "skill_exists": skill_exists,
        "new_memories": new_memories,
        "existing_memories": existing_memories,
    }


def apply_skill_import(store, embedder, bundle: dict[str, Any]) -> dict[str, Any]:
    """Write a validated bundle into the store. Strictly additive.

    Existence is re-checked per key at write time (not trusted from the
    preview), so nothing already stored is ever overwritten — replaying a
    bundle is a no-op. New memories are re-embedded from their content and
    the skill from its discovery metadata, both on this instance's embedder.
    """
    now = str(time.time())
    memories_written: list[str] = []
    memories_skipped: list[str] = []

    for mem in bundle["memories"]:
        key = mem["key"]
        if store.get(key) is not None:
            memories_skipped.append(key)
            continue
        fields = dict(mem["fields"])
        fields.setdefault("state", "active")
        fields["imported_at"] = now
        namespace = key.split(":")[1]
        vector = embedder.embed(fields["content"])
        store.upsert(namespace, key, fields, vector)
        memories_written.append(key)

    skill_key = bundle["skill_key"]
    skill_written = False
    if store.get(skill_key) is None:
        fields = dict(bundle["skill_fields"])
        fields.setdefault("state", "active")
        fields["imported_at"] = now
        vector = embedder.embed(discovery_text(
            fields.get("name", ""), fields.get("description", ""),
            fields.get("domain", ""),
        ))
        store.upsert("skill", skill_key, fields, vector)
        skill_written = True

    logger.info(
        "Imported skill bundle %s: skill %s, %d memories written, %d skipped",
        skill_key, "written" if skill_written else "already present (untouched)",
        len(memories_written), len(memories_skipped),
    )
    return {
        "skill_key": skill_key,
        "skill_written": skill_written,
        "memories_written": memories_written,
        "memories_skipped": memories_skipped,
    }
