"""Valkey connection and vector operations."""

import json
import logging
import os
import time
from typing import Any

import numpy as np
import valkey
from valkey.commands.search.field import NumericField, TagField, TextField, VectorField
from valkey.commands.search.indexDefinition import IndexDefinition, IndexType
from valkey.commands.search.query import Query

logger = logging.getLogger(__name__)

VECTOR_DIM = 384
VECTOR_ALGORITHM = "HNSW"
DISTANCE_METRIC = "COSINE"

INDEX_DEFINITIONS: dict[str, dict[str, Any]] = {
    "idx:episodic": {
        "prefix": "mem:episodic:",
        "fields": [
            VectorField(
                "vector",
                VECTOR_ALGORITHM,
                {"TYPE": "FLOAT32", "DIM": VECTOR_DIM, "DISTANCE_METRIC": DISTANCE_METRIC},
            ),
            TextField("content"),
            TagField("project"),
            TagField("state"),
            NumericField("surface_score"),
            NumericField("created_at"),
            NumericField("updated_at"),
            TagField("tags"),
            TextField("deprioritised_reason"),
            TextField("reinstate_hints"),
            NumericField("effort_score"),
            TagField("outcome"),
            NumericField("iterations"),
            TextField("abandoned_approaches"),
            TextField("breakthrough"),
            TextField("gotchas"),
            NumericField("experience_weight"),
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
            TextField("content"),
            TagField("project_name"),
            TextField("stack"),
            TagField("state"),
            NumericField("surface_score"),
            NumericField("created_at"),
            NumericField("updated_at"),
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
            TextField("content"),
            TextField("source_url"),
            TagField("feed_name"),
            NumericField("published_at"),
            TagField("topics"),
            TagField("state"),
            NumericField("surface_score"),
            NumericField("created_at"),
            NumericField("updated_at"),
        ],
    },
}


class ValkeyStore:
    """Manages Valkey connection, vector indexes, and CRUD operations."""

    def __init__(self) -> None:
        self._client: valkey.Valkey | None = None

    def connect(self, max_retries: int = 10, retry_delay: float = 2.0) -> None:
        """Connect to Valkey with retry logic on startup."""
        host = os.getenv("VALKEY_HOST", "valkey")
        port = int(os.getenv("VALKEY_PORT", "6379"))
        password = os.getenv("VALKEY_PASSWORD", "")

        for attempt in range(1, max_retries + 1):
            try:
                self._client = valkey.Valkey(
                    host=host,
                    port=port,
                    password=password,
                    decode_responses=True,
                )
                self._client.ping()
                logger.info("Connected to Valkey at %s:%d", host, port)
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

    def _ensure_indexes(self) -> None:
        """Create vector indexes if they don't already exist."""
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

    def upsert(
        self, namespace: str, key: str, fields: dict[str, Any], vector: np.ndarray
    ) -> None:
        """Store a hash with an embedded vector."""
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
        idx_name = f"idx:{namespace}"
        blob = vector.astype(np.float32).tobytes()

        filter_clause = filter_expr if filter_expr else "*"
        query_str = f"({filter_clause})=>[KNN {top_k} @vector $vec AS similarity_score]"

        q = (
            Query(query_str)
            .sort_by("similarity_score")
            .return_fields(
                "similarity_score", "content", "project", "state", "surface_score",
                "created_at", "updated_at", "tags", "deprioritised_reason",
                "reinstate_hints", "effort_score", "outcome", "iterations",
                "abandoned_approaches", "breakthrough", "gotchas",
                "experience_weight", "source_url", "feed_name", "published_at",
                "topics", "project_name", "stack",
            )
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
            entry: dict[str, Any] = {"key": doc.id}
            for attr in dir(doc):
                if attr.startswith("_") or attr in ("id", "payload"):
                    continue
                val = getattr(doc, attr, None)
                if val is not None and not callable(val):
                    entry[attr] = val
            output.append(entry)
        return output

    def get(self, key: str) -> dict[str, Any] | None:
        """Retrieve all fields for a key."""
        data = self.client.hgetall(key)
        if not data:
            return None
        data.pop("vector", None)
        return data

    def set_field(self, key: str, field: str, value: Any) -> None:
        """Update a single field without re-embedding."""
        self.client.hset(key, field, value)

    def delete(self, key: str) -> None:
        """Hard delete a key."""
        self.client.delete(key)
        logger.info("Deleted key %s", key)

    def scan_prefix(self, prefix: str) -> list[str]:
        """Return all keys matching a prefix using SCAN."""
        keys: list[str] = []
        cursor = 0
        while True:
            cursor, batch = self.client.scan(cursor=cursor, match=f"{prefix}*", count=100)
            keys.extend(batch)
            if cursor == 0:
                break
        return keys

    def dump_all(self) -> dict[str, dict[str, Any]]:
        """Export all mem:* and topics:* keys with all fields using HSCAN. Safe for large datasets."""
        result: dict[str, dict[str, Any]] = {}
        prefixes = ["mem:", "topics:", "log:recall:"]
        for prefix in prefixes:
            for key in self.scan_prefix(prefix):
                key_type = self.client.type(key)
                if key_type == "hash":
                    fields: dict[str, Any] = {}
                    cursor = 0
                    while True:
                        cursor, data = self.client.hscan(key, cursor=cursor, count=100)
                        for field_name, field_val in data.items():
                            if field_name == "vector":
                                continue
                            fields[field_name] = field_val
                        if cursor == 0:
                            break
                    result[key] = fields
                elif key_type == "set":
                    members = self.client.smembers(key)
                    result[key] = {"_type": "set", "members": list(members)}
        return result

    def restore_all(self, data: dict[str, dict[str, Any]]) -> tuple[int, int]:
        """Bulk restore from backup dict. Merges — existing keys only overwritten if backup is newer."""
        restored = 0
        skipped = 0
        for key, fields in data.items():
            if fields.get("_type") == "set":
                members = fields.get("members", [])
                if members:
                    self.client.sadd(key, *members)
                restored += 1
                continue

            existing = self.client.hgetall(key)
            if existing:
                existing_updated = float(existing.get("updated_at", "0"))
                backup_updated = float(fields.get("updated_at", "0"))
                if existing_updated >= backup_updated:
                    skipped += 1
                    continue

            safe_fields = {k: v for k, v in fields.items() if v is not None}
            if safe_fields:
                self.client.hset(key, mapping=safe_fields)
                restored += 1
        return restored, skipped
