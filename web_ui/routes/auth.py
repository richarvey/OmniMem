"""Login and logout routes for the web UI session auth."""

import logging

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse, Response
from starlette.routing import Route

from .. import auth, deps

logger = logging.getLogger("omnimem.web.auth")


def _render_login(
    request: Request, next_path: str, error: str | None = None, status_code: int = 200
) -> HTMLResponse:
    template = request.app.state.templates.get_template("login.html")
    content = template.render(request=request, next=next_path, error=error)
    return HTMLResponse(content, status_code=status_code)


async def login_page(request: Request) -> Response:
    """GET /login — sign-in form, or straight through if already signed in."""
    if not auth.login_enabled():
        return RedirectResponse("/", status_code=303)

    token = request.cookies.get(auth.SESSION_COOKIE, "")
    if deps.store and auth.session_user(deps.store.client, token):
        return RedirectResponse("/", status_code=303)

    next_path = auth.safe_next(request.query_params.get("next", ""))
    return _render_login(request, next_path)


async def login_submit(request: Request) -> Response:
    """POST /login — verify credentials, mint a session, redirect to next."""
    if not auth.login_enabled():
        return RedirectResponse("/", status_code=303)

    form = await request.form()
    next_path = auth.safe_next(str(form.get("next", "")))
    client_ip = request.client.host if request.client else "unknown"

    if auth.login_limiter.is_blocked(client_ip):
        logger.warning("Web UI login rate limit hit for %s", client_ip)
        return _render_login(
            request,
            next_path,
            error="Too many failed attempts. Please wait and try again.",
            status_code=429,
        )

    username = str(form.get("username", ""))
    password = str(form.get("password", ""))

    if not auth.verify_credentials(username, password):
        auth.login_limiter.record_failure(client_ip)
        logger.warning(
            "Failed web UI login attempt for user '%s' from %s", username, client_ip
        )
        return _render_login(
            request, next_path, error="Invalid username or password.", status_code=401
        )

    auth.login_limiter.reset(client_ip)
    token = auth.create_session(deps.store.client, username)
    logger.info("Web UI login for '%s' from %s", username, client_ip)

    response = RedirectResponse(next_path, status_code=303)
    response.set_cookie(
        auth.SESSION_COOKIE,
        token,
        max_age=auth.session_ttl_seconds(),
        httponly=True,
        samesite="lax",
        secure=request.url.scheme == "https",
        path="/",
    )
    return response


async def logout(request: Request) -> Response:
    """POST /logout — revoke the session server-side and clear the cookie."""
    token = request.cookies.get(auth.SESSION_COOKIE, "")
    if deps.store:
        auth.destroy_session(deps.store.client, token)

    target = auth.LOGIN_PATH if auth.login_enabled() else "/"
    response = RedirectResponse(target, status_code=303)
    response.delete_cookie(auth.SESSION_COOKIE, path="/")
    return response


routes = [
    Route("/login", login_page, methods=["GET"]),
    Route("/login", login_submit, methods=["POST"]),
    Route("/logout", logout, methods=["POST"]),
]
