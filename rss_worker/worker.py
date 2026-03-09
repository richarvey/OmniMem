"""RSS ingestion scheduler entry point."""

import logging
import os
import sys
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
    feeds_path = os.getenv("FEEDS_CONFIG_PATH", "/app/feeds.yml")
    logger.info("Starting RSS ingestion cycle...")
    result = ingest_all_feeds(feeds_path)
    logger.info("Ingestion result: %s", result)


def main() -> None:
    """Entry point: wait for Valkey, run initial ingestion, then schedule repeats."""
    _wait_for_valkey()

    # Run immediately on startup
    run_ingestion()

    # Schedule repeating runs
    schedule_hours = int(os.getenv("RSS_SCHEDULE_HOURS", "6"))
    logger.info("Scheduling RSS ingestion every %d hours", schedule_hours)

    scheduler = BlockingScheduler()
    scheduler.add_job(run_ingestion, "interval", hours=schedule_hours)

    try:
        scheduler.start()
    except (KeyboardInterrupt, SystemExit):
        logger.info("RSS worker shutting down")


if __name__ == "__main__":
    main()
