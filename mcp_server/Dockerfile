FROM python:3.12-alpine

RUN apk add --no-cache gcc musl-dev g++ openblas-dev lapack-dev

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir --prefer-binary -r requirements.txt

COPY . .

RUN mkdir -p /app/backups

RUN adduser -D mcpuser && chown -R mcpuser:mcpuser /app
USER mcpuser

ENTRYPOINT ["python", "server.py"]
