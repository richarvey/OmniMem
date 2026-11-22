# Connecting OmniMem to OpenAI Codex CLI

Codex CLI is OpenAI's open source coding agent for the terminal. It's configured in TOML and speaks Streamable HTTP natively, which is exactly what OmniMem 7 serves, so it's a two-line job.

## Quick setup

Create or edit `~/.codex/config.toml`:

```toml
[mcp_servers.omnimem]
url = "http://127.0.0.1:8765/mcp"
```

On the desktop app, **Copy MCP URL** in the tray or menu bar menu puts that address on your clipboard.

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the Configuration page):

```toml
[mcp_servers.omnimem]
url = "http://127.0.0.1:8765/mcp"
bearer_token_env_var = "OMNIMEM_TOKEN"
```

Then set the token in your shell: `export OMNIMEM_TOKEN=your-token-here`

Note that `bearer_token_env_var` takes the **name** of the variable, not the token itself. I've tripped over that one. If you'd rather keep the token in the file, `http_headers = { Authorization = "Bearer your-token-here" }` does the same.

On the desktop, the tray or menu bar menu can do this for you: **Add to Codex** appears once Codex is installed (its `codex` command, its `~/.codex` folder, or on a Mac the Codex app), writes the entry above into `~/.codex/config.toml` with your access token as an `http_headers` line if one is set, and keeps a copy of the file as it was beside it, as `config.toml.omnimem-backup`. It switches an `omnimem` entry that points somewhere else (a 6.x supergateway bridge, say) to this server, and leaves the rest of the file alone, comments included. Restart Codex afterwards.

## Config file locations

| File | Scope | Use when |
|------|-------|----------|
| `~/.codex/config.toml` | All projects | OmniMem should be available everywhere (recommended) |
| `.codex/config.toml` (project root) | Single project | Only this project needs OmniMem (requires project trust) |

## Hooks: stopping known dead ends

An agent with OmniMem connected still has to choose to ask it, and mostly doesn't. On OmniMem's dead-end benchmark, agents with the answer sitting in the store made no memory calls at all and walked straight into a failure the project had already recorded. What worked was putting the memory in the way of the action. Codex's hooks speak the same protocol as Claude Code's, so OmniMem's are exactly the [Claude Code hooks](../docs/claude-code-hook.md), `omnimem hook`:

- **The guard**, on `PreToolUse`, runs before Codex's shell (`Bash`) and its file edits (`apply_patch`). If what the call would put in place matches an approach this project abandoned, it denies the call, and Codex hands the model the reason: what was tried, why it failed and what worked instead. Reads are never matched, a shell command is only checked when it changes something, and the lines a patch takes out aren't read, so removing a dead end is never blocked
- **The briefing**, on `SessionStart`, hands a new conversation the project's briefing: where the work stands and what's been tried and abandoned. It runs at startup, after `/clear` and after a compaction, not for a resumed session, which already has it

Both fail open: if OmniMem's database can't be read, or anything else goes wrong, the call goes ahead.

### Installing the hooks

From the tray or menu bar menu, choose **Add hooks to Codex**. Or from a terminal:

```bash
omnimem install-hooks --codex            # every project: ~/.codex/hooks.json
omnimem install-hooks --codex --project  # this project only: .codex/hooks.json
```

`--dry-run` says what it would do. Put `--db <file>` before `install-hooks` to point the hooks at another database.

**Then trust them.** Codex doesn't run a hook nobody has reviewed: the next time it starts it lists the new hooks and asks, and `/hooks` shows them any time. OmniMem deliberately doesn't do that step for you. The hooks are skipped until you've said yes, and again if their command ever changes (reinstalling OmniMem somewhere else, say), so after an **Update hooks in Codex** you'll be asked once more.

What lands in `hooks.json` looks like this:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "^(startup|clear|compact)$",
        "hooks": [{ "type": "command", "command": "/usr/bin/omnimem hook session-start", "timeout": 15 }]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "^(Bash|apply_patch)$",
        "hooks": [{ "type": "command", "command": "/usr/bin/omnimem hook", "timeout": 15 }]
      }
    ]
  }
}
```

The hooks run OmniMem by its full path. If you move or reinstall it, install them again: the menu says **Update hooks in Codex** when the hooks on disk aren't the ones it would write. Any other hooks in the file are kept; an `omnimem hook` command you added by hand counts as OmniMem's and is replaced.

If you keep your hooks inline in `config.toml` (`[[hooks.PreToolUse]]`) instead, Codex reads both and warns that one folder has two formats. Either move yours into `hooks.json` or copy the two entries above into your `config.toml` and skip the installer.

### Limits

- The project is named after the folder Codex was started in, or `OMNIMEM_PROJECT` when that's set, the same as the Claude Code hooks
- The hooks read OmniMem's database directly, so they need OmniMem on the same machine as Codex
- Claude Code's third hook, recording what a session did when it ends, isn't installed: Codex gives `SessionEnd` at most three seconds, which isn't long enough to load the embedding model, and its transcript isn't in the form OmniMem reads
- Codex's hosted tools (web search) and MCP tools aren't checked, only what it runs and edits locally
- Not available in the Flatpak, whose sandbox can't reach `~/.codex`

## Known quirks

- **The section name matters**: it must be `[mcp_servers]` with an underscore. `[mcp-servers]` or `[mcpservers]` and Codex quietly ignores the whole block.
- **Silent config failures**: a TOML syntax error isn't reported, Codex just carries on without the server. If the tools vanish, check your TOML first.
- **Coming from 6.x**: the old supergateway bridge to `/sse` isn't needed any more. Point Codex straight at `/mcp`.

## Verifying the connection

Start a Codex session and ask:

> Call the OmniMem health tool to check the connection

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Notes

- Codex moves quickly, so check the [Codex CLI repository](https://github.com/openai/codex) for the latest MCP options
- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should come back with `{"status": "ok"}`. On the desktop app the tray icon's status line tells you too
