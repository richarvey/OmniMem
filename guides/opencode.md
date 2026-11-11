# Connecting OmniMem to OpenCode

OpenCode is an open source terminal coding agent. It supports remote MCP servers over Streamable HTTP, so it connects straight to OmniMem's `/mcp` endpoint.

## Quick setup

Create or edit `~/.config/opencode/opencode.json` for global access:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "omnimem": {
      "type": "remote",
      "url": "http://127.0.0.1:8765/mcp",
      "oauth": false,
      "timeout": 15000
    }
  }
}
```

OpenCode uses `"mcp"` as the top-level key (not `"mcpServers"`) and `"type": "remote"` for anything on a network.

On the desktop app, **Copy MCP URL** in the tray or menu bar menu gives you the address.

`"oauth": false` stops OpenCode trying to negotiate OAuth when it sees a 401. That's what you want with a plain access token. If you've switched on OmniMem's OAuth for a remote server and want OpenCode to sign in through the login page, leave it out and OpenCode will run the flow itself.

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the Configuration page), add it as a header:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "omnimem": {
      "type": "remote",
      "url": "http://127.0.0.1:8765/mcp",
      "headers": {
        "Authorization": "Bearer {env:OMNIMEM_TOKEN}"
      },
      "oauth": false,
      "timeout": 15000
    }
  }
}
```

Set `OMNIMEM_TOKEN` in your shell. OpenCode uses `{env:VAR_NAME}`, single braces and no dollar.

Coming from 6.x, change `/sse` to `/mcp`.

On the desktop, the tray or menu bar menu can do this for you: **Add to OpenCode** appears once OpenCode is installed, writes the entry above (with your access token, if one is set) into OpenCode's config file and keeps a copy of the file as it was beside it, as `opencode.json.omnimem-backup`. Restart OpenCode afterwards.

## Config file locations

OpenCode merges config from all of these, later ones winning on conflicts:

| Priority | Location | Use when |
|----------|----------|----------|
| Lowest | `~/.config/opencode/opencode.json` | OmniMem should be available everywhere (recommended) |
| Higher | `opencode.json` in the project root | Only this project needs OmniMem |
| Highest | `OPENCODE_CONFIG_CONTENT` environment variable | Inline JSON for CI or testing |

## Using OmniMem with OpenCode

Once it's configured the tools are available to the model automatically:

- "Call the OmniMem briefing tool for this project"
- "Remember this decision in OmniMem"
- "Check whether we've tried this approach before"

## Hooks: stopping known dead ends

An agent with OmniMem connected still has to choose to ask it, and mostly doesn't. On OmniMem's dead-end benchmark, agents with the answer sitting in the store made no memory calls at all and walked into a failure the project had already recorded. What worked was putting the memory in the way of the action. In Claude Code that's [hooks](../docs/claude-code-hook.md); in OpenCode it's a plugin, which runs the same `omnimem hook` command:

- **Before a tool call that changes something** (`shell`, `edit`, `write` and `patch`, or in OpenCode 1.x `bash` and `apply_patch`), it checks what the call would put in place against what this project tried and abandoned. A match stops the call, and the model is told what was tried, why it was dropped and what worked instead. Reads are never stopped, and neither is a shell command that only looks
- **At the start of each session**, it adds what OmniMem knows about the project to the system prompt: where the work stands, what was abandoned, and any contradictions waiting to be settled. It's fetched once per session

If OmniMem can't be run, or has nothing to say, the plugin stays out of the way and the work carries on.

The same file works with OpenCode 2.x and with 1.x from 1.18.29, which load plugins through different APIs.

### Installing the plugin

From the tray or menu bar menu, choose **Add hooks to OpenCode**. Or from a terminal:

```bash
omnimem install-hooks --opencode            # every project: ~/.config/opencode/plugins/omnimem.js
omnimem install-hooks --opencode --project  # this project only: .opencode/plugins/omnimem.js
```

`--dry-run` says what it would do. Put `--db <file>` before `install-hooks` to point the plugin at another database. Restart OpenCode to load it.

The plugin runs OmniMem by its full path, since OpenCode's desktop app doesn't get your shell's `PATH`. If you move or reinstall OmniMem somewhere else, install the plugin again: the menu says **Update hooks in OpenCode** when the plugin on disk isn't the one it would write. A file called `omnimem.js` that OmniMem didn't write is never overwritten.

### Limits

- The project is named after the folder OpenCode was started in, or `OMNIMEM_PROJECT` when that's set, the same as the Claude Code hooks
- The hooks read OmniMem's database directly, so they need OmniMem on the same machine as OpenCode
- OpenCode doesn't run plugin hooks for tool calls made by a subagent started with the `task` tool ([anomalyco/opencode#5894](https://github.com/anomalyco/opencode/issues/5894)), so those calls aren't checked
- Claude Code's third hook, recording what a session did when it ends, isn't in the plugin yet: OpenCode has no event for a session ending
- Not available in the Flatpak, whose sandbox can't reach OpenCode's config folder

## Verifying the connection

Start an OpenCode session and ask:

> Call the OmniMem health tool to check the connection

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Known quirks

- **Connected but no tools**: there's been a bug where OpenCode shows a green "connected" status but registers nothing. Restart OpenCode and check the terminal output.
- **OAuth auto-negotiation**: OpenCode watches for 401 responses and starts OAuth. With a plain access token, keep `"oauth": false`.
- **The 5 second default timeout**: the first call after OmniMem starts can take longer while the embedding model loads. `"timeout": 15000` gives it room.
- **Context cost**: every tool description goes into the model's context on each request, and OmniMem has 49 tools. Set `"enabled": false` to switch OmniMem off for a session if you need the tokens back.

## Notes

- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should return `{"status": "ok"}`, and the desktop app's tray status line says whether it's running
- OAuth tokens OpenCode obtains are stored in `~/.local/share/opencode/mcp-auth.json`
- Check the [OpenCode documentation](https://opencode.ai/docs/mcp-servers/) for the latest MCP options
