# The Settings Panel

In 6.x you managed OmniMem from a web dashboard on port 8080, with its own login page. That's gone. Everything it showed now lives in the desktop app's settings window, and there's no HTTP version at all.

That's deliberate. A dashboard on a port is something else on your machine (or your network) can reach, so it needs a login, sessions and a rate limit, and all of that is attack surface for a page only you should ever see. The settings window gets its pages straight from the OmniMem process through a custom `omnimem://` protocol and talks back over IPC. No socket, no session, no login.

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](quick-start.md#build-from-source)).

## Opening it

The desktop app (the Windows MSI, the macOS DMG or the Flatpak) puts an icon in your tray or menu bar while it runs. Its menu has:

- a status line with the memory count and the MCP address
- **Settings…**, which opens the window
- **Copy MCP URL**, for pasting into a client's config
- **Start at login**
- **Quit OmniMem**

Closing the window leaves OmniMem running in the tray. Launching the app a second time brings the window forward instead of starting another server. On GNOME without the AppIndicator extension there's no tray, so the app opens the window instead.

While the engine loads (and on first start, downloads the embedding model) the pages show a starting screen, then fill in.

Headless installs (`omnimem serve`, the `.deb`, `.rpm` and tarball, and the Docker image) have no panel. Configure those with environment variables and manage memories through the MCP tools.

## Pages

| Page | What it does |
|---|---|
| **Dashboard** | Namespace counts and state breakdowns, recent activity, the enrichment queue, and a version check. Counts are cached for `DASHBOARD_STATS_TTL` seconds (60) |
| **Memories** | Browse every memory with namespace, state, project, licence and provenance filters. `licence=unknown` is the queue of records nobody has classified. Rows carry deprioritise and delete actions and a recall-heat bar |
| **Memory detail** | Full content and metadata, experience data and contradictions. Edit tags, set the licence (with an optional note) and the provenance, through the same engine as `retag`, `set_licence` and `set_provenance`, so extracted facts follow and `updated_at` isn't touched |
| **Create** | Store a memory, with the duplicate check shown inline and the namespace defaults for licence and provenance |
| **Search** | Semantic search through the full recall pipeline, weak matches marked |
| **Projects** | List, create, edit and delete projects, with domain chips, a domain filter, a **Suggest** button that drafts domains from the stack and recurring tags, and bulk deprioritise and reinstate |
| **Experience** | Effort stats, outcomes, breakthroughs and the most effortful work, with the graveyard on its own page |
| **Skills** | Compiled skills with their rules, sources and load counts, plus pending proposals. **New Skill** runs the same propose-and-accept gate as `compile_skill`, and refuses a domain that already has a skill (recompiles stay with the MCP tool, which shows the diff). Skills can be deleted but never edited |
| **Duplicates** | Scan a namespace for near-identical clusters and archive the extras |
| **Contradictions** | Each recorded pair once, both sides side by side |
| **Suppressions** | Add and remove suppressed topics |
| **Telemetry** | Most recalled, gone cold (no recall in `TELEMETRY_COLD_DAYS`, 60), never recalled, skills included |
| **Token overhead** | What OmniMem costs an agent's context before any tool is called (the instructions, tool schemas and deferred tool names, measured from what the server sends), plus per-tool call counts, durations and errors with a reset button |
| **RSS feeds** | Edit the reading list: URLs, topics, mode, project label, licence and skill influence scores. Upload or download `feeds.yml` whole. Every change is mirrored for the skill compiler and picked up by the scheduler |
| **Backups** | Create, upload (up to 100 MB), preview, restore, download and delete backups. Restores go through the same path as `restore_from_file`, so every migration runs and memories are re-embedded |
| **Configuration** | Everything a headless install sets as environment variables. See below |

### Skill export and import

**Export** writes a checksummed zip of the skill, its source memories and any RSS feeds that influence its domain. **Import** takes a bundle (up to 20 MB), previews exactly what would be added, and writes only when you confirm. It's strictly additive: nothing already on the receiving side is overwritten, and a feed that's already in the reading list at most gains the influence entry it was missing.

### Downloads

A webview can't be relied on to save a download (WebKitGTK does it silently, macOS won't tell you), so skill exports, backups and `feeds.yml` are written straight into your Downloads folder, never over an existing file, and the page tells you where it went.

## Configuration

The Configuration page covers the settings a headless install would put in the environment, grouped as MCP server, OAuth, Claude, recall, RSS, skills, embeddings and the panel itself. Each shows its default, and a note when a real environment variable is overriding it. The [configuration reference](configuration.md) lists them all.

- **Ordinary settings** are validated together and written to `omnimem.env` in the data folder. Nothing is written if any value is refused, and hand-written lines the page doesn't know about are kept.
- **Secrets** (`MCP_AUTH_TOKEN`, `OAUTH_ADMIN_PASSWORD`, `ANTHROPIC_API_KEY` and `HF_TOKEN`) go to the OS keychain: Keychain on macOS, Credential Manager on Windows, the Secret Service on Linux. Once saved they're never shown again; you can replace or clear them.
- **Changes apply at the next start.** The app reads the file and the keychain before anything else runs.
- **A real environment variable always wins** over both.

The data folder is `%APPDATA%\squarecows\OmniMem\data` on Windows, `~/Library/Application Support/com.squarecows.OmniMem` on macOS and `$XDG_DATA_HOME/omnimem` on Linux. The database, `omnimem.env`, `feeds.yml` and `backups/` all live there.

## What happened to /metrics

It went with the web UI. The numbers it exposed (memory counts, recall counters, cold and never-recalled memories, tool calls and errors) are on the Dashboard, Telemetry and Token overhead pages instead.
