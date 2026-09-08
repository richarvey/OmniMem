"""Sentence embeddings with ONNX Runtime — no PyTorch, no sentence-transformers.

This is the v6.7 embedding backend. It runs the maintainer-exported ONNX
graph of a sentence-transformers model (``onnx/model.onnx`` in the
model's Hugging Face repo) with ``onnxruntime``, tokenises with the Rust
``tokenizers`` library, and does the model's mean pooling and L2
normalisation in numpy. The output is the same vector sentence-transformers
produces for the same text — cosine 1.0000 on all-MiniLM-L6-v2 — at
roughly a third of the single-text latency and a tenth of the load time,
without the ~2 GB of PyTorch that used to be most of every image.

The MCP server, the web UI and the RSS worker all run this same module:
the worker image ships the ``memory`` package too (since 6.7), so the three
cannot embed differently. ``memory/embedder.py`` wraps it behind the
EMBEDDING_BACKEND switch.

Configuration (all optional):

    EMBEDDING_MODEL             model repo, default all-MiniLM-L6-v2. A bare
                                name is looked up under sentence-transformers/;
                                a path to a directory holding the repo's files
                                (onnx/model.onnx, tokenizer.json, optionally
                                sentence_bert_config.json) is used as-is, with
                                no network — the air-gapped option alongside
                                HF_HUB_OFFLINE=1 with a pre-filled cache.
    EMBEDDING_ONNX_FILE         which graph in the repo, default onnx/model.onnx.
                                The repo also ships optimised and quantised
                                variants (onnx/model_O4.onnx,
                                onnx/model_qint8_arm64.onnx, ...); a quantised
                                graph is faster and smaller but NOT vector-
                                equivalent to the float model — re-embed the
                                store, and run the benchmark, before switching.
    EMBEDDING_MODEL_REVISION    git revision of the repo to fetch. The default
                                model is pinned to a known commit so a
                                maintainer re-export can never change vectors
                                under a live store; any other model follows
                                main unless you pin it.
    EMBEDDING_MAX_SEQ_LENGTH    token cap, default from the repo's
                                sentence_bert_config.json (256 for MiniLM),
                                clamped to the graph's position table.
    EMBEDDING_THREADS           onnxruntime intra-op threads, default ORT's own.
    HF_HOME / HF_HUB_OFFLINE    the usual Hugging Face cache controls. Files
                                are looked for in the cache first (no network
                                round trip when present), then downloaded once.

Pooling follows the model's own 1_Pooling/config.json (mean, CLS or max);
a model using anything else is refused at load rather than embedded into
the wrong space. The output dimension is exposed as ``dimension`` so the
caller can check it against the vector index.
"""

from __future__ import annotations

import json
import logging
import os
from typing import Any

import numpy as np

logger = logging.getLogger(__name__)

DEFAULT_MODEL = "all-MiniLM-L6-v2"
DEFAULT_ONNX_FILE = "onnx/model.onnx"
# The sentence-transformers/all-MiniLM-L6-v2 commit the 6.7 numbers were
# measured against. Pinned so an upstream re-export can't move vectors under
# a live store; EMBEDDING_MODEL_REVISION overrides.
DEFAULT_MODEL_REVISION = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41"
DEFAULT_MAX_SEQ_LENGTH = 256
DEFAULT_BATCH_SIZE = 32
MIN_SEQ_LENGTH = 3  # two special tokens plus at least one of the text
_TOKENIZER_FILE = "tokenizer.json"
_SBERT_CONFIG_FILE = "sentence_bert_config.json"
_POOLING_CONFIG_FILE = "1_Pooling/config.json"
_MODEL_CONFIG_FILE = "config.json"
_TOKEN_OUTPUT_NAMES = ("last_hidden_state", "token_embeddings")
POOLING_MODES = ("mean", "cls", "max")


class EmbeddingModelError(RuntimeError):
    """The configured model can't be run by this backend, with a message
    that says what to do about it."""


def resolve_repo(model: str) -> str:
    """'all-MiniLM-L6-v2' → 'sentence-transformers/all-MiniLM-L6-v2'; a
    name with an owner, or a local directory, is used as given."""
    model = model.strip()
    if os.path.isdir(model) or "/" in model:
        return model
    return f"sentence-transformers/{model}"


def _env_int(name: str, default: int | None) -> int | None:
    raw = os.getenv(name, "").strip()
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        logger.warning("%s=%r is not an integer, using %s", name, raw, default)
        return default


class OnnxSentenceEmbedding:
    """A sentence-transformers-shaped encoder backed by ONNX Runtime.

    ``encode(texts, normalize_embeddings=True, batch_size=32)`` matches the
    subset of the SentenceTransformer API the codebase uses, so the
    Embedder singleton and the RSS ingester call it the same way they
    called the old model. Loading is explicit (``load()``) or lazy on the
    first ``encode``.
    """

    def __init__(
        self,
        model: str | None = None,
        onnx_file: str | None = None,
        max_seq_length: int | None = None,
        threads: int | None = None,
    ) -> None:
        self.repo = resolve_repo(model or os.getenv("EMBEDDING_MODEL") or DEFAULT_MODEL)
        self.onnx_file = onnx_file or os.getenv("EMBEDDING_ONNX_FILE") or DEFAULT_ONNX_FILE
        self.revision = (os.getenv("EMBEDDING_MODEL_REVISION") or "").strip() or (
            DEFAULT_MODEL_REVISION if self.repo == resolve_repo(DEFAULT_MODEL) else None
        )
        self._max_seq_length = max_seq_length or _env_int("EMBEDDING_MAX_SEQ_LENGTH", None)
        self._threads = threads if threads is not None else _env_int("EMBEDDING_THREADS", None)
        self._session: Any = None
        self._tokenizer: Any = None
        self._input_names: list[str] = []
        self._output_index = 0
        self.pooling = "mean"
        self.dimension: int | None = None

    # -- loading ----------------------------------------------------------

    @property
    def is_loaded(self) -> bool:
        return self._session is not None

    @property
    def max_seq_length(self) -> int:
        return self._max_seq_length or DEFAULT_MAX_SEQ_LENGTH

    def load(self) -> None:
        """Download (or find in the cache) and open the graph and tokeniser."""
        if self._session is not None:
            return
        import onnxruntime as ort
        from tokenizers import Tokenizer

        logger.info("Loading ONNX embedding model %s (%s)", self.repo, self.onnx_file)
        fetch = self._fetcher()
        model_path = self._required_file(fetch, self.onnx_file)
        tokenizer_path = self._required_file(fetch, _TOKENIZER_FILE)
        self.pooling = self._configured_pooling(fetch)
        if self._max_seq_length is None:
            self._max_seq_length = self._configured_max_seq_length(fetch)
        self._max_seq_length = self._validated_max_seq_length(fetch, self._max_seq_length)

        options = ort.SessionOptions()
        # ORT's own "extended" level covers the fusions that matter on CPU;
        # the model file may already be an optimised export anyway.
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_EXTENDED
        if self._threads:
            options.intra_op_num_threads = self._threads
        session = ort.InferenceSession(
            model_path, sess_options=options, providers=["CPUExecutionProvider"],
        )
        self._input_names = [i.name for i in session.get_inputs()]
        self._output_index, output_shape = self._token_output(session)
        self.dimension = int(output_shape[-1]) if isinstance(output_shape[-1], int) else None

        self._tokenizer = Tokenizer.from_file(tokenizer_path)
        self._tokenizer.enable_truncation(max_length=self.max_seq_length)
        # Pad per batch to its longest member, not to the cap: padding to
        # 256 for a ten-token memory would quadruple the work for nothing.
        # Use the model's own [PAD] id rather than the library default of 0
        # (the same for BERT-family models, not for every tokeniser).
        pad_id = self._tokenizer.token_to_id("[PAD]")
        if pad_id is None:
            self._tokenizer.enable_padding()
        else:
            self._tokenizer.enable_padding(pad_id=pad_id, pad_token="[PAD]")
        # Assigned last: a failure anywhere above leaves is_loaded False.
        self._session = session
        logger.info(
            "ONNX embedding model loaded (dim=%s, pooling=%s, max_seq_length=%d, revision=%s)",
            self.dimension, self.pooling, self.max_seq_length, self.revision or "main",
        )

    # -- files ------------------------------------------------------------

    def _fetcher(self) -> Any:
        """``fetch(filename) -> path`` (raises FileNotFoundError when the
        repo has no such file): a local directory is read directly; anything
        else is looked for in the Hugging Face cache first — no network round
        trip when it is already there — then downloaded."""
        if os.path.isdir(self.repo):
            base = self.repo

            def from_directory(filename: str) -> str:
                path = os.path.join(base, filename)
                if not os.path.isfile(path):
                    raise FileNotFoundError(path)
                return path

            return from_directory

        from huggingface_hub import constants as hf_constants
        from huggingface_hub import hf_hub_download
        from huggingface_hub.errors import EntryNotFoundError, LocalEntryNotFoundError

        def from_hub(filename: str) -> str:
            try:
                return hf_hub_download(
                    self.repo, filename, revision=self.revision, local_files_only=True,
                )
            except Exception:
                pass  # not cached (or cached under another revision): fetch
            try:
                return hf_hub_download(self.repo, filename, revision=self.revision)
            except LocalEntryNotFoundError as exc:
                # Raised both when the hub says the file is absent *and* when
                # the hub could not be reached at all. Offline, "not cached"
                # is the whole story; online, it is a network failure and
                # must propagate — silently defaulting a config here is how
                # two processes end up embedding differently. It subclasses
                # FileNotFoundError, so it is re-raised as something the
                # optional-file readers will not mistake for "absent".
                if hf_constants.HF_HUB_OFFLINE:
                    raise FileNotFoundError(f"{self.repo}:{filename}") from exc
                raise ConnectionError(
                    f"Could not reach the Hugging Face hub for {self.repo}:{filename} "
                    "(and it is not in the local cache). Check the network, or set "
                    "HF_HUB_OFFLINE=1 to run from the cache alone."
                ) from exc
            except EntryNotFoundError as exc:
                # A genuine 404 for this file. RepositoryNotFoundError and
                # RevisionNotFoundError are not subclasses and propagate as
                # themselves — a typo'd model name is not a missing export.
                raise FileNotFoundError(f"{self.repo}:{filename}") from exc

        return from_hub

    def _required_file(self, fetch: Any, filename: str) -> str:
        """A file the backend can't run without, with a message that says
        what to do when it isn't there."""
        try:
            return fetch(filename)
        except FileNotFoundError as exc:
            raise EmbeddingModelError(
                f"The ONNX embedding backend needs {filename} from {self.repo} and "
                "could not get it. Either the model has no ONNX export (set "
                "EMBEDDING_BACKEND=torch and install mcp_server/requirements-torch.txt, "
                "or export one with optimum), or this host is offline and the file "
                "is not in the Hugging Face cache — a cache filled by the pre-6.7 "
                "torch backend holds the weights but not onnx/model.onnx, so pre-fetch "
                "it online once, or point EMBEDDING_MODEL at a directory holding the "
                "repo's files."
            ) from exc

    def _optional_json(self, fetch: Any, filename: str) -> dict | None:
        """A repo config file, or None when the repo genuinely lacks it.
        Any other failure (a hub outage, a dropped connection) propagates:
        silently falling back would pin this process to defaults the next
        process might not share."""
        try:
            path = fetch(filename)
        except FileNotFoundError:
            return None
        try:
            with open(path, encoding="utf-8") as fh:
                data = json.load(fh)
        except (OSError, ValueError) as exc:
            logger.warning("Unreadable %s for %s (%s); ignoring it", filename, self.repo, exc)
            return None
        return data if isinstance(data, dict) else None

    def _configured_pooling(self, fetch: Any) -> str:
        """The model's pooling mode from 1_Pooling/config.json.

        Mean pooling is what all-MiniLM-L6-v2 and most sentence-transformers
        models use, but a CLS-pooled model (bge, e5) embedded with mean
        pooling lands in a different space from every vector its torch
        backend ever stored — silently. So the config is read, and a mode
        this backend doesn't implement is refused rather than approximated.
        """
        config = self._optional_json(fetch, _POOLING_CONFIG_FILE)
        if config is None:
            logger.warning("No %s for %s; assuming mean pooling", _POOLING_CONFIG_FILE, self.repo)
            return "mean"
        known = {
            "mean": "pooling_mode_mean_tokens",
            "cls": "pooling_mode_cls_token",
            "max": "pooling_mode_max_tokens",
        }
        enabled = [mode for mode, key in known.items() if config.get(key)]
        others = sorted(
            key for key, on in config.items()
            if on and key.startswith("pooling_mode_") and key not in known.values()
        )
        if len(enabled) != 1 or others:
            described = others or enabled or ["no pooling mode at all"]
            raise EmbeddingModelError(
                f"{self.repo} pools with {described}, which the ONNX backend "
                f"does not implement (supported: {', '.join(POOLING_MODES)}, one at a time). "
                "Set EMBEDDING_BACKEND=torch for this model."
            )
        return enabled[0]

    def _configured_max_seq_length(self, fetch: Any) -> int:
        """The model's own cap from sentence_bert_config.json, else the default."""
        config = self._optional_json(fetch, _SBERT_CONFIG_FILE)
        try:
            value = int((config or {}).get("max_seq_length", DEFAULT_MAX_SEQ_LENGTH))
        except (TypeError, ValueError):
            value = 0
        if value < MIN_SEQ_LENGTH:
            logger.warning("No usable max_seq_length in %s for %s; using %d",
                           _SBERT_CONFIG_FILE, self.repo, DEFAULT_MAX_SEQ_LENGTH)
            return DEFAULT_MAX_SEQ_LENGTH
        return value

    def _validated_max_seq_length(self, fetch: Any, value: int) -> int:
        """Bound the cap: at least the two special tokens plus one, and no
        more than the graph's position table (config.json
        max_position_embeddings), which is where an over-long input crashes
        inside the first remember() rather than at startup."""
        if value < MIN_SEQ_LENGTH:
            logger.warning("EMBEDDING_MAX_SEQ_LENGTH=%d is below %d; using %d",
                           value, MIN_SEQ_LENGTH, DEFAULT_MAX_SEQ_LENGTH)
            value = DEFAULT_MAX_SEQ_LENGTH
        config = self._optional_json(fetch, _MODEL_CONFIG_FILE) or {}
        try:
            positions = int(config.get("max_position_embeddings", 0))
        except (TypeError, ValueError):
            positions = 0
        if positions and value > positions:
            logger.warning("max_seq_length %d exceeds the model's %d positions; clamping",
                           value, positions)
            value = positions
        return value

    @staticmethod
    def _token_output(session: Any) -> tuple[int, list]:
        """Index and shape of the per-token output. Chosen by name when the
        graph exports several, and checked to be rank 3 so a graph whose
        first output is already pooled fails here, not on the first batch."""
        outputs = session.get_outputs()
        index = next(
            (i for i, o in enumerate(outputs) if o.name in _TOKEN_OUTPUT_NAMES), 0,
        )
        shape = list(outputs[index].shape)
        if len(shape) != 3:
            raise EmbeddingModelError(
                f"Output {outputs[index].name!r} has shape {shape}; the ONNX backend needs "
                "a [batch, tokens, dim] token-embedding output (last_hidden_state)."
            )
        return index, shape

    # -- encoding ---------------------------------------------------------

    def encode(
        self,
        texts: str | list[str],
        normalize_embeddings: bool = True,
        batch_size: int = DEFAULT_BATCH_SIZE,
        **ignored: Any,
    ) -> np.ndarray:
        """Embed one text (→ 1-D float32) or a list (→ 2-D float32).

        Mean pooling over the attention mask, then L2 normalisation — the
        pooling stack the sentence-transformers model card specifies.
        ``normalize_embeddings=False`` returns the raw pooled vectors; note
        that sentence-transformers' own all-MiniLM-L6-v2 carries a Normalize
        module and returns unit vectors regardless, so the two backends only
        agree for the normalised call, which is the only one this codebase
        makes. Extra keyword arguments are accepted (and logged at debug)
        so SentenceTransformer call sites keep working.
        """
        if ignored:
            logger.debug("encode(): ignoring unsupported arguments %s", sorted(ignored))
        if self._session is None:
            self.load()
        single = isinstance(texts, str)
        batch = [texts] if single else list(texts)
        if not batch:
            return np.zeros((0, self.dimension or 0), dtype=np.float32)

        batch_size = max(1, int(batch_size))
        chunks = [
            self._encode_chunk(batch[i:i + batch_size])
            for i in range(0, len(batch), batch_size)
        ]
        vectors = np.concatenate(chunks, axis=0) if len(chunks) > 1 else chunks[0]
        if normalize_embeddings:
            norms = np.linalg.norm(vectors, axis=1, keepdims=True)
            vectors = vectors / np.clip(norms, 1e-12, None)
        vectors = vectors.astype(np.float32, copy=False)
        return vectors[0] if single else vectors

    def _encode_chunk(self, texts: list[str]) -> np.ndarray:
        encodings = self._tokenizer.encode_batch(texts)
        input_ids = np.asarray([e.ids for e in encodings], dtype=np.int64)
        attention_mask = np.asarray([e.attention_mask for e in encodings], dtype=np.int64)
        feeds: dict[str, np.ndarray] = {"input_ids": input_ids, "attention_mask": attention_mask}
        if "token_type_ids" in self._input_names:
            feeds["token_type_ids"] = np.asarray([e.type_ids for e in encodings], dtype=np.int64)

        token_embeddings = self._session.run(None, feeds)[self._output_index]  # [batch, tokens, dim]
        return pool(token_embeddings, attention_mask, self.pooling)


def pool(token_embeddings: np.ndarray, attention_mask: np.ndarray, mode: str) -> np.ndarray:
    """sentence-transformers' Pooling module in numpy, for the modes it
    is safe to reproduce: mean over the attention mask, the CLS (first)
    token, or the element-wise max over unmasked tokens."""
    if mode == "cls":
        return token_embeddings[:, 0, :]
    mask = attention_mask[..., None].astype(np.float32)
    if mode == "max":
        masked = np.where(mask > 0, token_embeddings, -np.inf)
        return masked.max(axis=1)
    summed = (token_embeddings * mask).sum(axis=1)
    counts = np.clip(mask.sum(axis=1), 1e-9, None)
    return summed / counts
