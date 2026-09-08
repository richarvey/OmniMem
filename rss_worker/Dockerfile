# Stage 1: Build — install dependencies with build tools
FROM python:3.12-slim AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends gcc g++ && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY rss_worker/requirements.txt .
# v6.7: embeddings run on ONNX Runtime — no PyTorch, which was most of the
# image. EMBEDDING_BACKEND=torch needs mcp_server/requirements-torch.txt on top
# (CPU wheel first — see that file's header).
RUN pip install --no-cache-dir --prefer-binary -r requirements.txt

# Stage 2: Runtime — slim image without build tools
FROM python:3.12-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends libgomp1 && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /usr/local/lib/python3.12/site-packages /usr/local/lib/python3.12/site-packages
COPY --from=builder /usr/local/bin /usr/local/bin

# The shared memory package (embedding backend, licence/provenance
# vocabularies) — built from the repo root like web_ui, so the worker runs
# the same code as the server instead of a copy.
COPY mcp_server/memory/ /app/memory/
COPY rss_worker/ /app/

# Model files land here; compose mounts a shared volume on it so a
# recreated container doesn't re-download.
ENV HF_HOME=/app/hf-cache
RUN mkdir -p /app/hf-cache && \
    adduser --disabled-password --gecos "" rssuser && chown -R rssuser:rssuser /app
USER rssuser

ENTRYPOINT ["python", "worker.py"]
