# Guides

OmniMem speaks MCP over Streamable HTTP at `/mcp`, so anything that can use a remote MCP server can use it. On a local install the address is `http://127.0.0.1:8765/mcp`, and the desktop app's **Copy MCP URL** menu item puts it on your clipboard.

## Connecting your agent

| Agent | How it connects |
|-------|-----------------|
| [Claude Code](claude-code.md) | Native HTTP, access token as a header |
| [Claude Desktop](claude-desktop.md) | Through the `mcp-remote` bridge |
| [claude.ai](claude-ai.md) | OAuth 2.1, needs a public HTTPS address |
| [Cursor](cursor.md) | Native HTTP, Agent mode |
| [GitHub Copilot](github-copilot.md) | Native HTTP in VS Code, Agent mode |
| [GitLab Duo](gitlab-duo.md) | Native HTTP, no custom headers |
| [Kiro](kiro.md) | Native HTTP, access token as a header |
| [OpenCode](opencode.md) | Remote server, access token or OAuth |
| [OpenAI Codex CLI](codex.md) | Native HTTP, token from an environment variable |
| [Open Design](open-design.md) | OAuth 2.1 as a public client |

## Running OmniMem somewhere

| Where | What it uses |
|-------|--------------|
| [macOS](omnimem-setup-macos.md) | The menu bar app from the DMG |
| [Raspberry Pi](omnimem-setup-raspberry-pi.md) | The arm64 `.deb` and its systemd service |
| [AWS](omnimem-setup-aws-linux.md) | The `.deb` or `.rpm`, with Caddy for HTTPS |
| [Google Cloud](omnimem-setup-gcp-linux.md) | The `.deb`, with Caddy for HTTPS |
| [Linux with Tailscale Funnel](omnimem-setup-linux-tailscale.md) | The `.deb`, Funnel and OAuth, for claude.ai |
| [Docker](docker.md) | The single headless image |

For everything else, start with the [quick start](../docs/quick-start.md).
