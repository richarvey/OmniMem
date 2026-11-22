# Connecting OmniMem to Mistral Vibe

Vibe is Mistral's terminal coding agent (the `vibe` command, from Mistral Vibe Code). It talks to MCP servers over streamable HTTP, so it connects straight to OmniMem's `/mcp` endpoint, and since 3.0 it has hooks, so OmniMem can put its memory in the way of a known dead end, the same as it does in Claude Code.

This is the CLI on your own machine. Mistral's chat (Le Chat) is a different thing, connecting from Mistral's servers: see [the Mistral guide](mistral.md) for that.

## Quick setup

On the desktop, the tray or menu bar menu does both halves for you once Vibe is installed:

- **Add to Mistral Vibe** writes OmniMem's entry into `~/.vibe/config.toml`, with your access token if one is set
- **Add hooks to Mistral Vibe** writes OmniMem's two hooks into `~/.vibe/hooks.toml`

Both keep a copy of the file as it was beside it (`config.toml.omnimem-backup`, `hooks.toml.omnimem-backup`) and leave the rest of the file alone, comments included. Restart Vibe afterwards.

The menu finds Vibe by its `vibe` command (where `uv tool install mistral-vibe` and the install script put it, `~/.local/bin`, or anywhere on your `PATH`) or by the `~/.vibe` folder it makes the first time it runs. If you've moved that folder with `VIBE_HOME`, the menu follows it, as long as `VIBE_HOME` is set where OmniMem starts too.

## Adding OmniMem by hand

In `~/.vibe/config.toml` (or `$VIBE_HOME/config.toml`):

```toml
[[mcp_servers]]
name = "omnimem"
transport = "streamable-http"
url = "http://127.0.0.1:8765/mcp"
```

**Copy MCP URL** in the tray or menu bar menu gives you the address.

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the Configuration page), Vibe sends it as a header:

```toml
[[mcp_servers]]
name = "omnimem"
transport = "streamable-http"
url = "http://127.0.0.1:8765/mcp"

[mcp_servers.auth]
type = "static"
api_key_env = "OMNIMEM_TOKEN"
```

With `api_key_env`, Vibe reads the token from that environment variable (or from `~/.vibe/.env`) and sends `Authorization: Bearer <token>`. The menu writes the token itself instead, as `headers = { Authorization = "Bearer ..." }` in the same table, since an app can't set your shell's environment.

For a remote OmniMem with OAuth switched on, leave the `auth` table out and run `vibe mcp add omnimem --url https://mcp.example.com/mcp`, which signs in through OmniMem's login page in your browser and keeps the token in your keychain.

Vibe names the tools `omnimem_recall`, `omnimem_briefing` and so on. To stop it asking before the read-only ones, give them `permission = "always"`:

```toml
[tools.omnimem_recall]
permission = "always"

[tools.omnimem_briefing]
permission = "always"
```

## Hooks: stopping known dead ends

An agent with OmniMem connected still has to choose to ask it, and mostly doesn't. On OmniMem's dead-end benchmark, agents with the answer sitting in the store made no memory calls at all and walked straight into a failure the project had already recorded. What worked was putting the memory in the way of the action. Vibe's hooks are shell commands, like Claude Code's, so OmniMem's are the same `omnimem hook` command, answering in Vibe's terms:

- **`omnimem-guard`**, a `pre_tool` hook, runs before an `edit`, `write_file`, `bash` or `process_start` call (`process_start` is how Vibe starts a background command). If what the call would put in place matches an approach this project abandoned, it denies the call, and Vibe hands the model the reason: what was tried, why it failed and what worked instead. Reads are never matched, so looking at a dead end to check whether it still holds is never blocked, and a shell command is only checked when it changes something
- **`omnimem-briefing`**, a `session_start` hook, hands a new conversation the project's briefing: where the work stands and what's been tried and abandoned. Vibe adds it to your first message, out of sight, and shows `✓ [omnimem-briefing] Briefed on <project>` in the conversation. It runs for a new session or after `/clear`, not for a resumed one, which already has it

Both fail open: if OmniMem's database can't be read, or anything else goes wrong, the call goes ahead as if the hook weren't there.

### Installing the hooks

From the tray or menu bar menu, choose **Add hooks to Mistral Vibe**. Or from a terminal:

```bash
omnimem install-hooks --vibe            # every project: ~/.vibe/hooks.toml
omnimem install-hooks --vibe --project  # this project only: .vibe/hooks.toml
```

`--dry-run` says what it would do. Put `--db <file>` before `install-hooks` to point the hooks at another database. Vibe only reads a project's `.vibe/hooks.toml` once you've trusted the folder.

What lands in `hooks.toml` looks like this:

```toml
[[hooks]]
name = "omnimem-briefing"
type = "session_start"
match = "re:startup|clear"
command = "/usr/bin/omnimem hook session-start --agent vibe"
timeout = 15
description = "OmniMem: what this project already knows"

[[hooks]]
name = "omnimem-guard"
type = "pre_tool"
match = "re:edit|write_file|bash|process_start"
command = "/usr/bin/omnimem hook pre-tool-use --agent vibe"
timeout = 15
description = "OmniMem: stop an approach this project already abandoned"
```

The hooks run OmniMem by its full path. If you move or reinstall OmniMem somewhere else, install them again: the menu says **Update hooks in Mistral Vibe** when the hooks on disk aren't the ones it would write. OmniMem only ever touches hooks whose names start with `omnimem-`.

### Limits

- **The briefing needs Vibe 3.1.1 or later**, which added `session_start` hooks. An older Vibe reports the briefing hook as a config problem and skips it; the guard still works, from 3.0
- The project is named after the folder Vibe was started in, or `OMNIMEM_PROJECT` when that's set, the same as the Claude Code hooks
- The hooks read OmniMem's database directly, so they need OmniMem on the same machine as Vibe
- Claude Code's third hook, recording what a session did when it ends, isn't there yet: Vibe has a `session_end` hook, but its transcript isn't in the form OmniMem reads
- **On Windows**, Vibe runs hooks in Git Bash when it's installed and PowerShell otherwise. OmniMem writes its path with forward slashes, which both read, but PowerShell can't run a command whose path is in quotes, so if OmniMem's path has a space in it and you don't have Git Bash, put `& ` in front of the `command`
- Not available in the Flatpak, whose sandbox can't reach `~/.vibe`

## Verifying the connection

Start Vibe and type `/mcp`: OmniMem should be listed with its tools. Then ask:

> Call the OmniMem health tool to check the connection

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

To check the hooks, start a new session in a project OmniMem knows: the conversation should show the `omnimem-briefing` line before your first answer.

## Notes

- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should return `{"status": "ok"}`, and the desktop app's tray status line says whether it's running
- Vibe's MCP and hook settings are documented at [docs.mistral.ai/vibe/code/cli/mcp-servers](https://docs.mistral.ai/vibe/code/cli/mcp-servers) and [docs.mistral.ai/vibe/code/cli/hooks](https://docs.mistral.ai/vibe/code/cli/hooks)
