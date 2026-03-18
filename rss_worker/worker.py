"""RSS ingestion scheduler entry point with feeds.yml file watcher."""

import logging
import os
import sys
import threading
import time

import valkey
from apscheduler.schedulers.blocking import BlockingScheduler
from dotenv import load_dotenv

from ingester import ingest_all_feeds

load_dotenv()

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("rss_worker")

FEEDS_PATH = os.getenv("FEEDS_CONFIG_PATH", "/app/feeds.yml")
WATCH_INTERVAL = int(os.getenv("FEEDS_WATCH_INTERVAL", "10"))


def _wait_for_valkey(max_retries: int = 5) -> None:
    """Wait for Valkey to become available with exponential backoff."""
    host = os.getenv("VALKEY_HOST", "valkey")
    port = int(os.getenv("VALKEY_PORT", "6379"))
    password = os.getenv("VALKEY_PASSWORD", "")

    for attempt in range(1, max_retries + 1):
        try:
            client = valkey.Valkey(host=host, port=port, password=password)
            client.ping()
            logger.info("Valkey is available at %s:%d", host, port)
            client.close()
            return
        except (valkey.ConnectionError, valkey.TimeoutError) as exc:
            delay = 2 ** attempt
            logger.warning(
                "Valkey connection attempt %d/%d failed: %s. Retrying in %ds...",
                attempt, max_retries, exc, delay,
            )
            if attempt == max_retries:
                logger.error("Failed to connect to Valkey after %d attempts", max_retries)
                sys.exit(1)
            time.sleep(delay)


def run_ingestion() -> None:
    """Run a single ingestion cycle."""
    logger.info("Starting RSS ingestion cycle...")
    result = ingest_all_feeds(FEEDS_PATH)
    logger.info("Ingestion result: %s", result)


def _get_mtime(path: str) -> float:
    """Get file modification time, returning 0 if file doesn't exist."""
    try:
        return os.path.getmtime(path)
    except OSError:
        return 0.0


def _watch_feeds_file(scheduler: BlockingScheduler) -> None:
    """Poll feeds.yml for changes and trigger re-ingestion when modified."""
    last_mtime = _get_mtime(FEEDS_PATH)
    logger.info("Watching %s for changes (polling every %ds)", FEEDS_PATH, WATCH_INTERVAL)

    while True:
        time.sleep(WATCH_INTERVAL)
        current_mtime = _get_mtime(FEEDS_PATH)

        if current_mtime > last_mtime:
            logger.info("feeds.yml changed (mtime %s → %s), triggering re-ingestion",
                        last_mtime, current_mtime)
            last_mtime = current_mtime
            try:
                scheduler.add_job(run_ingestion, id="file_change_trigger", replace_existing=True)
            except Exception:
                logger.exception("Failed to schedule re-ingestion after file change")


def main() -> None:
    """Entry point: wait for Valkey, run initial ingestion, then schedule repeats."""
    _wait_for_valkey()

    # Run immediately on startup
    run_ingestion()

    # Schedule repeating runs
    schedule_hours = int(os.getenv("RSS_SCHEDULE_HOURS", "6"))
    logger.info("Scheduling RSS ingestion every %d hours", schedule_hours)

    scheduler = BlockingScheduler()
    scheduler.add_job(
        run_ingestion,
        "interval",
        hours=schedule_hours,
        misfire_grace_time=schedule_hours * 3600,
        coalesce=True,
    )

    # Start file watcher in a background daemon thread
    watcher = threading.Thread(target=_watch_feeds_file, args=(scheduler,), daemon=True)
    watcher.start()

    try:
        scheduler.start()
    except (KeyboardInterrupt, SystemExit):
        logger.info("RSS worker shutting down")


if __name__ == "__main__":
    main()
