# Stage 1: Build — install dependencies with build tools
FROM python:3.12-slim AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends gcc g++ libopenblas-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY web_ui/requirements.txt .
RUN pip install --no-cache-dir --prefer-binary torch --index-url https://download.pytorch.org/whl/cpu && \
    pip install --no-cache-dir --prefer-binary -r requirements.txt

# Stage 2: Runtime — slim image without build tools
FROM python:3.12-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends libgomp1 libopenblas0 && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /usr/local/lib/python3.12/site-packages /usr/local/lib/python3.12/site-packages
COPY --from=builder /usr/local/bin /usr/local/bin

# Copy the shared memory package from the MCP server
COPY mcp_server/memory/ /app/memory/

# Copy the web UI application
COPY web_ui/ /app/web_ui/

RUN mkdir -p /app/backups

RUN adduser --disabled-password --gecos "" webuser && chown -R webuser:webuser /app
USER webuser

ENTRYPOINT ["python", "-m", "uvicorn", "web_ui.app:app", "--host", "0.0.0.0", "--port", "8080"]
