# Quick Start

OmniMem 7 is one program. Pick how you want to run it, point your agent at it, and you're done. If you're coming from 6.x there's a section on bringing your memories with you.

## Choose how to run it

**On the machine you code on**, use the desktop app. It lives in the tray (or the menu bar on a Mac), starts at login if you want it to, and gives you a settings window for everything.

**On a server, a Pi or a cloud box**, run it headless: `omnimem serve` under systemd, or the Docker image. There's no window, so you configure it with environment variables.

Both run exactly the same memory engine.

## Install the desktop app

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](#build-from-source)).

| Platform | Download | Notes |
|---|---|---|
| Windows 10 and 11 | `OmniMem-<version>-x64.msi` or `OmniMem-<version>-arm64.msi` | Installs per user with a Start menu entry. Unsigned for now, so SmartScreen will ask if you're sure |
| macOS | `OmniMem-<version>.dmg` | One universal app for Apple Silicon and Intel. Drag it to Applications; it lives in the menu bar with no Dock icon |
| Linux | The `com.squarecows.OmniMem` Flatpak | x86_64 and aarch64. On GNOME you'll want the AppIndicator extension to see the tray icon (without it, the settings window opens instead) |

Start OmniMem and the icon appears. The first start downloads the embedding model (about 90 MB, once) and the menu says **Starting** until it's ready. After that the menu shows:

- the status line: how many memories it holds and the MCP address
- **Settings…**: the settings window
- **Copy MCP URL**: for pasting into your agent's config
- **Start at login**
- **Quit OmniMem**

Closing the settings window leaves OmniMem running. Launching it a second time just brings the window forward.

Everything lives in one data folder:

| Platform | Data folder |
|---|---|
| Windows | `%APPDATA%\squarecows\OmniMem\data` |
| macOS | `~/Library/Application Support/com.squarecows.OmniMem` |
| Linux | `$XDG_DATA_HOME/omnimem` (usually `~/.local/share/omnimem`) |
| Linux Flatpak | `~/.var/app/com.squarecows.OmniMem/data/omnimem` |

The database (`omnimem.db`), `feeds.yml`, `backups/` and `omnimem.env` (your saved settings) all sit in there.

## Install on a server

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](#build-from-source)).

### Debian, Ubuntu, Fedora and friends

Grab the package for your architecture and install it:

```bash
sudo apt install ./omnimem_<version>_amd64.deb        # or _arm64.deb
sudo dnf install ./omnimem-<version>.x86_64.rpm       # or .aarch64.rpm
```

For anything else there's `omnimem-<version>-linux-<arch>.tar.gz`, which carries the binary, the unit file and an install script.

The packages install a systemd service, `omnimem.service`, that runs `omnimem serve` as its own `omnimem` user. Settings go in `/etc/omnimem/omnimem.env` and data in `/var/lib/omnimem`. There are no GUI libraries involved, so it's happy on a minimal server.

```bash
sudoedit /etc/omnimem/omnimem.env      # see configuration.md; .env.example in the repo is a good start
sudo systemctl enable --now omnimem
curl http://127.0.0.1:8765/healthz     # {"status": "ok"}
```

### Docker

```bash
docker run -d --name omnimem \
  -p 127.0.0.1:8765:8765 \
  -e MCP_HOST=0.0.0.0 \
  -e MCP_AUTH_TOKEN=pick-a-long-random-string \
  -e OMNIMEM_DB=/data/omnimem.db \
  -v omnimem-data:/data \
  richarvey/omnimem
```

Binding to `0.0.0.0` inside the container means OmniMem won't start without a token (or OAuth), which is exactly what you want. The [Docker guide](../guides/docker.md) has a Compose file and the rest of the details.

## Build from source

This is how you run 7.0 today. You'll need Rust 1.94 or newer ([rustup](https://rustup.rs) is easiest) and a C toolchain.

```bash
git clone https://code.squarecows.com/ric/omnimem.git && cd omnimem
git checkout v7.0.x
```

**Headless**, no GUI dependencies at all:

```bash
cargo build --release -p omnimem
./target/release/omnimem serve
```

With no `OMNIMEM_DB` set, `serve` keeps its database at `data/omnimem.db` under whichever folder you ran it from, with `feeds.yml` and `backups/` beside it.

**The desktop app** needs the `desktop` feature. On macOS and Windows that's all:

```bash
cargo build --release -p omnimem --features desktop
./target/release/omnimem            # no command runs the desktop app
```

On Linux you also need the GTK, WebKitGTK (2.40 or newer) and AppIndicator development packages first. On Debian or Ubuntu:

```bash
sudo apt install libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev libxdo-dev
```

The first start downloads the embedding model into your Hugging Face cache (`~/.cache/huggingface/hub` unless `HF_HOME` says otherwise). If you already ran 6.7, it's probably there.

The binary has a few other commands worth knowing:

| Command | What it does |
|---|---|
| `omnimem serve` | Run the MCP server, the RSS scheduler and the enrichment worker |
| `omnimem import <backup.json>` | Bring in a 6.x backup and embed it (`--no-embed` to skip embedding) |
| `omnimem export <file>` | Write every memory to a backup in the same format |
| `omnimem stats` | Records and vectors per namespace |
| `omnimem search "<query>"` | Raw similarity search, handy for checking an import (`--namespace`, `--top-k`, `--project`, `--json`) |
| `omnimem rss` | Run one feed check now (`--dry-run` shows what it would ingest) |
| `omnimem embed "<text>"` | Embed some text and print the start of the vector |

Every command takes `--db <file>` (or `OMNIMEM_DB`), and `OMNIMEM_LOG` sets the log level (`debug`, `info`, `warn`).

## Coming from 6.x

Your memories come with you, and they behave the same. The vectors, the recall scoring and the compiled skill bodies are identical to 6.x, so nothing needs retuning.

1. On 6.x, take a backup: ask your agent to call `dump_to_file()`, or use the Backups page in the old web UI. You get a JSON file.
2. Stop 6.x, or at least make sure it isn't on port 8765.
3. Bring the backup into 7.0, one of two ways:
   - **Desktop app**: open **Settings… → Backups**, upload the file and restore it.
   - **Command line**: `omnimem import omnimem_backup.json` (add `--db` if you aren't using the default location).

Backups don't carry vectors, so every memory is embedded again on the way in. That's roughly 8 ms a memory, so a few thousand take well under a minute.

A few things from 6.x are gone on purpose: Valkey, the Compose stack, the web UI on port 8080, `/metrics`, and the SSE transport. The [configuration page](configuration.md#gone-since-6x) lists the settings that went with them.

## Connect your agent

OmniMem speaks MCP over streamable HTTP at:

```
http://127.0.0.1:8765/mcp
```

(The desktop app's **Copy MCP URL** gives you exactly this.) SSE has gone, so if an old config says `"type": "sse"` or ends in `/sse`, change it.

| Agent | Guide | How it connects |
|-------|-------|-----------|
| claude.ai | [../guides/claude-ai.md](../guides/claude-ai.md) | Streamable HTTP + OAuth 2.1 |
| Open Design | [../guides/open-design.md](../guides/open-design.md) | Streamable HTTP + OAuth 2.1 (public client, PKCE) |
| Claude Code | [../guides/claude-code.md](../guides/claude-code.md) | Streamable HTTP |
| Claude Desktop | [../guides/claude-desktop.md](../guides/claude-desktop.md) | Streamable HTTP |
| GitHub Copilot | [../guides/github-copilot.md](../guides/github-copilot.md) | Streamable HTTP |
| GitLab Duo | [../guides/gitlab-duo.md](../guides/gitlab-duo.md) | Streamable HTTP |
| Cursor | [../guides/cursor.md](../guides/cursor.md) | Streamable HTTP |
| AWS Kiro | [../guides/kiro.md](../guides/kiro.md) | Streamable HTTP |
| OpenCode | [../guides/opencode.md](../guides/opencode.md) | Streamable HTTP |
| OpenAI Codex CLI | [../guides/codex.md](../guides/codex.md) | Streamable HTTP |

**Claude Code** is the quickest to show. Either run:

```bash
claude mcp add --transport http --scope user omnimem http://127.0.0.1:8765/mcp
```

or add it to `~/.claude.json` yourself:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp"
    }
  }
}
```

If you've set `MCP_AUTH_TOKEN`, send it along:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

To stop Claude Code asking permission for every OmniMem call, allow the lot in `~/.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

(If you've already got entries in `allow`, just add `"mcp__omnimem__*"` to them.)

That's it. The server hands its usage guide to every agent that connects, through MCP's `instructions` field, so there's no file to copy into your projects. Your agent will load project context at the start of a session, check the graveyard before suggesting things, and store what it learns as it goes. If your setup ignores MCP instructions, or you want to tweak them, there's a copy in `claude_config/CLAUDE.md`.

## Next steps

- [Configuration](configuration.md): every setting
- [RSS feeds and the knowledge base](rss-knowledge.md): passive knowledge ingestion
- [Using it from multiple machines](remote-access.md): reverse proxies, OAuth 2.1 for claude.ai, the security checklist
- [Features in depth](features.md): the lifecycle, the graveyard, experience scoring and the rest
