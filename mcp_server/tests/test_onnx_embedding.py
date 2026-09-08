"""The ONNX embedding engine: pooling and normalisation maths against a fake
session and tokeniser, configuration resolution and load-time checks, and —
when the model is cached and OMNIMEM_REAL_EMBED_TESTS is set — a real
equivalence check against reference vectors."""

import json
import os
import sys
from pathlib import Path

import numpy as np
import pytest

from memory import onnx_embedding as oe

_ROOT = Path(__file__).resolve().parent.parent.parent


class FakeEncoding:
    def __init__(self, ids, mask, types=None):
        self.ids = ids
        self.attention_mask = mask
        self.type_ids = types if types is not None else [0] * len(ids)


class FakeTokenizer:
    """Tokenises by whitespace, pads each batch to its longest member."""

    def __init__(self):
        self.truncation = None
        self.padding = False

    def enable_truncation(self, max_length):
        self.truncation = max_length

    def enable_padding(self, **_kwargs):
        self.padding = True

    def token_to_id(self, token):
        return None

    def encode_batch(self, texts):
        tokens = [t.split()[: self.truncation] for t in texts]
        width = max(len(t) for t in tokens)
        out = []
        for toks in tokens:
            ids = [hash(w) % 1000 + 1 for w in toks] + [0] * (width - len(toks))
            mask = [1] * len(toks) + [0] * (width - len(toks))
            out.append(FakeEncoding(ids, mask))
        return out


class FakeIO:
    def __init__(self, name, shape):
        self.name = name
        self.shape = shape


class FakeSession:
    """Returns token embeddings equal to the token id broadcast across the
    dimension, so pooling results are hand-checkable; padded positions get
    a huge value that the mask must exclude."""

    def __init__(self, dim=4, with_type_ids=True, shape_last="dim", outputs=None):
        self.dim = dim
        self.inputs = [FakeIO("input_ids", ["b", "s"]), FakeIO("attention_mask", ["b", "s"])]
        if with_type_ids:
            self.inputs.append(FakeIO("token_type_ids", ["b", "s"]))
        self.outputs = outputs or [
            FakeIO("last_hidden_state", ["b", "s", dim if shape_last == "dim" else "d"]),
        ]
        self.calls = []

    def get_inputs(self):
        return self.inputs

    def get_outputs(self):
        return self.outputs

    def run(self, _outputs, feeds):
        self.calls.append(feeds)
        ids = feeds["input_ids"].astype(np.float32)
        mask = feeds["attention_mask"]
        emb = np.repeat(ids[..., None], self.dim, axis=-1)
        emb[mask == 0] = 1e6  # must be masked out by pooling
        # A pooled output first, when the graph declares one, exercises
        # output selection by name.
        if self.outputs[0].name == "sentence_embedding":
            return [emb[:, 0, :], emb]
        return [emb]


@pytest.fixture
def engine(monkeypatch):
    """An engine with a fake session and tokeniser already 'loaded'."""
    monkeypatch.delenv("EMBEDDING_MODEL", raising=False)
    monkeypatch.delenv("EMBEDDING_ONNX_FILE", raising=False)
    monkeypatch.delenv("EMBEDDING_MAX_SEQ_LENGTH", raising=False)
    monkeypatch.delenv("EMBEDDING_THREADS", raising=False)
    e = oe.OnnxSentenceEmbedding()
    e._session = FakeSession()
    e._tokenizer = FakeTokenizer()
    e._tokenizer.enable_truncation(e.max_seq_length)
    e._tokenizer.enable_padding()
    e._input_names = [i.name for i in e._session.get_inputs()]
    e.dimension = 4
    return e


class TestConfig:
    def test_resolve_repo(self):
        assert oe.resolve_repo("all-MiniLM-L6-v2") == "sentence-transformers/all-MiniLM-L6-v2"
        assert oe.resolve_repo(" BAAI/bge-small-en ") == "BAAI/bge-small-en"

    def test_defaults_and_env(self, monkeypatch):
        monkeypatch.delenv("EMBEDDING_MODEL", raising=False)
        e = oe.OnnxSentenceEmbedding()
        assert e.repo == "sentence-transformers/all-MiniLM-L6-v2"
        assert e.onnx_file == "onnx/model.onnx"
        assert e.max_seq_length == 256
        assert e.is_loaded is False
        monkeypatch.setenv("EMBEDDING_MODEL", "bge-small")
        monkeypatch.setenv("EMBEDDING_ONNX_FILE", "onnx/model_qint8_arm64.onnx")
        monkeypatch.setenv("EMBEDDING_MAX_SEQ_LENGTH", "128")
        monkeypatch.setenv("EMBEDDING_THREADS", "2")
        e = oe.OnnxSentenceEmbedding()
        assert (e.repo, e.onnx_file, e.max_seq_length, e._threads) == (
            "sentence-transformers/bge-small", "onnx/model_qint8_arm64.onnx", 128, 2,
        )
        assert oe.OnnxSentenceEmbedding(model="x", onnx_file="y", max_seq_length=64, threads=1)._threads == 1

    def test_local_directory_is_used_as_is(self, tmp_path):
        assert oe.resolve_repo(str(tmp_path)) == str(tmp_path)

    def test_bad_int_env_falls_back(self, monkeypatch, caplog):
        monkeypatch.setenv("EMBEDDING_THREADS", "many")
        assert oe._env_int("EMBEDDING_THREADS", None) is None
        assert "not an integer" in caplog.text


class TestEncode:
    def test_mean_pooling_ignores_padding_and_normalises(self, engine):
        vecs = engine.encode(["a b", "c"])
        assert vecs.shape == (2, 4) and vecs.dtype == np.float32
        # Every component equal ⇒ normalised vector is 1/sqrt(dim) each,
        # and the padded 1e6 never leaked in.
        np.testing.assert_allclose(vecs, np.full((2, 4), 0.5), atol=1e-6)

    def test_unnormalised_is_the_masked_mean(self, engine):
        raw = engine.encode(["a b", "c"], normalize_embeddings=False)
        enc = engine._tokenizer.encode_batch(["a b", "c"])
        expected0 = np.mean(enc[0].ids[:2])
        expected1 = enc[1].ids[0]
        np.testing.assert_allclose(raw[0], np.full(4, expected0), rtol=1e-6)
        np.testing.assert_allclose(raw[1], np.full(4, expected1), rtol=1e-6)

    def test_single_text_returns_1d(self, engine):
        assert engine.encode("just one").shape == (4,)

    def test_empty_batch(self, engine):
        assert engine.encode([]).shape == (0, 4)

    def test_batches_are_chunked(self, engine):
        vecs = engine.encode([f"t{i}" for i in range(7)], batch_size=3)
        assert vecs.shape == (7, 4)
        assert [f["input_ids"].shape[0] for f in engine._session.calls] == [3, 3, 1]

    def test_token_type_ids_only_when_the_graph_wants_them(self, engine):
        engine._session = FakeSession(with_type_ids=False)
        engine._input_names = ["input_ids", "attention_mask"]
        engine.encode("x")
        assert "token_type_ids" not in engine._session.calls[0]
        engine._session = FakeSession(with_type_ids=True)
        engine._input_names = ["input_ids", "attention_mask", "token_type_ids"]
        engine.encode("x")
        assert engine._session.calls[0]["token_type_ids"].dtype == np.int64

    def test_extra_kwargs_are_ignored_but_logged(self, engine, caplog):
        import logging
        caplog.set_level(logging.DEBUG, logger="memory.onnx_embedding")
        assert engine.encode("x", show_progress_bar=False, convert_to_numpy=True).shape == (4,)
        assert "ignoring unsupported arguments ['convert_to_numpy', 'show_progress_bar']" in caplog.text

    def test_encode_lazy_loads(self, monkeypatch):
        e = oe.OnnxSentenceEmbedding()
        calls = []

        def fake_load():
            calls.append(1)
            e._session = FakeSession()
            e._tokenizer = FakeTokenizer()
            e._tokenizer.enable_padding()
            e._input_names = ["input_ids", "attention_mask", "token_type_ids"]
            e.dimension = 4

        monkeypatch.setattr(e, "load", fake_load)
        e.encode("x")
        assert calls == [1]


class TestPool:
    def test_modes(self):
        tokens = np.array([[[1.0, 2.0], [3.0, 4.0], [9.0, 9.0]]], dtype=np.float32)
        mask = np.array([[1, 1, 0]])
        np.testing.assert_allclose(oe.pool(tokens, mask, "mean"), [[2.0, 3.0]])
        np.testing.assert_allclose(oe.pool(tokens, mask, "cls"), [[1.0, 2.0]])
        np.testing.assert_allclose(oe.pool(tokens, mask, "max"), [[3.0, 4.0]])


class TestLoad:
    def _fake_modules(self, monkeypatch, tmp_path, sbert_config=None, with_type_ids=True,
                      shape_last="dim", extra_files=None, outputs=None, cached=(),
                      fail_with=None, offline=False):
        import types

        files = {"onnx/model.onnx": "model", "tokenizer.json": "tok"}
        cfg = tmp_path / "sbert.json"
        if sbert_config is not None:
            cfg.write_text(sbert_config)
            files["sentence_bert_config.json"] = str(cfg)
        for name, content in (extra_files or {}).items():
            path = tmp_path / name.replace("/", "_")
            path.write_text(content)
            files[name] = str(path)
        downloads = []

        class EntryNotFoundError(OSError):
            pass

        class LocalEntryNotFoundError(EntryNotFoundError, FileNotFoundError):
            pass

        def hf_hub_download(repo, filename, revision=None, local_files_only=False):
            downloads.append((repo, filename, revision, local_files_only))
            if local_files_only and filename not in cached:
                raise LocalEntryNotFoundError("not cached")
            if offline and filename not in cached:
                raise LocalEntryNotFoundError("offline and not cached")
            if fail_with is not None and filename in fail_with:
                raise fail_with[filename]
            if filename not in files:
                raise EntryNotFoundError(filename)
            return files[filename]

        hub = types.ModuleType("huggingface_hub")
        hub.hf_hub_download = hf_hub_download
        errors = types.ModuleType("huggingface_hub.errors")
        errors.EntryNotFoundError = EntryNotFoundError
        errors.LocalEntryNotFoundError = LocalEntryNotFoundError
        constants = types.ModuleType("huggingface_hub.constants")
        constants.HF_HUB_OFFLINE = offline
        hub.errors = errors
        hub.constants = constants
        monkeypatch.setitem(sys.modules, "huggingface_hub", hub)
        monkeypatch.setitem(sys.modules, "huggingface_hub.errors", errors)
        monkeypatch.setitem(sys.modules, "huggingface_hub.constants", constants)
        self.LocalEntryNotFoundError = LocalEntryNotFoundError

        class SessionOptions:
            pass

        class GraphOptimizationLevel:
            ORT_ENABLE_EXTENDED = "extended"

        sessions = []

        def InferenceSession(path, sess_options=None, providers=None):
            sessions.append((path, sess_options, providers))
            return FakeSession(with_type_ids=with_type_ids, shape_last=shape_last, outputs=outputs)

        ort = types.ModuleType("onnxruntime")
        ort.SessionOptions = SessionOptions
        ort.GraphOptimizationLevel = GraphOptimizationLevel
        ort.InferenceSession = InferenceSession
        monkeypatch.setitem(sys.modules, "onnxruntime", ort)

        class Tokenizer:
            @staticmethod
            def from_file(path):
                assert path == "tok"
                return FakeTokenizer()

        tok = types.ModuleType("tokenizers")
        tok.Tokenizer = Tokenizer
        monkeypatch.setitem(sys.modules, "tokenizers", tok)
        return downloads, sessions

    def test_load_reads_config_and_sets_up_session(self, monkeypatch, tmp_path):
        downloads, sessions = self._fake_modules(
            monkeypatch, tmp_path, sbert_config=json.dumps({"max_seq_length": 128}),
        )
        e = oe.OnnxSentenceEmbedding(model="all-MiniLM-L6-v2", threads=3)
        e.load()
        e.load()  # idempotent
        assert e.is_loaded and e.dimension == 4 and e.max_seq_length == 128
        assert e._tokenizer.truncation == 128 and e._tokenizer.padding
        assert sessions[0][0] == "model" and sessions[0][2] == ["CPUExecutionProvider"]
        assert sessions[0][1].intra_op_num_threads == 3
        assert sessions[0][1].graph_optimization_level == "extended"
        # Each file is tried from the cache first, then fetched, with the
        # default model pinned to its known revision.
        fetched = [(f, rev, local) for _, f, rev, local in downloads]
        assert fetched[:2] == [("onnx/model.onnx", oe.DEFAULT_MODEL_REVISION, True),
                               ("onnx/model.onnx", oe.DEFAULT_MODEL_REVISION, False)]
        assert {f for f, _, _ in fetched} == {
            "onnx/model.onnx", "tokenizer.json", "1_Pooling/config.json",
            "sentence_bert_config.json", "config.json",
        }
        assert e.pooling == "mean"
        assert e.encode("a b").shape == (4,)

    def test_load_without_threads_or_config(self, monkeypatch, tmp_path, caplog):
        _, sessions = self._fake_modules(monkeypatch, tmp_path, shape_last="symbolic")
        e = oe.OnnxSentenceEmbedding()
        e.load()
        assert e.max_seq_length == 256
        assert e.dimension is None  # symbolic output shape
        assert not hasattr(sessions[0][1], "intra_op_num_threads")
        assert "assuming mean pooling" in caplog.text

    def test_cached_files_skip_the_network(self, monkeypatch, tmp_path):
        downloads, _ = self._fake_modules(
            monkeypatch, tmp_path, cached=("onnx/model.onnx", "tokenizer.json"),
        )
        oe.OnnxSentenceEmbedding().load()
        model_calls = [(local) for _, f, _, local in downloads if f == "onnx/model.onnx"]
        assert model_calls == [True]  # one cache hit, no fetch

    def test_non_default_model_follows_main_unless_pinned(self, monkeypatch, tmp_path):
        downloads, _ = self._fake_modules(monkeypatch, tmp_path)
        oe.OnnxSentenceEmbedding(model="BAAI/bge-small-en-v1.5").load()
        assert {rev for _, _, rev, _ in downloads} == {None}
        monkeypatch.setenv("EMBEDDING_MODEL_REVISION", "abc123")
        downloads.clear()
        oe.OnnxSentenceEmbedding(model="BAAI/bge-small-en-v1.5").load()
        assert {rev for _, _, rev, _ in downloads} == {"abc123"}

    def test_missing_onnx_export_is_explained(self, monkeypatch, tmp_path):
        self._fake_modules(monkeypatch, tmp_path)
        e = oe.OnnxSentenceEmbedding(model="intfloat/e5-small-v2", onnx_file="onnx/nope.onnx")
        with pytest.raises(oe.EmbeddingModelError, match="no ONNX export.*EMBEDDING_BACKEND=torch"):
            e.load()

    def test_unreachable_hub_propagates_when_online(self, monkeypatch, tmp_path):
        """huggingface_hub reports an unreachable hub as LocalEntryNotFoundError
        — the same class as "not in the cache". Online, that is a network
        failure and must not silently default a config."""
        failures: dict = {}
        self._fake_modules(monkeypatch, tmp_path, fail_with=failures)
        failures["sentence_bert_config.json"] = self.LocalEntryNotFoundError("connection refused")
        with pytest.raises(ConnectionError, match="Could not reach the Hugging Face hub"):
            oe.OnnxSentenceEmbedding().load()

    def test_offline_missing_config_defaults(self, monkeypatch, tmp_path, caplog):
        """Offline, an uncached optional config is simply absent."""
        self._fake_modules(monkeypatch, tmp_path, offline=True,
                           cached=("onnx/model.onnx", "tokenizer.json"))
        e = oe.OnnxSentenceEmbedding()
        e.load()
        assert e.max_seq_length == 256
        assert "assuming mean pooling" in caplog.text

    def test_offline_missing_required_file_is_explained(self, monkeypatch, tmp_path):
        self._fake_modules(monkeypatch, tmp_path, offline=True, cached=("tokenizer.json",))
        with pytest.raises(oe.EmbeddingModelError, match="offline and the file is not in the Hugging Face cache"):
            oe.OnnxSentenceEmbedding().load()

    def test_repository_not_found_propagates_as_itself(self, monkeypatch, tmp_path):
        class RepositoryNotFoundError(OSError):
            pass

        self._fake_modules(monkeypatch, tmp_path,
                           fail_with={"onnx/model.onnx": RepositoryNotFoundError("no such repo")})
        with pytest.raises(RepositoryNotFoundError):
            oe.OnnxSentenceEmbedding(model="typo/no-such-model").load()

    def test_failed_load_leaves_engine_unloaded(self, monkeypatch, tmp_path):
        self._fake_modules(monkeypatch, tmp_path)
        sys.modules["tokenizers"].Tokenizer = type(
            "Tokenizer", (), {"from_file": staticmethod(lambda p: (_ for _ in ()).throw(OSError("bad tok")))},
        )
        e = oe.OnnxSentenceEmbedding()
        with pytest.raises(OSError, match="bad tok"):
            e.load()
        assert e.is_loaded is False

    def test_unreadable_config_is_ignored(self, monkeypatch, tmp_path, caplog):
        self._fake_modules(monkeypatch, tmp_path, extra_files={"config.json": "{not json"})
        e = oe.OnnxSentenceEmbedding()
        e.load()
        assert "Unreadable config.json" in caplog.text

    @pytest.mark.parametrize("config,expected", [
        ('{"pooling_mode_cls_token": true, "pooling_mode_mean_tokens": false}', "cls"),
        ('{"pooling_mode_max_tokens": true}', "max"),
        ('{"pooling_mode_mean_tokens": true, "pooling_mode_cls_token": false}', "mean"),
    ])
    def test_pooling_mode_from_config(self, monkeypatch, tmp_path, config, expected):
        self._fake_modules(monkeypatch, tmp_path, extra_files={"1_Pooling/config.json": config})
        e = oe.OnnxSentenceEmbedding()
        e.load()
        assert e.pooling == expected

    @pytest.mark.parametrize("config", [
        '{"pooling_mode_mean_sqrt_len_tokens": true}',
        '{"pooling_mode_mean_tokens": true, "pooling_mode_cls_token": true}',
        '{"pooling_mode_weightedmean_tokens": true, "pooling_mode_mean_tokens": true}',
        '{}',
    ])
    def test_unsupported_pooling_is_refused(self, monkeypatch, tmp_path, config):
        self._fake_modules(monkeypatch, tmp_path, extra_files={"1_Pooling/config.json": config})
        with pytest.raises(oe.EmbeddingModelError, match="does not implement"):
            oe.OnnxSentenceEmbedding().load()

    def test_seq_length_bounds(self, monkeypatch, tmp_path, caplog):
        self._fake_modules(monkeypatch, tmp_path,
                           extra_files={"config.json": '{"max_position_embeddings": 512}'})
        e = oe.OnnxSentenceEmbedding(max_seq_length=1000)
        e.load()
        assert e.max_seq_length == 512
        assert "clamping" in caplog.text
        monkeypatch.setenv("EMBEDDING_MAX_SEQ_LENGTH", "1")
        e = oe.OnnxSentenceEmbedding()
        e.load()
        assert e.max_seq_length == 256
        assert "is below 3" in caplog.text

    def test_bad_position_count_ignored(self, monkeypatch, tmp_path):
        self._fake_modules(monkeypatch, tmp_path,
                           extra_files={"config.json": '{"max_position_embeddings": "lots"}'})
        e = oe.OnnxSentenceEmbedding(max_seq_length=300)
        e.load()
        assert e.max_seq_length == 300

    def test_token_output_selected_by_name(self, monkeypatch, tmp_path):
        outputs = [FakeIO("sentence_embedding", ["b", 4]), FakeIO("last_hidden_state", ["b", "s", 4])]
        self._fake_modules(monkeypatch, tmp_path, outputs=outputs)
        e = oe.OnnxSentenceEmbedding()
        e.load()
        assert e._output_index == 1
        np.testing.assert_allclose(e.encode(["a b", "c"]), np.full((2, 4), 0.5), atol=1e-6)

    def test_pooled_only_graph_is_refused(self, monkeypatch, tmp_path):
        outputs = [FakeIO("sentence_embedding", ["b", 4])]
        self._fake_modules(monkeypatch, tmp_path, outputs=outputs)
        with pytest.raises(oe.EmbeddingModelError, match="needs a \\[batch, tokens, dim\\]"):
            oe.OnnxSentenceEmbedding().load()

    def test_explicit_max_seq_length_skips_config(self, monkeypatch, tmp_path):
        downloads, _ = self._fake_modules(monkeypatch, tmp_path)
        e = oe.OnnxSentenceEmbedding(max_seq_length=64)
        e.load()
        assert e.max_seq_length == 64
        assert "sentence_bert_config.json" not in [f for _, f, _, _ in downloads]

    @pytest.mark.parametrize("config", ['{"max_seq_length": 0}', '{"max_seq_length": "many"}'])
    def test_unusable_configured_cap_falls_back(self, monkeypatch, tmp_path, config, caplog):
        self._fake_modules(monkeypatch, tmp_path, sbert_config=config)
        e = oe.OnnxSentenceEmbedding()
        e.load()
        assert e.max_seq_length == 256
        assert "No usable max_seq_length" in caplog.text

    def test_load_from_local_directory_uses_no_hub(self, monkeypatch, tmp_path):
        """A directory holding the repo's files is read directly — the
        air-gapped path — and a missing file is a clear FileNotFoundError."""
        import types
        (tmp_path / "onnx").mkdir()
        (tmp_path / "onnx" / "model.onnx").write_text("model")
        (tmp_path / "tokenizer.json").write_text("tok")
        (tmp_path / "sentence_bert_config.json").write_text('{"max_seq_length": 32}')

        hub = types.ModuleType("huggingface_hub")
        hub.hf_hub_download = lambda *a, **k: (_ for _ in ()).throw(AssertionError("hub used"))
        monkeypatch.setitem(sys.modules, "huggingface_hub", hub)
        (tmp_path / "1_Pooling").mkdir()
        (tmp_path / "1_Pooling" / "config.json").write_text('{"pooling_mode_mean_tokens": true}')
        ort = types.ModuleType("onnxruntime")
        ort.SessionOptions = type("SessionOptions", (), {})
        ort.GraphOptimizationLevel = type("G", (), {"ORT_ENABLE_EXTENDED": "x"})
        opened = []
        ort.InferenceSession = lambda path, sess_options=None, providers=None: opened.append(path) or FakeSession()
        monkeypatch.setitem(sys.modules, "onnxruntime", ort)
        tok = types.ModuleType("tokenizers")
        tok.Tokenizer = type("Tokenizer", (), {"from_file": staticmethod(lambda p: FakeTokenizer())})
        monkeypatch.setitem(sys.modules, "tokenizers", tok)

        e = oe.OnnxSentenceEmbedding(model=str(tmp_path))
        e.load()
        assert opened == [str(tmp_path / "onnx" / "model.onnx")]
        assert e.max_seq_length == 32

        missing = oe.OnnxSentenceEmbedding(model=str(tmp_path), onnx_file="onnx/nope.onnx")
        with pytest.raises(oe.EmbeddingModelError, match="onnx/nope.onnx"):
            missing.load()

    def test_pad_token_id_from_tokeniser(self, monkeypatch, tmp_path):
        """[PAD]'s own id is used when the tokeniser knows it."""
        calls = {}

        class PadAwareTokenizer(FakeTokenizer):
            def token_to_id(self, token):
                return 7 if token == "[PAD]" else None

            def enable_padding(self, **kwargs):
                calls.update(kwargs)
                self.padding = True

        self._fake_modules(monkeypatch, tmp_path)
        sys.modules["tokenizers"].Tokenizer = type(
            "Tokenizer", (), {"from_file": staticmethod(lambda p: PadAwareTokenizer())},
        )
        e = oe.OnnxSentenceEmbedding()
        e.load()
        assert calls == {"pad_id": 7, "pad_token": "[PAD]"}

    @pytest.mark.parametrize("config", ['{"max_seq_length": 0}', "not json"])
    def test_bad_config_values_fall_back(self, monkeypatch, tmp_path, config):
        self._fake_modules(monkeypatch, tmp_path, sbert_config=config)
        e = oe.OnnxSentenceEmbedding()
        e.load()
        assert e.max_seq_length == 256


@pytest.mark.skipif(
    not os.getenv("OMNIMEM_REAL_EMBED_TESTS"),
    reason="set OMNIMEM_REAL_EMBED_TESTS=1 to run the real model (needs the HF cache or network)",
)
def test_real_model_matches_reference_vectors():
    """The ONNX path must produce the vectors sentence-transformers does.

    The reference components below are the first eight of each vector as
    produced by sentence-transformers 6.0 / torch 2.13 on all-MiniLM-L6-v2
    (normalised). A change in tokenisation, pooling or normalisation would
    move them well beyond the tolerance; a different graph file would too.
    """
    reference = {
        "Decision: use uv for packaging.":
            [-0.060852, 0.090593, 0.018895, -0.022611, 0.123968, -0.049085, 0.071894, 0.124453],
        "The FT.SEARCH tag filter matches raw values only.":
            [0.012451, -0.006367, -0.064195, 0.052464, 0.124249, -0.029685, -0.046728, -0.007557],
    }
    e = oe.OnnxSentenceEmbedding("all-MiniLM-L6-v2")
    e.load()
    assert e.dimension == 384
    vecs = e.encode(list(reference))
    assert vecs.shape == (2, 384)
    np.testing.assert_allclose(np.linalg.norm(vecs, axis=1), 1.0, atol=1e-5)
    for vec, expected in zip(vecs, reference.values()):
        np.testing.assert_allclose(vec[:8], expected, atol=1e-4)
    # Single-text and batched paths agree with each other too.
    np.testing.assert_allclose(e.encode(list(reference)[0]), vecs[0], atol=1e-6)
