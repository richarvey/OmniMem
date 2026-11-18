# Connecting OmniMem to Lumo

Lumo is Proton's privacy-first AI assistant, and its desktop app can use remote MCP servers. That makes it a good match for OmniMem: a private assistant with a memory that lives on your own hardware, the same store your other agents use.

I've connected it over HTTPS with OAuth 2.1, so that's what this guide covers. There's one catch, and it's an easy one to miss: the server URL needs a trailing slash, `/mcp/` rather than `/mcp`.

## Prerequisites

- **The Lumo desktop app.** The MCP settings are in the desktop app
- **OmniMem on an HTTPS address**, with OAuth switched on. If it's on a server at home, [Tailscale Funnel](omnimem-setup-linux-tailscale.md) or a [Cloudflare Tunnel](omnimem-setup-linux-cloudflare.md) gets you one without opening ports. On AWS or Google Cloud, the setup guides add HTTPS with Caddy

## Step 1: Configure OmniMem

On a server, in `/etc/omnimem/omnimem.env` (or as environment variables for the Docker image):

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.example.com
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

On the desktop app, the same settings are on the Configuration page, in the **OAuth** section.

| Setting | What it's for |
|---------|---------------|
| `OAUTH_ENABLED` | Turns OmniMem's OAuth server on |
| `OAUTH_BASE_URL` | The https address Lumo reaches OmniMem at, with no path. It has to be exactly right, since Lumo follows the addresses OmniMem gives back |
| `OAUTH_ADMIN_USER`, `OAUTH_ADMIN_PASSWORD` | The account the login page asks for |

Restart OmniMem to pick it up:

```bash
sudo systemctl restart omnimem
```

Then check it from another machine:

```bash
curl https://mcp.example.com/.well-known/oauth-authorization-server
curl -i -X POST https://mcp.example.com/mcp/
```

The first should give JSON naming `authorization_endpoint`, `token_endpoint` and `registration_endpoint` on your address. The second should be a `401` with a `WWW-Authenticate` header, which is OmniMem asking for a sign-in.

## Step 2: Add OmniMem to Lumo

1. Open the Lumo desktop app and go to its settings, where MCP servers are added
2. Add a new server and give it a name, such as `omnimem`
3. For the URL, enter your address with `/mcp/` on the end, trailing slash included:

   ```
   https://mcp.example.com/mcp/
   ```

4. Save it, and Lumo goes off to sign in

Lumo works out from the server that it uses OAuth 2.1 with dynamic client registration, so there's no client ID or secret to copy anywhere. It registers itself and opens OmniMem's login page.

## Step 3: Sign in

1. Enter the username and password from step 1
2. Click **Sign in**
3. You're sent back to Lumo with OmniMem connected

## Step 4: Use it

Ask Lumo something that needs your memory, "what do you remember about this project?" say, and it'll call `recall` or `briefing`. Claude Code is set up to call `briefing` at the start of every session; Lumo won't unless you ask, so a line like "check OmniMem first" helps.

If Lumo asks before it calls a tool, the read-only ones are safe to allow: `recall`, `recall_index`, `recall_detail`, `briefing`, `get_project_context`, `list_projects`, `warn_if_abandoned`, `find_skills`, `get_skill`, `explain_memory`, `health` and `version`. Keep anything that writes or deletes, such as `remember`, `forget`, `archive` and `delete_project`, on approval.

Access tokens last an hour and Lumo refreshes them itself. After 30 days (`OAUTH_REFRESH_MAX_DAYS`, at most 90) you sign in again.

## Troubleshooting

Watch OmniMem's log while you add the server. It logs every OAuth request, so you can see how far Lumo got:

```bash
journalctl -u omnimem -f
```

| Problem | Fix |
|---------|-----|
| Lumo won't connect, though the URL looks right | Check it ends in `/mcp/`, with the trailing slash. Without it Lumo doesn't connect |
| No requests reach OmniMem at all | Lumo can't reach the address: check the tunnel or proxy, and the `curl` from step 1 |
| The sign-in fails, or Lumo is sent to an address that doesn't exist | `OAUTH_BASE_URL` doesn't match the address you gave Lumo. It's the same host, without `/mcp/` |
| `403 Forbidden Origin` in the log | Add the origin the log line names to `MCP_ALLOWED_ORIGINS` and restart |
| "Invalid or expired session" on the login page | The sign-in took longer than five minutes. Add the server again |
| "Too many failed attempts" on the login page | The rate limit tripped. Wait out the window (15 minutes by default) |
