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
_VALID_KEY_PREFIXES = ("mem:episodic:", "mem:project:", "mem:knowledge:", "topics:", "log:recall:", "meta:")

# Valid namespace names for search index lookups
_VALID_NAMESPACES = {"episodic", "project", "knowledge"}

VECTOR_DIM = 384
VECTOR_ALGORITHM = "HNSW"
DISTANCE_METRIC = "COSINE"

# Fields returned per namespace to avoid fetching unnecessary data
_NAMESPACE_RETURN_FIELDS: dict[str, tuple[str, ...]] = {
    "episodic": (
        "similarity_score", "content", "project", "state", "surface_score",
        "created_at", "updated_at", "tags", "deprioritised_reason",
        "reinstate_hints", "effort_score", "outcome", "iterations",
        "abandoned_approaches", "breakthrough", "gotchas", "experience_weight",
        "contradictions", "recall_count", "last_recalled",
    ),
    "project": (
        "similarity_score", "content", "project_name", "stack", "state",
        "surface_score", "created_at", "updated_at", "recall_count", "last_recalled",
    ),
    "knowledge": (
        "similarity_score", "content", "source_url", "feed_name", "published_at",
        "topics", "state", "surface_score", "created_at", "updated_at",
        "recall_count", "last_recalled", "expires_at",
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
        ],
    },
}


class ValkeyStore:
    """Manages Valkey connection, vector indexes, and CRUD operations."""

    def __init__(self) -> None:
        self._client: valkey.Valkey | None = None

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

    def _migrate_indexes(self) -> None:
        """Drop indexes whose field count doesn't match the definition.

        This is data-safe — dropindex() only removes the index, not the
        underlying hashes. _ensure_indexes() then recreates it with the
        new fields, and RediSearch re-indexes existing hashes automatically.
        """
        for idx_name, idx_def in INDEX_DEFINITIONS.items():
            try:
                info = self.client.ft(idx_name).info()
                existing_count = len(info.get("attributes", []))
                expected_count = len(idx_def["fields"])
                if existing_count < expected_count:
                    logger.info(
                        "Index %s has %d fields, expected %d — dropping for recreation",
                        idx_name, existing_count, expected_count,
                    )
                    self.client.ft(idx_name).dropindex()
            except valkey.ResponseError:
                pass  # Index doesn't exist yet — _ensure_indexes will create it

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

        q = (
            Query(query_str)
            .return_fields(*return_fields)
            .paging(0, top_k)
            .dialect(2)
        )

        try:
            results = self.client.ft(idx_name).search(q, query_params={"vec": blob})
        except valkey.ResponseError as exc:
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

    def delete(self, key: str) -> None:
        """Hard delete a key."""
        self._validate_key(key)
        self.client.delete(key)
        logger.info("Deleted key %s", key)

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
