"""OAuth login routes — registered via FastMCP custom_route."""

from __future__ import annotations

import html
import logging
from typing import TYPE_CHECKING
from urllib.parse import urlencode

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse, Response

if TYPE_CHECKING:
    from fastmcp import FastMCP
    from oauth.provider import OmniMemOAuthProvider

logger = logging.getLogger("omnimem.oauth")

LOGIN_PAGE = """\
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>OmniMem — Sign in</title>
  <style>
    *, *::before, *::after {{ box-sizing: border-box; }}
    body {{
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
      background: #0f172a; color: #e2e8f0;
      display: flex; align-items: center; justify-content: center;
      min-height: 100vh; margin: 0;
    }}
    .card {{
      background: #1e293b; border-radius: 12px; padding: 2rem;
      width: 100%; max-width: 380px; box-shadow: 0 4px 24px rgba(0,0,0,.4);
    }}
    h1 {{ font-size: 1.4rem; margin: 0 0 .25rem; text-align: center; }}
    .sub {{ text-align: center; color: #94a3b8; font-size: .85rem; margin-bottom: 1.5rem; }}
    label {{ display: block; font-size: .85rem; margin-bottom: .25rem; color: #94a3b8; }}
    input {{
      width: 100%; padding: .6rem .75rem; border: 1px solid #334155;
      border-radius: 6px; background: #0f172a; color: #e2e8f0;
      font-size: .95rem; margin-bottom: 1rem;
    }}
    input:focus {{ outline: none; border-color: #6366f1; }}
    button {{
      width: 100%; padding: .65rem; border: none; border-radius: 6px;
      background: #6366f1; color: #fff; font-size: .95rem; font-weight: 600;
      cursor: pointer;
    }}
    button:hover {{ background: #4f46e5; }}
    .error {{
      background: #7f1d1d; color: #fca5a5; padding: .5rem .75rem;
      border-radius: 6px; font-size: .85rem; margin-bottom: 1rem;
      text-align: center;
    }}
  </style>
</head>
<body>
  <div class="card">
    <h1>OmniMem</h1>
    <p class="sub">Sign in to authorise access</p>
    {error_block}
    <form method="post" action="/oauth/login">
      <input type="hidden" name="session" value="{session}">
      <label for="username">Username</label>
      <input id="username" name="username" type="text" required autofocus>
      <label for="password">Password</label>
      <input id="password" name="password" type="password" required>
      <button type="submit">Sign in</button>
    </form>
  </div>
</body>
</html>
"""


def _render_login(session: str, error: str | None = None) -> str:
    error_block = ""
    if error:
        error_block = f'<div class="error">{html.escape(error)}</div>'
    return LOGIN_PAGE.format(
        session=html.escape(session), error_block=error_block
    )


def register_oauth_routes(
    mcp: FastMCP, provider: OmniMemOAuthProvider
) -> None:
    """Register the /oauth/login GET and POST routes on the FastMCP instance."""

    @mcp.custom_route("/oauth/login", methods=["GET", "POST"])
    async def oauth_login(request: Request) -> Response:
        if request.method == "GET":
            session = request.query_params.get("session", "")
            if not provider.get_pending(session):
                return HTMLResponse(
                    _render_login("", error="Invalid or expired session."),
                    status_code=400,
                )
            return HTMLResponse(_render_login(session))

        # POST — validate credentials
        form = await request.form()
        session = str(form.get("session", ""))
        username = str(form.get("username", ""))
        password = str(form.get("password", ""))

        if not provider.get_pending(session):
            return HTMLResponse(
                _render_login("", error="Session expired. Please try again."),
                status_code=400,
            )

        if not provider.verify_credentials(username, password):
            logger.warning("Failed login attempt for user '%s'", username)
            return HTMLResponse(
                _render_login(session, error="Invalid username or password."),
                status_code=401,
            )

        code, redirect_uri, state = provider.complete_authorize(session)
        logger.info("Authorised session %s…, redirecting to client", session[:8])

        params: dict[str, str] = {"code": code}
        if state:
            params["state"] = state
        separator = "&" if "?" in redirect_uri else "?"
        target = f"{redirect_uri}{separator}{urlencode(params)}"
        return RedirectResponse(target, status_code=302)
