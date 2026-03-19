"""Topic suppression management routes."""

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse
from starlette.routing import Route

from .. import deps


async def suppressions_page(request: Request) -> HTMLResponse:
    """GET /suppressions — list and manage suppressed topics."""
    topics = deps.lifecycle.get_suppressed_topics()

    template = request.app.state.templates.get_template("suppressions.html")
    content = template.render(
        request=request,
        current_page="suppressions",
        topics=topics,
    )
    return HTMLResponse(content)


async def add_suppression(request: Request) -> HTMLResponse:
    """POST /suppressions/add — suppress a new topic."""
    form = await request.form()
    topic = form.get("topic", "").strip()

    if topic:
        deps.lifecycle.suppress_topic(topic)

    # Return updated list (htmx partial)
    topics = deps.lifecycle.get_suppressed_topics()
    template = request.app.state.templates.get_template("suppressions.html")
    content = template.render(
        request=request,
        current_page="suppressions",
        topics=topics,
    )
    return HTMLResponse(content)


async def remove_suppression(request: Request) -> HTMLResponse:
    """POST /suppressions/remove — unsuppress a topic."""
    form = await request.form()
    topic = form.get("topic", "").strip()

    if topic:
        deps.lifecycle.unsuppress_topic(topic)

    # Return updated list (htmx partial)
    topics = deps.lifecycle.get_suppressed_topics()
    template = request.app.state.templates.get_template("suppressions.html")
    content = template.render(
        request=request,
        current_page="suppressions",
        topics=topics,
    )
    return HTMLResponse(content)


routes = [
    Route("/suppressions", suppressions_page),
    Route("/suppressions/add", add_suppression, methods=["POST"]),
    Route("/suppressions/remove", remove_suppression, methods=["POST"]),
]
