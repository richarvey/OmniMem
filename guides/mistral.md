# Connecting OmniMem to Mistral

Mistral's chat (Le Chat, which Mistral's newer docs call Work) can use remote MCP servers as custom connectors, and it signs in to them with OAuth 2.1. So your OmniMem memories can be used in Mistral too, the same store that Claude Code and claude.ai use.

Like claude.ai, Mistral connects from its own servers and from its own web page, not from your laptop. OmniMem needs a public HTTPS address, OAuth switched on, and Mistral's origin allowed.

## Prerequisites

- **OmniMem 7.0.8 or later** reachable from the internet over HTTPS. Earlier 7.0 releases refused Mistral's discovery requests before it ever reached a sign-in page
- **A Cloudflare Tunnel or your own proxy, not Tailscale Funnel.** Mistral can't reach a Funnel (`*.ts.net`) address at all: in testing, not one request arrived over Funnel, while the same server over Cloudflare connected straight away. That's on Mistral's side and nothing in OmniMem can fix it. The [Cloudflare guide](omnimem-setup-linux-cloudflare.md) gets you a suitable address
- **A Mistral account that can add connectors.** Adding connectors, custom ones included, is for administrators. On the Free, Pro and Student plans, the account owner is the administrator

## Step 1: Configure OmniMem

On a server, in `/etc/omnimem/omnimem.env` (or as environment variables for the Docker image):

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.example.com
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
MCP_ALLOWED_ORIGINS=https://chat.mistral.ai
```

On the desktop app, the same settings are on the Configuration page, in the **OAuth** and **MCP server** sections.

| Setting | What it's for |
|----------|-------------|
| `OAUTH_ENABLED` | Turns OmniMem's OAuth server on |
| `OAUTH_BASE_URL` | The https address Mistral reaches OmniMem at. It has to be exactly right: Mistral follows the addresses OmniMem gives back, and a typo there sends it off to a host that doesn't exist |
| `OAUTH_ADMIN_USER`, `OAUTH_ADMIN_PASSWORD` | The account the login page asks for |
| `MCP_ALLOWED_ORIGINS` | Mistral calls `/mcp` from its own web page, so its origin has to be allowed, or `/mcp` answers `403 Forbidden Origin` |

Restart OmniMem to pick it up:

```bash
sudo systemctl restart omnimem
```

Then check it from outside, from any machine:

```bash
curl https://mcp.example.com/.well-known/oauth-authorization-server
curl -i -X POST https://mcp.example.com/mcp
```

The first should give JSON naming `authorization_endpoint`, `token_endpoint` and `registration_endpoint` on your address. The second should be a `401` with a `WWW-Authenticate` header. That `401` matters more than it looks: Mistral sends an unauthenticated request to `/mcp` first and only carries on if it gets `200`, `401` or `403`. Anything else, a `421` say, and it decides the URL isn't an MCP server at all.

## Step 2: Add the connector

1. Open the Connectors page, at [console.mistral.ai/connectors](https://console.mistral.ai/connectors/)
2. Click **+ Add Connector** and switch to the **Custom MCP Connector** tab
3. Fill in:
   - **Connector name**: something like `omnimem`, with no spaces or special characters
   - **Server URL**: your address including the path, `https://mcp.example.com/mcp`
   - **Description**: optional, but "My long-term memory" helps Mistral decide when to use it
4. Click **Connect**

Mistral works out from the server which kind of sign-in it uses. For OmniMem that's OAuth 2.1 with dynamic client registration, so it registers itself as a client and sends you to OmniMem's login page.

## Step 3: Sign in

1. Enter the username and password from step 1
2. Click **Sign in**
3. You're sent back to Mistral with the connector connected

## Step 4: Use it in a chat

Mistral uses a connected connector on its own when it seems relevant. To switch it on by hand in a conversation, click **+** (or type `/`) in the chat box, pick **Tools** and turn on your OmniMem connector. Then just ask, for instance "what do you remember about this project?", and it'll call `recall` or `briefing`.

Mistral asks before it calls a tool. To stop it asking for the read-only ones, open **Connectors → My Connectors**, click the OmniMem card, go to the **Functions** tab and turn on **Always allow** for those:

- **Safe to always allow**: `recall`, `recall_index`, `recall_detail`, `briefing`, `get_project_context`, `list_projects`, `warn_if_abandoned`, `find_skills`, `get_skill`, `explain_memory`, `health`, `version`
- **Leave on approval**: anything that writes or deletes, such as `remember`, `forget`, `archive`, `deprioritise`, `restore_from_file` and `delete_project`

Claude Code is set up to call `briefing` at the start of every session. Don't count on Mistral doing the same unless you ask. A line in your request ("check OmniMem first") goes a long way.

## Mistral and claude.ai on one server

If claude.ai already reaches OmniMem over a Tailscale Funnel name, you don't have to choose. Give Mistral a Cloudflare name as well and list both addresses:

```bash
OAUTH_BASE_URL=https://my-server.tail12345.ts.net,https://mcp.example.com
MCP_ALLOWED_ORIGINS=https://chat.mistral.ai
```

Each request is answered with the address it was made to, so claude.ai only ever sees the Funnel name and Mistral only the Cloudflare one. Both sign in to the same memories. More in [remote access](../docs/remote-access.md#more-than-one-address).

## How it works

It's the standard OAuth 2.1 authorisation code flow with PKCE, the same as claude.ai's, with a couple of extra requests at the start:

1. `GET /.well-known/mcp/server-card/mcp`. OmniMem answers `404`, which is fine; Mistral moves on
2. An unauthenticated `GET` and `POST` to `/mcp`, which answer `401` and point at OmniMem's protected resource document
3. The protected resource and authorisation server metadata
4. `POST /register`, where Mistral registers itself as a client
5. `/authorize` and OmniMem's login page in your browser, then `POST /token` for the access and refresh tokens
6. MCP requests to `/mcp` with the access token

In OmniMem's log the requests show up with the user agent `MistralAI-MCPClient/1.0`, plus an httpx one for some of the OAuth steps. OmniMem logs every OAuth request, so `journalctl -u omnimem -f` while you connect shows how far Mistral got.

Access tokens last an hour and Mistral refreshes them itself. After 30 days (`OAUTH_REFRESH_MAX_DAYS`, at most 90) you sign in again.

## Troubleshooting

The quickest diagnosis is to watch the log while you click **Connect**:

```bash
journalctl -u omnimem -f
```

If nothing at all turns up, the problem is between Mistral and your server (the tunnel, DNS, or a Tailscale Funnel address), and no OmniMem setting will fix it.

| Problem | Fix |
|---------|----------|
| No requests reach OmniMem at all | Mistral can't reach the address. On a `ts.net` Funnel address it never will: use a Cloudflare Tunnel or your own proxy |
| Mistral says the server isn't a valid MCP server | Its first request to `/mcp` got something other than `200`, `401` or `403`. Usually a `421 Misdirected Request`, so `OAUTH_BASE_URL` doesn't match the hostname. Check the URL ends in `/mcp` too |
| It fails while detecting the authentication method | `OAUTH_BASE_URL` is set but wrong, a `.co` for a `.com` say, so the address in the `WWW-Authenticate` header goes nowhere. Compare it with the address you typed into Mistral |
| `403 Forbidden Origin` in the log | Add `https://chat.mistral.ai` to `MCP_ALLOWED_ORIGINS` and restart. The log line says the origin isn't allowed |
| Cloudflare answers with a challenge page instead of JSON | A Cloudflare bot or WAF setting is catching Mistral's servers. See [the Cloudflare guide](omnimem-setup-linux-cloudflare.md#cloudflare-settings-that-get-in-the-way) |
| "Invalid or expired session" on the login page | The sign-in took longer than five minutes. Click **Connect** again |
| "Too many failed attempts" on the login page | The rate limit tripped. Wait out the window (15 minutes by default) |
