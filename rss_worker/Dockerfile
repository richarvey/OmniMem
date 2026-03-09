FROM python:3.12-alpine

RUN apk add --no-cache gcc musl-dev g++ openblas-dev lapack-dev

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir --prefer-binary -r requirements.txt

COPY . .

RUN adduser -D rssuser && chown -R rssuser:rssuser /app
USER rssuser

ENTRYPOINT ["python", "worker.py"]
