"""Singleton sentence-transformers embedding wrapper."""

import logging
import os
from typing import ClassVar

import numpy as np
from sentence_transformers import SentenceTransformer

logger = logging.getLogger(__name__)


class Embedder:
    """Loads embedding model once on startup. Provides embed and embed_batch methods."""

    _instance: ClassVar["Embedder | None"] = None
    _model: SentenceTransformer | None = None

    def __new__(cls) -> "Embedder":
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def load(self) -> None:
        """Load the model. Call once on startup."""
        if self._model is not None:
            return
        model_name = os.getenv("EMBEDDING_MODEL", "all-MiniLM-L6-v2")
        logger.info("Loading embedding model: %s", model_name)
        self._model = SentenceTransformer(model_name)
        logger.info("Embedding model loaded successfully")

    @property
    def model(self) -> SentenceTransformer:
        if self._model is None:
            self.load()
        return self._model

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
