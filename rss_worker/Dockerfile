FROM python:3.12-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends gcc g++ libopenblas-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir --prefer-binary torch --index-url https://download.pytorch.org/whl/cpu && \
    pip install --no-cache-dir --prefer-binary -r requirements.txt

COPY . .

RUN adduser --disabled-password --gecos "" rssuser && chown -R rssuser:rssuser /app
USER rssuser

ENTRYPOINT ["python", "worker.py"]
