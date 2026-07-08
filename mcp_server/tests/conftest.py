"""Shared test fixtures: in-memory fakes for ValkeyStore and Embedder."""

import json
import sys
import time
from pathlib import Path
from unittest.mock import MagicMock

import numpy as np
import pytest

# Ensure mcp_server is importable
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from memory.lifecycle import MemoryLifecycle, MemoryState
from memory.recall import RecallPipeline


class FakeValkeyClient:
    """Minimal fake that backs a ValkeyStore without needing Valkey running."""

    def __init__(self):
        self._data: dict[str, dict[str, str]] = {}
        self._sets: dict[str, set[str]] = {}

    # --- hash commands ---
    def hset(self, key, field=None, value=None, mapping=None):
        if key not in self._data:
            self._data[key] = {}
        if mapping:
            for k, v in mapping.items():
                self._data[key][str(k)] = str(v) if not isinstance(v, bytes) else v
        if field is not None:
            self._data[key][str(field)] = str(value) if not isinstance(value, bytes) else value

    def hkeys(self, key):
        return list(self._data.get(key, {}).keys())

    def hmget(self, key, fields):
        data = self._data.get(key, {})
        return [data.get(f) for f in fields]

    def hgetall(self, key):
        return dict(self._data.get(key, {}))

    def delete(self, key):
        self._data.pop(key, None)

    def ping(self):
        return True

    def scan(self, cursor=0, match="*", count=500):
        import fnmatch
        keys = [k for k in self._data if fnmatch.fnmatch(k, match)]
        return (0, keys)

    # --- set commands ---
    def sadd(self, key, *members):
        if key not in self._sets:
            self._sets[key] = set()
        self._sets[key].update(members)

    def srem(self, key, *members):
        if key in self._sets:
            self._sets[key] -= set(members)

    def smembers(self, key):
        return self._sets.get(key, set())

    # --- list commands ---
    def lpush(self, key, *values):
        if key not in self._data:
            self._data[key] = {"_list": []}
        lst = self._data[key].setdefault("_list", [])
        for v in values:
            lst.insert(0, v)
        return len(lst)

    def brpop(self, key, timeout=0):
        data = self._data.get(key, {})
        lst = data.get("_list", [])
        if lst:
            return (key, lst.pop())
        return None

    def llen(self, key):
        data = self._data.get(key, {})
        return len(data.get("_list", []))

    # --- hash increment ---
    def hincrby(self, key, field, amount=1):
        if key not in self._data:
            self._data[key] = {}
        current = int(self._data[key].get(field, "0"))
        new_val = current + amount
        self._data[key][field] = str(new_val)
        return new_val

    # --- pipeline ---
    def pipeline(self, transaction=False):
        return FakePipeline(self)

    # --- search (ft) ---
    def ft(self, index_name):
        return FakeSearchIndex(self, index_name)


class FakePipeline:
    def __init__(self, client: FakeValkeyClient):
        self._client = client
        self._commands: list = []

    def hset(self, key, field=None, value=None, mapping=None):
        self._commands.append(("hset", key, field, value, mapping))
        return self

    def hkeys(self, key):
        self._commands.append(("hkeys", key))
        return self

    def hmget(self, key, fields):
        self._commands.append(("hmget", key, fields))
        return self

    def hincrby(self, key, field, amount=1):
        self._commands.append(("hincrby", key, field, amount))
        return self

    def expire(self, key, ttl):
        self._commands.append(("expire", key, ttl))
        return self

    def execute(self):
        results = []
        for cmd in self._commands:
            if cmd[0] == "hset":
                _, key, field, value, mapping = cmd
                self._client.hset(key, field=field, value=value, mapping=mapping)
                results.append(True)
            elif cmd[0] == "hkeys":
                results.append(self._client.hkeys(cmd[1]))
            elif cmd[0] == "hmget":
                results.append(self._client.hmget(cmd[1], cmd[2]))
            elif cmd[0] == "hincrby":
                _, key, field, amount = cmd
                results.append(self._client.hincrby(key, field, amount))
            elif cmd[0] == "expire":
                results.append(True)
        self._commands = []
        return results


class FakeSearchDoc:
    def __init__(self, id, fields):
        self.id = id
        self.payload = None
        for k, v in fields.items():
            setattr(self, k, v)

    def __getattr__(self, name):
        return None


class FakeSearchResult:
    def __init__(self, docs):
        self.docs = docs
        self.total = len(docs)


class FakeSearchIndex:
    def __init__(self, client: FakeValkeyClient, index_name: str):
        self._client = client
        self._index_name = index_name

    def info(self):
        return {}

    def create_index(self, *args, **kwargs):
        pass

    def search(self, query, query_params=None):
        """Return stored hashes that match the index prefix, sorted by vector similarity."""
        prefix_map = {
            "idx:episodic": "mem:episodic:",
            "idx:project": "mem:project:",
            "idx:knowledge": "mem:knowledge:",
            "idx:preference": "mem:preference:",
        }
        prefix = prefix_map.get(self._index_name, "")
        query_vec = None
        if query_params and "vec" in query_params:
            query_vec = np.frombuffer(query_params["vec"], dtype=np.float32)

        docs = []
        for key, data in self._client._data.items():
            if not key.startswith(prefix):
                continue
            fields = {k: v for k, v in data.items() if k != "vector"}
            # Compute similarity if vectors present
            if query_vec is not None and "vector" in data:
                stored_vec = np.frombuffer(data["vector"], dtype=np.float32)
                # cosine distance (lower = more similar)
                dot = float(np.dot(query_vec, stored_vec))
                dist = 1.0 - dot
                fields["similarity_score"] = str(dist)
            else:
                fields["similarity_score"] = "0.0"
            docs.append(FakeSearchDoc(key, fields))

        # Sort by similarity_score ascending (closest first). No cap here —
        # FakeValkeyStore.search slices to the caller's top_k, mirroring the
        # real KNN behaviour where top_k governs candidate count.
        docs.sort(key=lambda d: float(d.similarity_score))
        return FakeSearchResult(docs)


class FakeEmbedder:
    """Deterministic embedder using additive word vectors so similar text has similar embeddings."""

    def __init__(self):
        self._model = True

    @property
    def is_loaded(self):
        return True

    def embed(self, text: str) -> np.ndarray:
        vec = np.zeros(384, dtype=np.float32)
        for word in text.lower().split():
            rng = np.random.RandomState(hash(word) % (2**31))
            vec += rng.randn(384).astype(np.float32)
        norm = np.linalg.norm(vec)
        if norm > 0:
            vec /= norm
        return vec

    def embed_batch(self, texts: list[str]) -> list[np.ndarray]:
        return [self.embed(t) for t in texts]


class FakeValkeyStore:
    """ValkeyStore backed by FakeValkeyClient instead of real Valkey."""

    def __init__(self):
        self._client = FakeValkeyClient()

    @property
    def client(self):
        return self._client

    def connect(self, **kwargs):
        pass

    def upsert(self, namespace, key, fields, vector):
        data = {k: v for k, v in fields.items() if v is not None}
        data["vector"] = vector.astype(np.float32).tobytes()
        self._client.hset(key, mapping=data)

    def search(self, namespace, vector, top_k=10, filter_expr=None):
        idx_name = f"idx:{namespace}"
        blob = vector.astype(np.float32).tobytes()

        from memory.store import _NAMESPACE_RETURN_FIELDS
        result = self._client.ft(idx_name).search(None, query_params={"vec": blob})

        output = []
        for doc in result.docs[:top_k]:
            entry = {"key": doc.id}
            for attr, val in doc.__dict__.items():
                if attr.startswith("_") or attr in ("id", "payload"):
                    continue
                if val is not None:
                    entry[attr] = val
            output.append(entry)
        return output

    def get(self, key):
        fields = self._client.hkeys(key)
        if not fields:
            return None
        text_fields = [f for f in fields if f != "vector"]
        if not text_fields:
            return None
        values = self._client.hmget(key, text_fields)
        return {f: v for f, v in zip(text_fields, values) if v is not None}

    def set_field(self, key, field, value):
        self._client.hset(key, field, value)

    def set_fields(self, key, mapping):
        if mapping:
            self._client.hset(key, mapping=mapping)

    def get_multi(self, keys):
        results = []
        for key in keys:
            results.append(self.get(key))
        return results

    def get_fields_multi(self, keys, fields):
        fields = list(fields)
        results = []
        for key in keys:
            row = self._client.hmget(key, fields)
            data = {f: v for f, v in zip(fields, row) if v is not None}
            results.append(data if data else None)
        return results

    def get_vectors_multi(self, keys):
        out = []
        for key in keys:
            raw = self._client._data.get(key, {}).get("vector")
            if isinstance(raw, bytes) and len(raw) == 384 * 4:
                out.append(np.frombuffer(raw, dtype=np.float32))
            else:
                out.append(None)
        return out

    def delete(self, key):
        self._client.delete(key)

    def scan_prefix(self, prefix):
        return [k for k in self._client._data if k.startswith(prefix)]

    def dump_all(self):
        result = {}
        for key, data in self._client._data.items():
            result[key] = {k: v for k, v in data.items() if k != "vector"}
        for key, members in self._client._sets.items():
            result[key] = {"_type": "set", "members": list(members)}
        return result

    def restore_all(self, data):
        """Merge backup data into the store. Newer entries overwrite older ones.

        Returns:
            (restored_count, skipped_count, restored_keys)
        """
        restored = 0
        skipped = 0
        restored_keys = []
        for key, fields in data.items():
            if fields.get("_type") == "set":
                members = fields.get("members", [])
                if members:
                    self._client.sadd(key, *members)
                    restored += 1
                    restored_keys.append(key)
                continue
            existing = self.get(key)
            if existing:
                existing_ts = float(existing.get("updated_at", "0"))
                backup_ts = float(fields.get("updated_at", "0"))
                if existing_ts >= backup_ts:
                    skipped += 1
                    continue
            safe = {k: v for k, v in fields.items() if v is not None}
            if safe:
                self._client.hset(key, mapping=safe)
                restored += 1
                restored_keys.append(key)
        return restored, skipped, restored_keys


@pytest.fixture
def fake_store():
    return FakeValkeyStore()


@pytest.fixture
def fake_embedder():
    return FakeEmbedder()


@pytest.fixture
def lifecycle(fake_store):
    return MemoryLifecycle(fake_store)


@pytest.fixture
def pipeline(fake_store, fake_embedder, lifecycle):
    return RecallPipeline(fake_store, fake_embedder, lifecycle)


def store_memory(store, embedder, key, content, namespace="episodic",
                 state="active", project=None, tags=None,
                 effort_score=None, outcome=None, experience_weight="1.0",
                 abandoned_approaches=None, surface_score="1.0",
                 reinstate_hints=None, deprioritised_reason=None,
                 breakthrough=None, gotchas=None, contradictions=None):
    """Helper: store a memory directly into the fake store."""
    now = str(time.time())
    vector = embedder.embed(content)
    fields = {
        "content": content,
        "state": state,
        "surface_score": surface_score,
        "experience_weight": experience_weight,
        "created_at": now,
        "updated_at": now,
        "tags": json.dumps(tags or []),
    }
    if project:
        fields["project"] = project
    if effort_score is not None:
        fields["effort_score"] = str(effort_score)
    if outcome:
        fields["outcome"] = outcome
    if abandoned_approaches:
        fields["abandoned_approaches"] = json.dumps(abandoned_approaches)
    if reinstate_hints:
        fields["reinstate_hints"] = json.dumps(reinstate_hints)
    if deprioritised_reason:
        fields["deprioritised_reason"] = deprioritised_reason
    if breakthrough:
        fields["breakthrough"] = breakthrough
    if gotchas:
        fields["gotchas"] = gotchas
    if contradictions:
        fields["contradictions"] = json.dumps(contradictions)
    store.upsert(namespace, key, fields, vector)
    return key
