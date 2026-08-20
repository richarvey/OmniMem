"""Valkey connection and vector operations."""

import json
import logging
import os
import re
import time
from typing import Any

import numpy as np
import valkey
from valkey.commands.search.field import NumericField, TagField, VectorField
from valkey.commands.search.indexDefinition import IndexDefinition, IndexType
from valkey.commands.search.query import Query

logger = logging.getLogger(__name__)

# Valid key prefixes to prevent writing to arbitrary Valkey keys
_VALID_KEY_PREFIXES = (
    "mem:episodic:", "mem:project:", "mem:knowledge:", "mem:preference:",
    "mem:skill:",
    "topics:", "log:recall:", "meta:", "qexp:", "queue:",
)

# Valid namespace names for search index lookups
_VALID_NAMESPACES = {"episodic", "project", "knowledge", "preference", "skill"}

VECTOR_DIM = 384
VECTOR_ALGORITHM = "HNSW"
DISTANCE_METRIC = "COSINE"

# Fields returned per namespace to avoid fetching unnecessary data.
# GOTCHA: this acts as a whitelist — a field stored in the hash but missing
# here is silently absent from search results. The knowledge namespace once
# lacked "project", which made recall's project_filter drop every knowledge
# result without a trace (issue #20).
_NAMESPACE_RETURN_FIELDS: dict[str, tuple[str, ...]] = {
    "episodic": (
        "similarity_score", "content", "project", "state", "surface_score",
        "created_at", "updated_at", "tags", "deprioritised_reason",
        "reinstate_hints", "effort_score", "outcome", "iterations",
        "abandoned_approaches", "breakthrough", "gotchas", "experience_weight",
        "contradictions", "recall_count", "last_recalled", "event_date",
        "enriched_from",
    ),
    "project": (
        "similarity_score", "content", "project_name", "stack", "state",
        "surface_score", "created_at", "updated_at", "recall_count", "last_recalled",
        "domains",
    ),
    "knowledge": (
        "similarity_score", "content", "source_url", "feed_name", "published_at",
        "topics", "state", "surface_score", "created_at", "updated_at",
        "recall_count", "last_recalled", "expires_at",
        "project", "event_date", "tags", "enriched_from",
    ),
    "preference": (
        "similarity_score", "content", "project", "scope", "state",
        "surface_score", "created_at", "updated_at", "tags",
        "recall_count", "last_recalled", "source_doc_id",
        "event_date", "enriched_from",
    ),
    # Skills are whole-document objects: search returns discovery metadata
    # only, never the body. The canonical body is fetched intact by ID via
    # get_skill() — chunking or projecting it through search would hand the
    # agent a fragment of a procedure that only works whole.
    "skill": (
        "similarity_score", "name", "description", "domain", "user",
        "state", "generated", "surface_score", "compiled_at",
        "contract_version", "created_at", "updated_at",
        "recall_count", "last_recalled",
    ),
}

INDEX_DEFINITIONS: dict[str, dict[str, Any]] = {
    "idx:episodic": {
        "prefix": "mem:episodic:",
        "fields": [
            VectorField(
                "vector",
                VECTOR_ALGORITHM,
                {"TYPE": "FLOAT32", "DIM": VECTOR_DIM, "DISTANCE_METRIC": DISTANCE_METRIC},
            ),
            TagField("project"),
            TagField("state"),
            NumericField("surface_score"),
            NumericField("created_at"),
            NumericField("updated_at"),
            TagField("tags"),
            NumericField("effort_score"),
            TagField("outcome"),
            NumericField("iterations"),
            NumericField("experience_weight"),
            NumericField("recall_count"),
        ],
    },
    "idx:project": {
        "prefix": "mem:project:",
        "fields": [
            VectorField(
                "vector",
                VECTOR_ALGORITHM,
                {"TYPE": "FLOAT32", "DIM": VECTOR_DIM, "DISTANCE_METRIC": DISTANCE_METRIC},
            ),
            TagField("project_name"),
            TagField("stack"),
            TagField("state"),
            NumericField("surface_score"),
            NumericField("created_at"),
            NumericField("updated_at"),
            NumericField("recall_count"),
            # Work-type domains (v6.6), sharing the compiled-skill vocabulary.
            # Stored comma-separated, which is what a TAG field actually
            # tokenises on — the JSON-array form used by `tags`/`topics`
            # indexes as unusable tokens. Startup index migration picks the
            # new field up automatically.
            TagField("domains"),
        ],
    },
    "idx:knowledge": {
        "prefix": "mem:knowledge:",
        "fields": [
            VectorField(
                "vector",
                VECTOR_ALGORITHM,
                {"TYPE": "FLOAT32", "DIM": VECTOR_DIM, "DISTANCE_METRIC": DISTANCE_METRIC},
            ),
            TagField("feed_name"),
            NumericField("published_at"),
            TagField("topics"),
            TagField("state"),
            NumericField("surface_score"),
            NumericField("created_at"),
            NumericField("updated_at"),
            NumericField("recall_count"),
            NumericField("expires_at"),
            # Extracted facts land here scoped to a project (issue #20);
            # indexed so the recall project filter can be pushed down.
            # Startup index migration picks up the new field automatically.
            TagField("project"),
        ],
    },
    "idx:preference": {
        "prefix": "mem:preference:",
        "fields": [
            VectorField(
                "vector",
                VECTOR_ALGORITHM,
                {"TYPE": "FLOAT32", "DIM": VECTOR_DIM, "DISTANCE_METRIC": DISTANCE_METRIC},
            ),
            TagField("project"),
            TagField("scope"),
            TagField("state"),
            TagField("tags"),
            NumericField("surface_score"),
            NumericField("created_at"),
            NumericField("updated_at"),
            NumericField("recall_count"),
        ],
    },
    # Compiled skills (v6). The vector embeds discovery metadata (name +
    # description + domain), not the body — find_skills and the briefing
    # decide relevance from it, then get_skill returns the body intact.
    "idx:skill": {
        "prefix": "mem:skill:",
        "fields": [
            VectorField(
                "vector",
                VECTOR_ALGORITHM,
                {"TYPE": "FLOAT32", "DIM": VECTOR_DIM, "DISTANCE_METRIC": DISTANCE_METRIC},
            ),
            TagField("domain"),
            TagField("user"),
            TagField("state"),
            TagField("generated"),
            NumericField("surface_score"),
            NumericField("compiled_at"),
            NumericField("contract_version"),
            NumericField("created_at"),
            NumericField("updated_at"),
            NumericField("recall_count"),
        ],
    },
}


class ValkeyStore:
    """Manages Valkey connection, vector indexes, and CRUD operations."""

    def __init__(self) -> None:
        self._client: valkey.Valkey | None = None
        # Lazily-created second client with decode_responses=False, used only
        # to read the binary vector field (the main client would try to UTF-8
        # decode it). Lets dedup/maintenance reuse stored embeddings instead
        # of re-embedding whole namespaces on every scan.
        self._raw_client: valkey.Valkey | None = None

    def connect(self, max_retries: int = 10, retry_delay: float = 2.0) -> None:
        """Connect to Valkey with connection pooling and retry logic on startup."""
        host = os.getenv("VALKEY_HOST", "valkey")
        port = int(os.getenv("VALKEY_PORT", "6379"))
        password = os.getenv("VALKEY_PASSWORD", "")

        for attempt in range(1, max_retries + 1):
            try:
                pool = valkey.ConnectionPool(
                    host=host,
                    port=port,
                    password=password,
                    decode_responses=True,
                    max_connections=int(os.getenv("VALKEY_MAX_CONNECTIONS", "20")),
                )
                self._client = valkey.Valkey(connection_pool=pool)
                self._client.ping()
                logger.info("Connected to Valkey at %s:%d (pooled)", host, port)
                self._ensure_indexes()
                return
            except (valkey.ConnectionError, valkey.TimeoutError) as exc:
                logger.warning(
                    "Valkey connection attempt %d/%d failed: %s",
                    attempt,
                    max_retries,
                    exc,
                )
                if attempt == max_retries:
                    raise
                time.sleep(retry_delay)

    @property
    def client(self) -> valkey.Valkey:
        if self._client is None:
            raise RuntimeError("ValkeyStore not connected. Call connect() first.")
        return self._client

    @property
    def raw_client(self) -> valkey.Valkey:
        """Binary-safe client (decode_responses=False) for reading vectors."""
        if self._raw_client is None:
            pool = valkey.ConnectionPool(
                host=os.getenv("VALKEY_HOST", "valkey"),
                port=int(os.getenv("VALKEY_PORT", "6379")),
                password=os.getenv("VALKEY_PASSWORD", ""),
                decode_responses=False,
                max_connections=int(os.getenv("VALKEY_RAW_MAX_CONNECTIONS", "4")),
            )
            self._raw_client = valkey.Valkey(connection_pool=pool)
        return self._raw_client

    def _migrate_indexes(self) -> None:
        """Drop indexes whose field count doesn't match the definition.

        This is data-safe — dropping an index removes only the index, not the
        underlying hashes. _ensure_indexes() then recreates it with the new
        fields, and the search module re-indexes existing hashes automatically.

        GOTCHA (fixed in 6.6.0, silently broken before it): do NOT use
        valkey-py's `client.ft(name).dropindex()` here. It appends its
        delete-documents flag as a positional argument even when False, so the
        wire command is `FT.DROPINDEX <index> ""` — three arguments. RediSearch
        tolerates the trailing empty string; valkey-search rejects it with
        "wrong number of arguments". That ResponseError used to be caught by
        the same `except` that handles a missing index, so every migration
        failed silently and every upgraded instance kept a stale index while
        the logs said nothing. The drop now has its own error handling, and a
        failure is logged as an error rather than swallowed.
        """
        for idx_name, idx_def in INDEX_DEFINITIONS.items():
            try:
                info = self.client.ft(idx_name).info()
            except valkey.ResponseError:
                continue  # Index doesn't exist yet — _ensure_indexes creates it

            existing_count = len(info.get("attributes", []))
            expected_count = len(idx_def["fields"])
            if existing_count >= expected_count:
                continue

            logger.info(
                "Index %s has %d fields, expected %d — dropping for recreation",
                idx_name, existing_count, expected_count,
            )
            try:
                self.client.execute_command("FT.DROPINDEX", idx_name)
            except valkey.ResponseError as exc:
                logger.error(
                    "Could not drop index %s for migration (%s) — it will keep "
                    "its old fields and any new ones will not be searchable",
                    idx_name, exc,
                )

    def _ensure_indexes(self) -> None:
        """Create vector indexes if they don't already exist."""
        self._migrate_indexes()
        for idx_name, idx_def in INDEX_DEFINITIONS.items():
            try:
                self.client.ft(idx_name).info()
                logger.info("Index %s already exists", idx_name)
            except valkey.ResponseError:
                definition = IndexDefinition(
                    prefix=[idx_def["prefix"]],
                    index_type=IndexType.HASH,
                )
                self.client.ft(idx_name).create_index(
                    idx_def["fields"],
                    definition=definition,
                )
                logger.info("Created index %s on prefix %s", idx_name, idx_def["prefix"])

    def _validate_key(self, key: str) -> None:
        """Ensure the key starts with a valid prefix."""
        if not any(key.startswith(p) for p in _VALID_KEY_PREFIXES):
            raise ValueError(f"Invalid key prefix: {key[:30]}")

    def upsert(
        self, namespace: str, key: str, fields: dict[str, Any], vector: np.ndarray
    ) -> None:
        """Store a hash with an embedded vector."""
        self._validate_key(key)
        data = {k: v for k, v in fields.items() if v is not None}
        data["vector"] = vector.astype(np.float32).tobytes()
        self.client.hset(key, mapping=data)
        logger.debug("Upserted key %s in namespace %s", key, namespace)

    def search(
        self,
        namespace: str,
        vector: np.ndarray,
        top_k: int = 10,
        filter_expr: str | None = None,
    ) -> list[dict[str, Any]]:
        """Vector similarity search within a namespace index."""
        if namespace not in _VALID_NAMESPACES:
            raise ValueError(f"Invalid namespace: {namespace}")

        # Clamp top_k to prevent excessive result sets
        top_k = max(1, min(top_k, 100))

        idx_name = f"idx:{namespace}"
        blob = vector.astype(np.float32).tobytes()

        # Sanitize filter_expr: only allow known safe RediSearch filter syntax
        # (tag filters like @state:{active}, numeric filters, boolean operators)
        if filter_expr:
            # Reject characters that could break out of the query context
            if any(c in filter_expr for c in [";", "\n", "\r", "\x00"]):
                logger.warning("Rejected filter_expr with suspicious characters: %s", filter_expr[:100])
                filter_expr = None

        filter_clause = filter_expr if filter_expr else "*"
        query_str = f"({filter_clause})=>[KNN {top_k} @vector $vec AS similarity_score]"

        # Only request fields relevant to this namespace
        return_fields = _NAMESPACE_RETURN_FIELDS.get(namespace, (
            "similarity_score", "content", "state", "surface_score",
            "created_at", "updated_at",
        ))

        def _run(qstr: str):
            q = (
                Query(qstr)
                .return_fields(*return_fields)
                .paging(0, top_k)
                .dialect(2)
            )
            return self.client.ft(idx_name).search(q, query_params={"vec": blob})

        try:
            results = _run(query_str)
        except valkey.ResponseError as exc:
            if filter_expr:
                # A bad filter must degrade to an unfiltered search, not an
                # empty result set — callers re-filter in Python anyway.
                logger.warning(
                    "Filtered search failed on %s (%s) — retrying unfiltered",
                    idx_name, exc,
                )
                try:
                    results = _run(
                        f"(*)=>[KNN {top_k} @vector $vec AS similarity_score]"
                    )
                except valkey.ResponseError as exc2:
                    logger.error("Search failed on %s: %s", idx_name, exc2)
                    return []
            else:
                logger.error("Search failed on %s: %s", idx_name, exc)
                return []

        output = []
        for doc in results.docs:
            # Use __dict__ directly instead of dir() to avoid iterating over
            # all class/instance attributes and calling callable() on each.
            entry: dict[str, Any] = {"key": doc.id}
            for attr, val in doc.__dict__.items():
                if attr.startswith("_") or attr in ("id", "payload"):
                    continue
                if val is not None:
                    entry[attr] = val
            output.append(entry)
        return output

    def get(self, key: str) -> dict[str, Any] | None:
        """Retrieve all text fields for a key (excludes binary vector)."""
        fields = self.client.hkeys(key)
        if not fields:
            return None
        text_fields = [f for f in fields if f != "vector"]
        if not text_fields:
            return None
        values = self.client.hmget(key, text_fields)
        return {f: v for f, v in zip(text_fields, values) if v is not None}

    def set_field(self, key: str, field: str, value: Any) -> None:
        """Update a single field without re-embedding."""
        self._validate_key(key)
        self.client.hset(key, field, value)

    def set_fields(self, key: str, mapping: dict[str, Any]) -> None:
        """Update multiple fields in a single round-trip."""
        self._validate_key(key)
        if mapping:
            self.client.hset(key, mapping=mapping)

    def set_fields_multi(
        self, keys: list[str], mapping: dict[str, Any], batch_size: int = 500
    ) -> int:
        """Apply the same field updates to many keys in pipelined batches.

        One HSET per key, flushed every ``batch_size`` keys so a large bulk
        update never builds an unbounded pipeline. Returns the number of keys
        written. Mirrors delete_many's batching for bulk state transitions.
        """
        if not keys or not mapping:
            return 0
        written = 0
        for i in range(0, len(keys), batch_size):
            batch = keys[i:i + batch_size]
            pipe = self.client.pipeline(transaction=False)
            for key in batch:
                self._validate_key(key)
                pipe.hset(key, mapping=mapping)
            pipe.execute()
            written += len(batch)
        return written

    def get_multi(self, keys: list[str]) -> list[dict[str, Any] | None]:
        """Retrieve all text fields for multiple keys (excludes binary vectors)."""
        if not keys:
            return []
        # Phase 1: get field names for all keys
        pipe = self.client.pipeline(transaction=False)
        for key in keys:
            pipe.hkeys(key)
        all_fields = pipe.execute()

        # Phase 2: fetch values for text fields only (skip binary vector)
        pipe = self.client.pipeline(transaction=False)
        fetch_indices: list[int] = []
        text_field_lists: list[list[str]] = []
        for i, fields in enumerate(all_fields):
            text_fields = [f for f in (fields or []) if f != "vector"]
            text_field_lists.append(text_fields)
            if text_fields:
                pipe.hmget(keys[i], text_fields)
                fetch_indices.append(i)

        fetched = pipe.execute() if fetch_indices else []

        results: list[dict[str, Any] | None] = [None] * len(keys)
        for fetch_pos, key_idx in enumerate(fetch_indices):
            values = fetched[fetch_pos]
            text_fields = text_field_lists[key_idx]
            data = {f: v for f, v in zip(text_fields, values) if v is not None}
            results[key_idx] = data if data else None
        return results

    def get_fields_multi(
        self, keys: list[str], fields: tuple[str, ...] | list[str]
    ) -> list[dict[str, Any] | None]:
        """Batch-fetch a fixed projection of fields for many keys.

        One pipelined HMGET per key in a single round-trip — half the trips of
        get_multi (which first HKEYS then HMGET) — and only pulls the named
        fields, so list/count/aggregate views don't drag back large fields like
        content, breakthrough, gotchas or abandoned_approaches they never use.

        Returns a list aligned with ``keys``; each entry is a dict of the
        requested fields that were present, or None if the key had none.
        """
        if not keys:
            return []
        fields = list(fields)
        pipe = self.client.pipeline(transaction=False)
        for key in keys:
            pipe.hmget(key, fields)
        rows = pipe.execute()

        results: list[dict[str, Any] | None] = []
        for row in rows:
            data = {f: v for f, v in zip(fields, row) if v is not None}
            results.append(data if data else None)
        return results

    def delete(self, key: str) -> None:
        """Hard delete a key."""
        self._validate_key(key)
        self.client.delete(key)
        logger.info("Deleted key %s", key)

    def delete_many(self, keys: list[str], batch_size: int = 500) -> int:
        """Hard delete many keys via pipelined batches.

        Bulk cleanup (delete_project) needs this — deleting tens of
        thousands of memories one MCP call at a time took ~45 minutes.

        Returns the number of keys deleted.
        """
        deleted = 0
        for key in keys:
            self._validate_key(key)
        for start in range(0, len(keys), batch_size):
            batch = keys[start:start + batch_size]
            pipe = self.client.pipeline(transaction=False)
            for key in batch:
                pipe.delete(key)
            pipe.execute()
            deleted += len(batch)
        logger.info("Bulk deleted %d keys", deleted)
        return deleted

    def count_records(self, namespace: str) -> int:
        """Count actual hash records for a namespace via SCAN.

        Authoritative — unlike FT.INFO num_docs, which can drift if the
        search index doesn't see hash deletions (e.g. when keyspace
        notifications are disabled).
        """
        if namespace not in _VALID_NAMESPACES:
            raise ValueError(f"Invalid namespace: {namespace}")
        return len(self.scan_prefix(f"mem:{namespace}:"))

    def count_all_records(self) -> dict[str, int]:
        """Record counts for every namespace from a single SCAN of mem:*.

        One pass over the keyspace instead of one full SCAN per namespace —
        health() uses this so its cost no longer scales with 4x the DB size.
        """
        counts = {ns: 0 for ns in _VALID_NAMESPACES}
        for key in self.scan_prefix("mem:"):
            parts = key.split(":", 2)
            if len(parts) >= 2 and parts[1] in counts:
                counts[parts[1]] += 1
        return counts

    def get_vectors_multi(self, keys: list[str]) -> list[np.ndarray | None]:
        """Batch-read stored embedding vectors via the binary-safe client.

        Returns float32 arrays aligned with ``keys`` (None where the key or
        vector is missing/malformed). Reusing stored vectors is dramatically
        cheaper than re-embedding content — dedup and maintenance scans went
        from a full model pass over the namespace to a single pipeline read.
        """
        if not keys:
            return []
        pipe = self.raw_client.pipeline(transaction=False)
        for key in keys:
            pipe.hget(key, "vector")
        rows = pipe.execute()

        expected = VECTOR_DIM * 4  # float32
        out: list[np.ndarray | None] = []
        for raw in rows:
            if raw is None or len(raw) != expected:
                out.append(None)
            else:
                out.append(np.frombuffer(raw, dtype=np.float32))
        return out

    def reindex_namespace(self, namespace: str) -> dict[str, int]:
        """Drop and recreate the search index for a namespace.

        Data-safe: dropindex() removes only the index, not the underlying
        hashes. Recreating it causes valkey-search to re-scan the prefix
        and rebuild from existing hashes only — phantom entries from
        previous deletes are dropped.

        Returns:
            Dict with before/after num_docs and actual record count.
        """
        if namespace not in _VALID_NAMESPACES:
            raise ValueError(f"Invalid namespace: {namespace}")

        idx_name = f"idx:{namespace}"
        idx_def = INDEX_DEFINITIONS[idx_name]

        before = 0
        try:
            info = self.client.ft(idx_name).info()
            before = int(info.get("num_docs", 0))
        except valkey.ResponseError:
            pass

        # Use execute_command directly. The symptom recorded here originally
        # was that .dropindex() left the index in place and create_index then
        # failed with "already exists"; the cause (pinned down in 6.6.0) is
        # that valkey-py appends its delete-documents flag positionally even
        # when False, so the command is `FT.DROPINDEX <index> ""` and
        # valkey-search rejects the third argument outright. See
        # _migrate_indexes, which had the same bug without the workaround.
        try:
            self.client.execute_command("FT.DROPINDEX", idx_name)
        except valkey.ResponseError as exc:
            msg = str(exc).lower()
            if "unknown index" in msg or "no such index" in msg:
                pass  # Already gone — fine
            else:
                logger.warning("FT.DROPINDEX %s failed: %s", idx_name, exc)
                raise

        definition = IndexDefinition(
            prefix=[idx_def["prefix"]],
            index_type=IndexType.HASH,
        )
        self.client.ft(idx_name).create_index(
            idx_def["fields"],
            definition=definition,
        )

        actual = self.count_records(namespace)

        # FT.INFO num_docs may take a moment to settle after recreate;
        # the actual record count is the source of truth.
        try:
            info = self.client.ft(idx_name).info()
            after = int(info.get("num_docs", 0))
        except valkey.ResponseError:
            after = actual

        logger.info(
            "Reindexed %s: num_docs %d -> %d (actual records: %d)",
            idx_name, before, after, actual,
        )
        return {
            "namespace": namespace,
            "before_num_docs": before,
            "after_num_docs": after,
            "actual_records": actual,
            "removed_phantoms": max(0, before - actual),
        }

    def scan_prefix(self, prefix: str) -> list[str]:
        """Return all keys matching a prefix using SCAN."""
        keys: list[str] = []
        cursor = 0
        while True:
            cursor, batch = self.client.scan(cursor=cursor, match=f"{prefix}*", count=500)
            keys.extend(batch)
            if cursor == 0:
                break
        return keys

    def dump_all(self) -> dict[str, dict[str, Any]]:
        """Export all mem:* and topics:* keys with all fields. Safe for large datasets."""
        result: dict[str, dict[str, Any]] = {}
        prefixes = ["mem:", "topics:", "log:recall:", "meta:"]
        for prefix in prefixes:
            keys = self.scan_prefix(prefix)
            if not keys:
                continue

            # Batch TYPE checks using a pipeline instead of N round-trips
            pipe = self.client.pipeline(transaction=False)
            for key in keys:
                pipe.type(key)
            types = pipe.execute()

            # Batch fetch: pipeline hgetall for hashes, smembers for sets
            hash_keys: list[str] = []
            set_keys: list[str] = []
            for key, key_type in zip(keys, types):
                if key_type == "hash":
                    hash_keys.append(key)
                elif key_type == "set":
                    set_keys.append(key)

            # Fetch all hashes in two pipeline phases (skip binary vector field)
            if hash_keys:
                # Phase 1: get field names
                pipe = self.client.pipeline(transaction=False)
                for key in hash_keys:
                    pipe.hkeys(key)
                all_fields = pipe.execute()

                # Phase 2: fetch text field values only
                pipe = self.client.pipeline(transaction=False)
                fetch_indices: list[int] = []
                text_field_lists: list[list[str]] = []
                for i, fields in enumerate(all_fields):
                    text_fields = [f for f in (fields or []) if f != "vector"]
                    text_field_lists.append(text_fields)
                    if text_fields:
                        pipe.hmget(hash_keys[i], text_fields)
                        fetch_indices.append(i)

                fetched = pipe.execute() if fetch_indices else []
                for fetch_pos, key_idx in enumerate(fetch_indices):
                    values = fetched[fetch_pos]
                    text_fields = text_field_lists[key_idx]
                    data = {f: v for f, v in zip(text_fields, values) if v is not None}
                    if data:
                        result[hash_keys[key_idx]] = data

            # Fetch all sets in one pipeline
            if set_keys:
                pipe = self.client.pipeline(transaction=False)
                for key in set_keys:
                    pipe.smembers(key)
                set_results = pipe.execute()
                for key, members in zip(set_keys, set_results):
                    result[key] = {"_type": "set", "members": list(members)}

        return result

    def restore_all(self, data: dict[str, dict[str, Any]]) -> tuple[int, int, list[str]]:
        """Bulk restore from backup dict. Merges — existing keys only overwritten if backup is newer.

        Returns:
            (restored_count, skipped_count, restored_keys) where restored_keys
            lists the keys that were actually written.
        """
        restored = 0
        skipped = 0
        restored_keys: list[str] = []

        # Separate sets from hashes, validating key prefixes
        set_items: list[tuple[str, list]] = []
        hash_items: list[tuple[str, dict[str, Any]]] = []
        for key, fields in data.items():
            # Validate key prefix to prevent restoring arbitrary keys
            if not any(key.startswith(p) for p in _VALID_KEY_PREFIXES):
                logger.warning("Skipping key with invalid prefix during restore: %s", key[:50])
                skipped += 1
                continue

            if fields.get("_type") == "set":
                members = fields.get("members", [])
                if members:
                    set_items.append((key, members))
            else:
                hash_items.append((key, fields))

        # Restore sets in a pipeline
        if set_items:
            pipe = self.client.pipeline(transaction=False)
            for key, members in set_items:
                pipe.sadd(key, *members)
            pipe.execute()
            restored += len(set_items)
            restored_keys.extend(k for k, _ in set_items)

        # Batch-fetch existing updated_at timestamps to decide merge
        if hash_items:
            hash_keys = [k for k, _ in hash_items]
            pipe = self.client.pipeline(transaction=False)
            for key in hash_keys:
                pipe.hmget(key, "updated_at")
            existing_timestamps = pipe.execute()

            # Write all qualifying hashes in a pipeline
            write_pipe = self.client.pipeline(transaction=False)
            write_count = 0
            written_keys: list[str] = []
            for (key, fields), ts_list in zip(hash_items, existing_timestamps):
                existing_updated_raw = ts_list[0] if ts_list else None
                if existing_updated_raw is not None:
                    existing_updated = float(existing_updated_raw)
                    backup_updated = float(fields.get("updated_at", "0"))
                    if existing_updated >= backup_updated:
                        skipped += 1
                        continue

                safe_fields = {k: v for k, v in fields.items() if v is not None}
                if safe_fields:
                    write_pipe.hset(key, mapping=safe_fields)
                    write_count += 1
                    written_keys.append(key)

            if write_count:
                write_pipe.execute()
            restored += write_count
            restored_keys.extend(written_keys)

        return restored, skipped, restored_keys
