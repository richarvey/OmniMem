# The Settings Panel

OmniMem's settings panel is a set of web pages at `/admin` on the same address as the MCP server: `http://127.0.0.1:8765/admin` on a desktop, `https://mcp.example.com/admin` behind a proxy. The desktop app opens it in your browser, and a server or a Docker container serves it the same way, so there's one panel everywhere instead of a settings panel per operating system.

From 7.0 to 7.0.9 the panel lived only in the desktop app's window, served over a private `omnimem://` scheme so no socket could reach it. That kept it safe, but a server had no panel at all. 7.1 puts it on HTTP and pays for that properly: a sign-in, server-side sessions, CSRF checks and HTTPS. `ADMIN_PANEL=false` turns it off for an install that should only ever speak MCP, which is how OmniMem Cloud runs.

## Opening it

**On a desktop**, the app (the Windows MSI, the macOS DMG or the Flatpak) puts an icon in your tray or menu bar while it runs. Its menu has:

- a status line with the memory count
- **Settings…**, which opens the panel in your browser, already signed in
- **Copy MCP URL**, for pasting into a client's config
- **Add to Claude Desktop** and **Add to Claude Code**, which check the client's settings and add OmniMem, or switch an `omnimem` entry pointing elsewhere to this server, but never add it twice
- **Start at login**
- **Quit OmniMem**

Settings… signs you in with a link that works once, within a minute, so there's no password to set up on your own machine. Launching the app a second time opens the panel instead of starting another server. On GNOME without the AppIndicator extension there's no tray, so the app opens the panel as soon as it starts, with a note at the top saying what to install.

**On a server**, sign in one of two ways:

- **With the admin password.** Set `OAUTH_ADMIN_USER` and `OAUTH_ADMIN_PASSWORD`, the same pair the OAuth login page uses (OAuth itself doesn't have to be on), and `/admin` asks for them.
- **With a sign-in link.** Run `omnimem admin-link` on the server and open the link it prints. It works once, within five minutes. In Docker that's `docker exec <container> omnimem admin-link`.

While the engine loads (and on first start, downloads the embedding model) the pages show a starting screen, then fill in.

## Who gets in

The panel is where a person accepts a skill an agent proposed, settles a contradiction and confirms what OmniMem believes, so it has to stay a person's:

- **Only a panel session opens it.** The MCP bearer token (`MCP_AUTH_TOKEN`) and OAuth access tokens are never read on `/admin`, so an agent connected over MCP can't reach the pages it would need to approve its own work.
- **Sessions live on the server**, as hashes in the store, for `ADMIN_SESSION_HOURS` (12). **Sign out**, in the footer, ends a session everywhere, and a restart keeps you signed in. The cookie is `HttpOnly`, `SameSite=Strict`, scoped to `/admin`, and `Secure` behind HTTPS.
- **Changes must come from the panel's own pages.** A form post or an htmx request has to carry `Sec-Fetch-Site: same-origin` (or, from an older browser, an `Origin` matching the server). With the strict cookie, no other site can make your browser change anything.
- **HTTPS, or this machine.** The panel only opens to a browser on the same machine or through a proxy that says it used HTTPS (`X-Forwarded-Proto: https`). Anything else would send the password and the session in the clear. If the network is private already (WireGuard, Tailscale, a container port published to localhost only), `ADMIN_ALLOW_HTTP=true` lifts the rule.
- **Failed logins are limited** per address with the OAuth login's settings, `OAUTH_LOGIN_MAX_ATTEMPTS` (10) in `OAUTH_LOGIN_WINDOW_SECONDS` (900). The panel keeps its own count.
- **The pages run no inline script** and every response carries the same Content Security Policy, plus `X-Frame-Options: DENY` and `Referrer-Policy: same-origin`, so a sign-in link can't leak to another site in a referrer.

## Pages

| Page | What it does |
|---|---|
| **Dashboard** | Namespace counts and state breakdowns, recent activity, the enrichment queue, and a version check (a marker in the sidebar, from `UPDATE_CHECK`). Counts are cached for `DASHBOARD_STATS_TTL` seconds (60) |
| **Memories** | Browse every memory with namespace, state, project, licence and provenance filters. `licence=unknown` is the queue of records nobody has classified. Rows carry deprioritise and delete actions and a recall-heat bar |
| **Memory detail** | Full content and metadata, experience data and contradictions. Edit tags, set the licence (with an optional note) and the provenance, through the same engine as `retag`, `set_licence` and `set_provenance`, so extracted facts follow and `updated_at` isn't touched |
| **Create** | Store a memory, with the duplicate check shown inline and the namespace defaults for licence and provenance |
| **Search** | Semantic search through the full recall pipeline, weak matches marked. **True on** and **Known on** ask what was true, or what OmniMem believed, on a day ([bi-temporal memory](bitemporal.md)). A memory's own page shows when it was true and lets you change it |
| **Projects** | List, create, edit and delete projects, with domain chips, a domain filter, a **Suggest** button that drafts domains from the stack and recurring tags, and bulk deprioritise and reinstate |
| **Experience** | Effort stats, outcomes, breakthroughs and the most effortful work, with the graveyard on its own page |
| **Skills** | Compiled skills with their rules, sources and load counts, plus **Pending proposals**: every stashed draft or diff, including ones an agent proposed over MCP, each with Accept and Discard (a count shows beside the Skills link while any wait; `HUMAN_APPROVAL` is on the Configuration page). **New Skill** runs the same propose-and-accept gate as `compile_skill`, and refuses a domain that already has a skill (recompiles stay with the MCP tool, which shows the diff). Skills can be deleted but never edited |
| **Duplicates** | Scan a namespace for near-identical clusters and archive the extras |
| **Contradictions** | Each pair still in dispute once, both sides side by side, with **A is current**, **B is current**, **Both stand** and **Not a contradiction** (an optional reason, and a box to deprioritise the out-of-date memory). A click is your approval and records the resolution on both memories. Above them, **Pending** lists resolutions an agent proposed that couldn't be put to you through MCP, each with Accept and Discard. See [settling a contradiction](features.md#settling-a-contradiction) |
| **Suppressions** | Add and remove suppressed topics |
| **Telemetry** | Most recalled, gone cold (no recall in `TELEMETRY_COLD_DAYS`, 60), never recalled, skills included |
| **Token overhead** | What OmniMem costs an agent's context before any tool is called (the instructions, tool schemas and deferred tool names, measured from what the server sends), plus per-tool call counts, durations and errors with a reset button |
| **RSS feeds** | Edit the reading list: URLs, topics, mode, project label, licence and skill influence scores. Upload or download `feeds.yml` whole. Every change is mirrored for the skill compiler and picked up by the scheduler |
| **Audit Log** | The log of changes (see [the audit log](audit-log.md)), newest first, filtered by key, actor, action and date. Each memory's page has a History section with its own rows |
| **Backups** | Create, upload (up to 100 MB), preview, restore, download and delete backups. Tick **Include RSS feeds** to put the reading list in a new backup, and **Include audit log** to put the audit log in; restoring a backup that has an audit log adds the rows you don't have, and restoring a backup that has feeds adds the ones you don't already list (matched by URL) and leaves the rest alone. Restores go through the same path as `restore_from_file`, so every migration runs and memories are re-embedded |
| **Configuration** | Everything a headless install sets as environment variables. See below |

### Skill export and import

**Export** writes a checksummed zip of the skill, its source memories and any RSS feeds that influence its domain. **Import** takes a bundle (up to 20 MB), previews exactly what would be added, and writes only when you confirm. It's strictly additive: nothing already on the receiving side is overwritten, and a feed that's already in the reading list at most gains the influence entry it was missing.

### Downloads

Skill exports, backups and `feeds.yml` download through the browser like any other file, so they land wherever your browser puts downloads, on whichever machine you're browsing from.

## Configuration

The Configuration page edits the settings file, grouped as MCP server, OAuth, the settings panel, Claude, recall, RSS, skills, embeddings and the panel's pages. Each shows its default, and a note when a real environment variable is overriding it. The [configuration reference](configuration.md) lists them all.

- **Ordinary settings** are validated together and written to the settings file: `omnimem.env` in the data folder on a desktop, or whichever file the server loaded (`OMNIMEM_ENV_FILE`). Nothing is written if any value is refused, and hand-written lines the page doesn't know about are kept.
- **Secrets** (`MCP_AUTH_TOKEN`, `OAUTH_ADMIN_PASSWORD`, `ANTHROPIC_API_KEY` and `HF_TOKEN`) go to the OS keychain on a desktop: Keychain on macOS, Credential Manager on Windows, the Secret Service on Linux. A server has no keychain, so they go into the settings file beside everything else, which keeps its permissions (and is readable by its owner alone if the page creates it). Either way they're never shown again once saved; you can replace or clear them.
- **Changes apply at the next start.** OmniMem reads the file (and the keychain) before anything else runs.
- **A real environment variable always wins** over both.
- **The Linux packages let the page save `/etc/omnimem/omnimem.env`.** The folder belongs to root and the `omnimem` group (`0770`) and the service's unit lists it in `ReadWritePaths`, since a save writes a new copy beside the file and renames it into place. After a save the file belongs to the `omnimem` user, still `0640`, so only root and the `omnimem` group can read it. The Docker image keeps the folder root's, because a save inside a container is gone when it's recreated: there the page says it can't save, and you set the environment or mount your own file. Where a file is read-only to OmniMem for any other reason, the page says so and you edit it as an administrator. A server with no settings file at all (configured purely by environment variables) shows nothing to edit.

The data folder is `%APPDATA%\squarecows\OmniMem\data` on Windows, `~/Library/Application Support/com.squarecows.OmniMem` on macOS and `$XDG_DATA_HOME/omnimem` on Linux. The database, `omnimem.env`, `feeds.yml` and `backups/` all live there.

## What happened to /metrics

It went with the web UI. The numbers it exposed (memory counts, recall counters, cold and never-recalled memories, tool calls and errors) are on the Dashboard, Telemetry and Token overhead pages instead.
