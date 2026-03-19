"""Semantic search route using the recall pipeline."""

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps


async def search_form(request: Request) -> HTMLResponse:
    """GET /search — search form page."""
    template = request.app.state.templates.get_template("search.html")
    content = template.render(
        request=request,
        results=None,
        query="",
        namespace="",
        project="",
        top_k=10,
        current_page="search",
    )
    return HTMLResponse(content)


async def search_results(request: Request) -> HTMLResponse:
    """GET /search/results — htmx endpoint returning search results partial."""
    query = request.query_params.get("query", "").strip()
    namespace = request.query_params.get("namespace", "")
    project = request.query_params.get("project", "")
    top_k = min(50, max(1, int(request.query_params.get("top_k", "10"))))

    if not query:
        return HTMLResponse('<p class="empty-state">Enter a search query.</p>')

    namespaces = [namespace] if namespace else None
    project_filter = project or None

    recall_results = deps.pipeline.recall(
        query=query,
        namespaces=namespaces,
        top_k=top_k,
        project_filter=project_filter,
    )

    results = []
    for r in recall_results:
        results.append({
            "key": r.key,
            "namespace": r.namespace,
            "content": r.content[:300],
            "score": round(r.adjusted_score, 4),
            "state": r.state,
            "project": r.project or "",
            "result_type": r.result_type,
            "tags": r.tags,
            "reinstate_candidate": r.reinstate_candidate,
            "effort_score": r.effort_score,
            "outcome": r.outcome,
            "breakthrough": r.breakthrough,
        })

    template = request.app.state.templates.get_template("search_results.html")
    content = template.render(results=results, query=query)
    return HTMLResponse(content)


routes = [
    Route("/search", search_form),
    Route("/search/results", search_results),
]
