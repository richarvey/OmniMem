"""Singleton embedding wrapper with a selectable backend.

``EMBEDDING_BACKEND`` picks the engine:

    onnx    (default since v6.7) memory/onnx_embedding.py — ONNX Runtime plus
            the Rust tokenizers library. No PyTorch in the image.
    torch   the pre-6.7 path: sentence-transformers on PyTorch. Not installed
            by default any more; ``pip install sentence-transformers`` to use
            it. Kept as the rollback and as the reference the ONNX backend is
            benchmarked against (scripts/embedding_bench.py).

Both engines expose ``encode(texts, normalize_embeddings=True)`` and produce
the same vectors for the same model, so nothing above this module knows
which one is running.
"""

import logging
import os
from typing import Any, ClassVar

import numpy as np

from .store import VECTOR_DIM

logger = logging.getLogger(__name__)

BACKEND_ONNX = "onnx"
BACKEND_TORCH = "torch"
BACKENDS = (BACKEND_ONNX, BACKEND_TORCH)


def configured_backend() -> str:
    """The backend EMBEDDING_BACKEND names, defaulting to onnx."""
    raw = os.getenv("EMBEDDING_BACKEND", BACKEND_ONNX).strip().lower()
    if raw not in BACKENDS:
        raise ValueError(
            f"EMBEDDING_BACKEND={raw!r} is not one of {', '.join(BACKENDS)}"
        )
    return raw


def build_model(backend: str | None = None) -> Any:
    """Instantiate (but don't load) the configured engine."""
    backend = backend or configured_backend()
    model_name = os.getenv("EMBEDDING_MODEL", "all-MiniLM-L6-v2")
    if backend == BACKEND_TORCH:
        try:
            from sentence_transformers import SentenceTransformer
        except ImportError as exc:
            raise RuntimeError(TORCH_INSTALL_HINT) from exc
        logger.info("Loading embedding model %s (torch backend)", model_name)
        model = SentenceTransformer(model_name)
        check_dimension(model.get_sentence_embedding_dimension(), model_name)
        return model
    from .onnx_embedding import OnnxSentenceEmbedding

    engine = OnnxSentenceEmbedding(model_name)
    engine.load()
    check_dimension(engine.dimension, model_name)
    return engine


TORCH_INSTALL_HINT = (
    "EMBEDDING_BACKEND=torch needs sentence-transformers, which the images no "
    "longer ship. Install the CPU wheel first or pip pulls CUDA builds:\n"
    "  pip install --prefer-binary torch --index-url https://download.pytorch.org/whl/cpu\n"
    "  pip install -r mcp_server/requirements-torch.txt"
)


def check_dimension(dimension: int | None, model_name: str) -> None:
    """Refuse a model whose vectors don't fit the index.

    The HNSW indexes are declared at VECTOR_DIM; a wider vector is written
    without complaint and then silently invisible to every search, dedup
    and maintenance pass. Better to stop at startup.
    """
    if dimension is not None and dimension != VECTOR_DIM:
        raise RuntimeError(
            f"Embedding model {model_name} produces {dimension}-dimensional vectors "
            f"but the store's indexes are built for {VECTOR_DIM}. Every stored vector "
            "would need re-embedding and the indexes rebuilding; pick a "
            f"{VECTOR_DIM}-dimensional model (the default all-MiniLM-L6-v2 is one)."
        )


class Embedder:
    """Loads the embedding model once on startup. Provides embed and embed_batch."""

    _instance: ClassVar["Embedder | None"] = None
    _model: Any = None
    _backend: str | None = None

    def __new__(cls) -> "Embedder":
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def load(self) -> None:
        """Load the model. Call once on startup."""
        if self._model is not None:
            return
        backend = configured_backend()
        self._model = build_model(backend)
        self._backend = backend
        logger.info("Embedding model loaded successfully (%s backend)", backend)

    @property
    def model(self) -> Any:
        if self._model is None:
            self.load()
        return self._model

    @property
    def backend(self) -> str | None:
        """Which engine is loaded, or None before load()."""
        return self._backend

    @property
    def is_loaded(self) -> bool:
        return self._model is not None

    def embed(self, text: str) -> np.ndarray:
        """Return a normalised float32 vector for a single text."""
        vector = self.model.encode(text, normalize_embeddings=True)
        return np.array(vector, dtype=np.float32)

    def embed_batch(self, texts: list[str]) -> list[np.ndarray]:
        """Return normalised float32 vectors for a batch of texts."""
        vectors = self.model.encode(texts, normalize_embeddings=True)
        return [np.array(v, dtype=np.float32) for v in vectors]
