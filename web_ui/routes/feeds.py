"""RSS feed management routes: list, create, edit, delete, download, upload.

Feeds can carry skill influence associations — a `skills:` mapping of skill
domain to influence score (1-10) per feed in feeds.yml. Every write here also
mirrors the feed list into Valkey (memory/feed_influence.py) so the skill
compiler, which runs without the feeds.yml mount, sees the current scores.
"""

import logging
import os
from urllib.parse import quote

import yaml
from starlette.requests import Request
from starlette.responses import FileResponse, HTMLResponse, RedirectResponse
from starlette.routing import Route

from memory.feed_influence import sync_feed_influences, validate_feed_skills
from memory.licence import (
    LICENCE_CHOICES,
    LICENCE_UNKNOWN,
    note_for_reclassification,
    resolve_licence,
    validate_licence_note,
)
from memory.skills import SKILL_KEY_PREFIX

from .. import deps

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


def _sync_influence(feeds: list[dict]) -> None:
    """Mirror feeds into Valkey for the skill compiler; never block the save."""
    try:
        sync_feed_influences(deps.store.client, feeds)
    except Exception:
        logger.exception("Failed to mirror feed influence into Valkey")


def _known_skill_domains() -> list[str]:
    """Domains of compiled skills, for the edit form's suggestion list."""
    try:
        keys = deps.store.scan_prefix(SKILL_KEY_PREFIX)
        rows = deps.store.get_fields_multi(keys, ("domain",)) if keys else []
    except Exception:
        return []
    domains = {(row or {}).get("domain") for row in rows}
    return sorted(d for d in domains if d)


def _skills_summary(feed: dict) -> str:
    """'python (8), docker (3)' for the list view; strongest first."""
    skills = feed.get("skills")
    if not isinstance(skills, dict) or not skills:
        return ""
    pairs = sorted(skills.items(), key=lambda kv: (-int(kv[1]), kv[0]))
    return ", ".join(f"{domain} ({score})" for domain, score in pairs)


def _parse_skills_form(form) -> dict[str, int]:
    """Paired skill_domain / skill_influence rows into a skills mapping.

    Blank domain rows are dropped (that's how the form removes an
    association). Raises ValueError with a user-facing message on a bad
    domain or score — validation itself lives in memory/feed_influence.py.
    """
    domains = form.getlist("skill_domain")
    influences = form.getlist("skill_influence")
    raw: dict[str, str] = {}
    for domain, influence in zip(domains, influences):
        domain = (domain or "").strip()
        if not domain:
            continue
        raw[domain] = (influence or "").strip() or "5"
    return validate_feed_skills(raw)


def _form_error_redirect(url: str, exc: Exception) -> RedirectResponse:
    return RedirectResponse(url=f"{url}?error={quote(str(exc))}", status_code=303)


def _feed_licence(feed: dict) -> tuple[str, str]:
    """(class, note) a feed declares, for display. Unparseable → unknown,
    matching what the ingester would stamp on its articles."""
    raw = feed.get("licence")
    try:
        # A YAML boolean (`licence: no`) is a mistyped declaration, not an
        # absent one — it must not fold to the empty-string alias.
        licence_class, derived = resolve_licence(raw if isinstance(raw, str) or raw is None else str(raw))
    except ValueError:
        licence_class, derived = LICENCE_UNKNOWN, None
    note = str(feed.get("licence_note") or "").strip() or derived or ""
    return licence_class, note


def _parse_licence_form(form, current: dict | None = None) -> dict:
    """licence / licence_note fields into the feeds.yml keys to store.

    The form offers the four classes, so a feed hand-written with an
    identifier (`licence: cc-by-4.0`) is shown as its class with the
    identifier in the note, and saving writes it back in that split form —
    the articles it stamps are identical either way. An empty choice stores
    nothing, which ingests as unknown. Because the note is pre-filled from
    the current declaration, a note that matches it is dropped when the
    class changes: switching Open → Restricted must not carry "OGL v3.0"
    onto every future article. Raises ValueError on an unrecognised value
    or an over-long note.
    """
    raw = (form.get("licence") or "").strip()
    entry: dict = {}
    if raw:
        resolve_licence(raw)
        entry["licence"] = raw
    note = validate_licence_note(form.get("licence_note") or "")
    if current is not None:
        old_class, old_note = _feed_licence(current)
        note = note_for_reclassification(
            old_class if current.get("licence") else "", old_note,
            resolve_licence(raw)[0] if raw else "", note,
        )
    if note:
        entry["licence_note"] = note
    return entry


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
            "digest": feed.get("mode") == "digest",
            "skills": _skills_summary(feed),
            "licence": _feed_licence(feed)[0],
        })

    message = request.query_params.get("message")
    error = request.query_params.get("error")
    template = request.app.state.templates.get_template("feeds/list.html")
    content = template.render(
        request=request, feeds=items, current_page="feeds",
        message=message, error=error,
    )
    return HTMLResponse(content)


def _skills_rows(feed: dict) -> list[dict]:
    skills = feed.get("skills")
    if not isinstance(skills, dict):
        return []
    return [
        {"domain": domain, "influence": score}
        for domain, score in sorted(
            skills.items(), key=lambda kv: (-int(kv[1]), kv[0]),
        )
    ]


async def feed_create_form(request: Request) -> HTMLResponse:
    """GET /feeds/new — form to add a new feed."""
    feed = {"name": "", "url": "", "topics": "", "digest": False, "skills": [],
            "licence": "", "licence_note": ""}
    template = request.app.state.templates.get_template("feeds/edit.html")
    content = template.render(
        request=request, feed=feed, current_page="feeds", is_new=True,
        skill_domains=_known_skill_domains(), licence_classes=LICENCE_CHOICES,
        error=request.query_params.get("error"),
    )
    return HTMLResponse(content)


async def feed_create(request: Request) -> RedirectResponse:
    """POST /feeds/new — add a new feed to feeds.yml."""
    form = await request.form()
    name = form.get("name", "").strip()
    url = form.get("url", "").strip()
    topics_raw = form.get("topics", "").strip()
    digest = form.get("digest") == "on"

    if not name or not url:
        return RedirectResponse(url="/feeds/new", status_code=303)

    try:
        skills = _parse_skills_form(form)
        licence_entry = _parse_licence_form(form)
    except ValueError as exc:
        return _form_error_redirect("/feeds/new", exc)

    topics = [t.strip() for t in topics_raw.split(",") if t.strip()] if topics_raw else []

    feed_entry: dict = {"url": url, "name": name, "topics": topics}
    if digest:
        feed_entry["mode"] = "digest"
    if skills:
        feed_entry["skills"] = skills
    feed_entry.update(licence_entry)

    feeds = _load_feeds()
    feeds.append(feed_entry)
    _save_feeds(feeds)
    _sync_influence(feeds)

    logger.info("Added RSS feed: %s (%s)", name, url)
    return RedirectResponse(url="/feeds", status_code=303)


async def feed_edit_form(request: Request) -> HTMLResponse:
    """GET /feeds/{index}/edit — edit form for an existing feed."""
    index = int(request.path_params["index"])
    feeds = _load_feeds()

    if index < 0 or index >= len(feeds):
        return HTMLResponse('<p class="empty-state">Feed not found.</p>', status_code=404)

    raw = feeds[index]
    licence_class, licence_note = _feed_licence(raw)
    feed = {
        "index": index,
        "name": raw.get("name", ""),
        "url": raw.get("url", ""),
        "topics": ", ".join(raw.get("topics", [])),
        "digest": raw.get("mode") == "digest",
        "skills": _skills_rows(raw),
        # The form offers the four classes; a declared identifier shows as
        # its class with the identifier in the note.
        "licence": licence_class if raw.get("licence") else "",
        "licence_note": licence_note,
    }

    template = request.app.state.templates.get_template("feeds/edit.html")
    content = template.render(
        request=request, feed=feed, current_page="feeds", is_new=False,
        skill_domains=_known_skill_domains(), licence_classes=LICENCE_CHOICES,
        error=request.query_params.get("error"),
    )
    return HTMLResponse(content)


async def feed_save(request: Request) -> RedirectResponse:
    """POST /feeds/{index}/edit — update an existing feed."""
    index = int(request.path_params["index"])
    form = await request.form()

    name = form.get("name", "").strip()
    url = form.get("url", "").strip()
    topics_raw = form.get("topics", "").strip()
    digest = form.get("digest") == "on"

    if not name or not url:
        return RedirectResponse(url=f"/feeds/{index}/edit", status_code=303)

    feeds = _load_feeds()
    if index < 0 or index >= len(feeds):
        return RedirectResponse(url="/feeds", status_code=303)

    try:
        skills = _parse_skills_form(form)
        licence_entry = _parse_licence_form(form, current=feeds[index])
    except ValueError as exc:
        return _form_error_redirect(f"/feeds/{index}/edit", exc)

    topics = [t.strip() for t in topics_raw.split(",") if t.strip()] if topics_raw else []

    feed_entry: dict = {"url": url, "name": name, "topics": topics}
    if digest:
        feed_entry["mode"] = "digest"
    if skills:
        feed_entry["skills"] = skills
    feed_entry.update(licence_entry)
    feeds[index] = feed_entry
    _save_feeds(feeds)
    _sync_influence(feeds)

    logger.info("Updated RSS feed #%d: %s (%s)", index, name, url)
    return RedirectResponse(url="/feeds", status_code=303)


async def feed_delete(request: Request) -> RedirectResponse:
    """POST /feeds/{index}/delete — remove a feed."""
    index = int(request.path_params["index"])
    feeds = _load_feeds()

    if 0 <= index < len(feeds):
        removed = feeds.pop(index)
        _save_feeds(feeds)
        _sync_influence(feeds)
        logger.info("Deleted RSS feed #%d: %s", index, removed.get("name", ""))

    return RedirectResponse(url="/feeds", status_code=303)


async def feed_download(request: Request):
    """GET /feeds/download — download the feeds.yml file."""
    if not os.path.exists(FEEDS_PATH):
        return RedirectResponse(url="/feeds?error=No+feeds.yml+file+found", status_code=303)

    return FileResponse(
        FEEDS_PATH,
        media_type="application/x-yaml",
        filename="feeds.yml",
    )


async def feed_upload(request: Request) -> RedirectResponse:
    """POST /feeds/upload — upload a YAML file to replace feeds.yml."""
    form = await request.form()
    upload = form.get("file")

    if not upload or not upload.filename:
        return RedirectResponse(url="/feeds?error=No+file+selected", status_code=303)

    # Only allow .yml / .yaml files
    if not upload.filename.lower().endswith((".yml", ".yaml")):
        return RedirectResponse(
            url="/feeds?error=Only+.yml+or+.yaml+files+are+accepted", status_code=303,
        )

    raw = await upload.read()

    # Validate YAML structure
    try:
        data = yaml.safe_load(raw)
    except yaml.YAMLError as exc:
        logger.warning("Uploaded feeds file is not valid YAML: %s", exc)
        return RedirectResponse(url="/feeds?error=Invalid+YAML+file", status_code=303)

    if not isinstance(data, dict) or "feeds" not in data:
        return RedirectResponse(
            url="/feeds?error=YAML+must+contain+a+top-level+'feeds'+key", status_code=303,
        )

    if not isinstance(data["feeds"], list):
        return RedirectResponse(
            url="/feeds?error='feeds'+must+be+a+list", status_code=303,
        )

    # Write the validated file — mtime change will trigger rss_worker reload
    with open(FEEDS_PATH, "wb") as f:
        f.write(raw)
    _sync_influence(data["feeds"])

    logger.info("Uploaded new feeds.yml (%d feeds) from %s", len(data["feeds"]), upload.filename)
    return RedirectResponse(
        url="/feeds?message=Feeds+config+uploaded+successfully.+RSS+worker+will+reload+automatically.",
        status_code=303,
    )


routes = [
    Route("/feeds", feed_list),
    Route("/feeds/new", feed_create_form),
    Route("/feeds/new", feed_create, methods=["POST"]),
    Route("/feeds/download", feed_download),
    Route("/feeds/upload", feed_upload, methods=["POST"]),
    Route("/feeds/{index:int}/edit", feed_edit_form),
    Route("/feeds/{index:int}/edit", feed_save, methods=["POST"]),
    Route("/feeds/{index:int}/delete", feed_delete, methods=["POST"]),
]
