"""Lifecycle management POST endpoints: deprioritise, archive, reinstate, delete."""

import logging

from starlette.requests import Request
from starlette.responses import RedirectResponse
from starlette.routing import Route

from memory.lifecycle import MemoryState

from .. import deps

logger = logging.getLogger(__name__)


async def deprioritise(request: Request) -> RedirectResponse:
    """POST /lifecycle/deprioritise — deprioritise a memory."""
    form = await request.form()
    key = form.get("key", "")
    reason = form.get("reason", "Deprioritised via web UI")

    try:
        deps.lifecycle.transition(key, MemoryState.DEPRIORITISED, reason=reason)
    except ValueError as exc:
        logger.warning("Failed to deprioritise %s: %s", key, exc)

    return RedirectResponse(url=f"/memory/{key}", status_code=303)


async def archive(request: Request) -> RedirectResponse:
    """POST /lifecycle/archive — archive a memory."""
    form = await request.form()
    key = form.get("key", "")

    try:
        deps.lifecycle.transition(key, MemoryState.ARCHIVED)
    except ValueError as exc:
        logger.warning("Failed to archive %s: %s", key, exc)

    return RedirectResponse(url=f"/memory/{key}", status_code=303)


async def reinstate(request: Request) -> RedirectResponse:
    """POST /lifecycle/reinstate — reinstate a memory to active."""
    form = await request.form()
    key = form.get("key", "")

    try:
        deps.lifecycle.transition(key, MemoryState.ACTIVE)
        deps.store.set_fields(key, {"deprioritised_reason": "", "surface_score": "1.0"})
    except ValueError as exc:
        logger.warning("Failed to reinstate %s: %s", key, exc)

    return RedirectResponse(url=f"/memory/{key}", status_code=303)


async def delete(request: Request) -> RedirectResponse:
    """POST /lifecycle/delete — permanently delete a memory."""
    form = await request.form()
    key = form.get("key", "")

    try:
        deps.lifecycle.transition(key, MemoryState.DELETED)
    except ValueError:
        # If transition fails (e.g. already deleted), force delete
        try:
            deps.store.delete(key)
        except Exception as exc:
            logger.warning("Failed to delete %s: %s", key, exc)

    return RedirectResponse(url="/memories", status_code=303)


routes = [
    Route("/lifecycle/deprioritise", deprioritise, methods=["POST"]),
    Route("/lifecycle/archive", archive, methods=["POST"]),
    Route("/lifecycle/reinstate", reinstate, methods=["POST"]),
    Route("/lifecycle/delete", delete, methods=["POST"]),
]
