"""RSS feed management routes: list, create, edit, delete."""

import logging
import os

import yaml
from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse
from starlette.routing import Route

logger = logging.getLogger(__name__)

FEEDS_PATH = os.getenv("FEEDS_CONFIG_PATH", "/app/feeds.yml")


def _load_feeds() -> list[dict]:
    """Load feeds list from YAML file."""
    try:
        with open(FEEDS_PATH) as f:
            data = yaml.safe_load(f)
        return data.get("feeds", []) if data else []
    except FileNotFoundError:
        logger.warning("feeds.yml not found at %s", FEEDS_PATH)
        return []


def _save_feeds(feeds: list[dict]) -> None:
    """Write feeds list back to YAML file."""
    with open(FEEDS_PATH, "w") as f:
        yaml.dump(
            {"feeds": feeds},
            f,
            default_flow_style=False,
            sort_keys=False,
            allow_unicode=True,
        )


async def feed_list(request: Request) -> HTMLResponse:
    """GET /feeds — list all configured RSS feeds."""
    feeds = _load_feeds()
    items = []
    for i, feed in enumerate(feeds):
        items.append({
            "index": i,
            "name": feed.get("name", ""),
            "url": feed.get("url", ""),
            "topics": ", ".join(feed.get("topics", [])),
        })

    template = request.app.state.templates.get_template("feeds/list.html")
    content = template.render(request=request, feeds=items, current_page="feeds")
    return HTMLResponse(content)


async def feed_create_form(request: Request) -> HTMLResponse:
    """GET /feeds/new — form to add a new feed."""
    feed = {"name": "", "url": "", "topics": ""}
    template = request.app.state.templates.get_template("feeds/edit.html")
    content = template.render(
        request=request, feed=feed, current_page="feeds", is_new=True,
    )
    return HTMLResponse(content)


async def feed_create(request: Request) -> RedirectResponse:
    """POST /feeds/new — add a new feed to feeds.yml."""
    form = await request.form()
    name = form.get("name", "").strip()
    url = form.get("url", "").strip()
    topics_raw = form.get("topics", "").strip()

    if not name or not url:
        return RedirectResponse(url="/feeds/new", status_code=303)

    topics = [t.strip() for t in topics_raw.split(",") if t.strip()] if topics_raw else []

    feeds = _load_feeds()
    feeds.append({"url": url, "name": name, "topics": topics})
    _save_feeds(feeds)

    logger.info("Added RSS feed: %s (%s)", name, url)
    return RedirectResponse(url="/feeds", status_code=303)


async def feed_edit_form(request: Request) -> HTMLResponse:
    """GET /feeds/{index}/edit — edit form for an existing feed."""
    index = int(request.path_params["index"])
    feeds = _load_feeds()

    if index < 0 or index >= len(feeds):
        return HTMLResponse('<p class="empty-state">Feed not found.</p>', status_code=404)

    raw = feeds[index]
    feed = {
        "index": index,
        "name": raw.get("name", ""),
        "url": raw.get("url", ""),
        "topics": ", ".join(raw.get("topics", [])),
    }

    template = request.app.state.templates.get_template("feeds/edit.html")
    content = template.render(
        request=request, feed=feed, current_page="feeds", is_new=False,
    )
    return HTMLResponse(content)


async def feed_save(request: Request) -> RedirectResponse:
    """POST /feeds/{index}/edit — update an existing feed."""
    index = int(request.path_params["index"])
    form = await request.form()

    name = form.get("name", "").strip()
    url = form.get("url", "").strip()
    topics_raw = form.get("topics", "").strip()

    if not name or not url:
        return RedirectResponse(url=f"/feeds/{index}/edit", status_code=303)

    topics = [t.strip() for t in topics_raw.split(",") if t.strip()] if topics_raw else []

    feeds = _load_feeds()
    if index < 0 or index >= len(feeds):
        return RedirectResponse(url="/feeds", status_code=303)

    feeds[index] = {"url": url, "name": name, "topics": topics}
    _save_feeds(feeds)

    logger.info("Updated RSS feed #%d: %s (%s)", index, name, url)
    return RedirectResponse(url="/feeds", status_code=303)


async def feed_delete(request: Request) -> RedirectResponse:
    """POST /feeds/{index}/delete — remove a feed."""
    index = int(request.path_params["index"])
    feeds = _load_feeds()

    if 0 <= index < len(feeds):
        removed = feeds.pop(index)
        _save_feeds(feeds)
        logger.info("Deleted RSS feed #%d: %s", index, removed.get("name", ""))

    return RedirectResponse(url="/feeds", status_code=303)


routes = [
    Route("/feeds", feed_list),
    Route("/feeds/new", feed_create_form),
    Route("/feeds/new", feed_create, methods=["POST"]),
    Route("/feeds/{index:int}/edit", feed_edit_form),
    Route("/feeds/{index:int}/edit", feed_save, methods=["POST"]),
    Route("/feeds/{index:int}/delete", feed_delete, methods=["POST"]),
]
