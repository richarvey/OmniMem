"""Capture 6.x OAuth HTTP behaviour (FastMCP 4 + mcp 2) as golden fixtures.

Run from mcp_server/ so `oauth` imports. Everything random is masked.
"""

import base64
import hashlib
import json
import re
import sys

from starlette.testclient import TestClient

from fastmcp import FastMCP
from fastmcp.server.auth import MultiAuth
from oauth.provider import OmniMemOAuthProvider
from oauth.routes import register_oauth_routes

BASE = "https://mcp.example.com"

provider = OmniMemOAuthProvider(base_url=BASE, admin_user="admin", admin_password="secret123")
mcp = FastMCP("omnimem", instructions="x", auth=provider)
register_oauth_routes(mcp, provider)
app = mcp.http_app(path="/mcp", host_origin_protection=False)

from importlib.metadata import version

out = {
    "routes": sorted({getattr(r, "path", "") for r in app.routes}),
    "versions": {"fastmcp": version("fastmcp"), "mcp": version("mcp")},
}
# Entered so the lifespan starts the session manager an authenticated /mcp call needs.
c = TestClient(app, base_url=BASE, follow_redirects=False).__enter__()


def capture(resp, keep=("content-type", "cache-control", "pragma", "www-authenticate",
                        "access-control-allow-origin", "access-control-allow-methods",
                        "access-control-allow-headers", "access-control-max-age", "location")):
    return {
        "status": resp.status_code,
        "headers": {k: resp.headers[k] for k in keep if k in resp.headers},
        "body": resp.text,
    }


def mask(s):
    s = re.sub(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "<uuid>", s)
    s = re.sub(r'"client_secret":"[0-9a-f]{64}"', '"client_secret":"<hex64>"', s)
    s = re.sub(r'"client_id_issued_at":\d+', '"client_id_issued_at":<now>', s)
    return s


out["as_metadata"] = capture(c.get("/.well-known/oauth-authorization-server"))
out["pr_metadata"] = capture(c.get("/.well-known/oauth-protected-resource/mcp"))
out["pr_metadata_root"] = capture(c.get("/.well-known/oauth-protected-resource"))
out["openid"] = capture(c.get("/.well-known/openid-configuration"))
out["mcp_no_auth"] = capture(c.post("/mcp", json={}))
out["mcp_bad_token"] = capture(c.post("/mcp", json={}, headers={"authorization": "Bearer nope"}))
out["mcp_basic"] = capture(c.post("/mcp", json={}, headers={"authorization": "Basic abc"}))
out["preflight_token"] = capture(c.options("/token", headers={
    "origin": "https://claude.ai", "access-control-request-method": "POST",
    "access-control-request-headers": "content-type"}))
out["metadata_with_origin"] = capture(c.get("/.well-known/oauth-authorization-server",
                                            headers={"origin": "https://claude.ai"}))

reg = c.post("/register", json={"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"],
                                "client_name": "claudeai", "extra_field": 1})
out["register"] = capture(reg)
out["register"]["body"] = mask(out["register"]["body"])
client = reg.json()
pub = c.post("/register", json={"redirect_uris": ["http://localhost:3000/cb"],
                                "token_endpoint_auth_method": "none"})
out["register_public"] = capture(pub)
out["register_public"]["body"] = mask(out["register_public"]["body"])
out["register_bad_scope"] = capture(c.post("/register", json={"redirect_uris": ["https://a.example/cb"], "scope": "admin"}))
out["register_no_redirects"] = capture(c.post("/register", json={"client_name": "x"}))
out["register_pkjwt"] = capture(c.post("/register", json={"redirect_uris": ["https://a.example/cb"], "token_endpoint_auth_method": "private_key_jwt"}))

verifier = "a" * 50
challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).decode().rstrip("=")
q = {"client_id": client["client_id"], "redirect_uri": "https://claude.ai/api/mcp/auth_callback",
     "response_type": "code", "code_challenge": challenge, "code_challenge_method": "S256",
     "state": "st", "scope": "omnimem", "resource": BASE + "/mcp"}
a = c.get("/authorize", params=q)
out["authorize"] = capture(a)
out["authorize"]["headers"]["location"] = re.sub(r"session=[\w-]+", "session=<id>", a.headers["location"])
out["authorize_unknown_client"] = capture(c.get("/authorize", params={**q, "client_id": "nope"}))
out["authorize_bad_response_type"] = capture(c.get("/authorize", params={**q, "response_type": "token"}))
out["authorize_bad_redirect"] = capture(c.get("/authorize", params={**q, "redirect_uri": "https://evil.example/cb"}))
out["authorize_bad_scope"] = capture(c.get("/authorize", params={**q, "scope": "admin"}))
out["authorize_no_challenge"] = capture(c.get("/authorize", params={k: v for k, v in q.items() if k != "code_challenge"}))

session = a.headers["location"].split("session=")[1]
login_get = c.get(f"/oauth/login?session={session}")
out["login_get_status"] = login_get.status_code
login = c.post("/oauth/login", data={"session": session, "username": "admin", "password": "secret123"})
out["login"] = capture(login)
code = re.search(r"code=([\w-]+)", login.headers["location"]).group(1)
out["login"]["headers"]["location"] = login.headers["location"].replace(code, "<code>")

sec = client["client_secret"]
tok_base = {"grant_type": "authorization_code", "code": code, "client_id": client["client_id"],
            "client_secret": sec, "redirect_uri": "https://claude.ai/api/mcp/auth_callback"}
out["token_bad_verifier"] = capture(c.post("/token", data={**tok_base, "code_verifier": "b" * 50}))
out["token_bad_secret"] = capture(c.post("/token", data={**tok_base, "client_secret": "x", "code_verifier": verifier}))
out["token_missing_client"] = capture(c.post("/token", data={"grant_type": "authorization_code"}))
out["token_bad_grant"] = capture(c.post("/token", data={**tok_base, "grant_type": "password", "code_verifier": verifier}))
t = c.post("/token", data={**tok_base, "code_verifier": verifier})
out["token"] = capture(t)
tokens = t.json()
out["token"]["body"] = out["token"]["body"].replace(tokens["access_token"], "<access>").replace(tokens["refresh_token"], "<refresh>")
out["token_code_reuse"] = capture(c.post("/token", data={**tok_base, "code_verifier": verifier}))

r = c.post("/token", data={"grant_type": "refresh_token", "refresh_token": tokens["refresh_token"],
                           "client_id": client["client_id"], "client_secret": sec})
out["refresh_status"] = r.status_code
r2 = c.post("/token", data={"grant_type": "refresh_token", "refresh_token": tokens["refresh_token"],
                            "client_id": client["client_id"], "client_secret": sec})
out["refresh_replay_same_pair"] = r.json() == r2.json()
out["refresh_bad_scope"] = capture(c.post("/token", data={"grant_type": "refresh_token", "refresh_token": r.json()["refresh_token"],
                                                         "client_id": client["client_id"], "client_secret": sec, "scope": "admin"}))
out["mcp_with_token_status"] = c.post("/mcp", json={"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}},
    headers={"authorization": "Bearer " + r.json()["access_token"], "accept": "application/json, text/event-stream"}).status_code
out["revoke"] = capture(c.post("/revoke", data={"token": r.json()["access_token"], "client_id": client["client_id"], "client_secret": sec}))
out["revoke_public_no_secret"] = capture(c.post("/revoke", data={"token": "x", "client_id": pub.json()["client_id"]}))
out["icon"] = capture(c.get("/favicon.ico"), keep=("content-type", "cache-control"))
out["login_page_bad_session"] = capture(c.get("/oauth/login?session=bogus"), keep=("content-type",))

json.dump(out, sys.stdout, indent=2, sort_keys=True)
