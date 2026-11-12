# Connecting OmniMem to Kiro

Kiro is AWS's AI IDE, built on VS Code, and it plans before it codes. It supports Streamable HTTP MCP servers, and its spec-driven workflow pairs rather nicely with OmniMem's project context.

## Quick setup

Create or edit `~/.kiro/settings/mcp.json` for global access:

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

On the desktop app, **Copy MCP URL** in the tray or menu bar menu gives you the address.

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the Configuration page), add it as a header:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp",
      "headers": {
        "Authorization": "Bearer ${OMNIMEM_TOKEN}"
      }
    }
  }
}
```

Set `OMNIMEM_TOKEN` in your shell environment (`.zshrc`, `.bashrc`) so Kiro can expand it. The IDE uses `${VAR_NAME}` syntax.

Coming from 6.x, change `"type": "sse"` to `"type": "http"` and `/sse` to `/mcp`.

On the desktop, the tray or menu bar menu can do this for you: **Add to Kiro** appears once Kiro is installed (its `~/.kiro` folder, its CLI, or on a Mac the Kiro app), writes the entry above into `~/.kiro/settings/mcp.json`, with your access token in the `Authorization` header if one is set (and the file then readable by you alone), and keeps a copy of the file as it was beside it, as `mcp.json.omnimem-backup`. It switches an `omnimem` entry that points somewhere else (a 6.x server, say) to this one and leaves every other server alone. Once Kiro reaches this OmniMem the item drops out of the menu. Kiro picks up changes to the file as you save it, so there's nothing to restart.

## Global vs project config

| File | Scope | Use when |
|------|-------|----------|
| `~/.kiro/settings/mcp.json` | All projects | OmniMem should be available everywhere (recommended) |
| `.kiro/settings/mcp.json` (project root) | Single project | Only this project needs OmniMem |

## Kiro's MCP panel

You can also add the server through Kiro's UI:

1. Open the **MCP Servers** panel from the sidebar or command palette
2. Click **Add Server**
3. Choose **HTTP** as the transport
4. Enter the URL: `http://127.0.0.1:8765/mcp`
5. Add the authorisation header if you've set a token

## Using OmniMem with Kiro

Once connected, the tools are available to Kiro's agent:

- "Call the OmniMem briefing tool for this project"
- "Remember this architectural decision in OmniMem"
- "Check whether we've tried this approach before"

The agent can load project state at the start of each session and store the decisions as your specs evolve, so the reasoning behind a design doesn't evaporate when you close the window.

## Verifying the connection

In Kiro's chat panel, ask:

> Can you call the OmniMem health tool?

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Hooks: stopping known dead ends

An agent with OmniMem connected still has to choose to ask it, and mostly doesn't. On OmniMem's dead-end benchmark, agents with the answer sitting in the store made no memory calls at all and walked straight into a failure the project had already recorded. What worked was putting the memory in the way of the action, which is what Kiro's hooks are for. OmniMem's are two command hooks running `omnimem hook --agent kiro`:

- **The guard**, on `PreToolUse`, sees each tool call before it runs. If a shell command or a file write would put in place an approach this project abandoned, it exits with code 2, which blocks the call, and Kiro hands the model the reason: what was tried, why it failed and what worked instead. Reads are never matched, a shell command is only checked when it changes something, and the text a replacement takes out isn't read, so removing a dead end is never blocked
- **The briefing**, on `SessionStart`, prints the project's briefing, which Kiro adds to the new session's context: where the work stands and what's been tried and abandoned

Both fail open: if OmniMem's database can't be read, or anything else goes wrong, the call goes ahead.

### Installing the hooks

From the tray or menu bar menu, choose **Add hooks to Kiro**. Or from a terminal:

```bash
omnimem install-hooks --kiro            # Kiro's CLI, every project: ~/.kiro/hooks/omnimem.json
omnimem install-hooks --kiro --project  # this project, IDE and CLI: .kiro/hooks/omnimem.json
```

Kiro's CLI reads hooks from `~/.kiro/hooks`, but the IDE reads them from the workspace's `.kiro/hooks`, so for the IDE run the `--project` form in each project you want guarded. `--dry-run` says what it would do. Put `--db <file>` before `install-hooks` to point the hooks at another database.

The hooks are a file of their own, `omnimem.json`, so your other hooks are never touched:

```json
{
  "version": "v1",
  "hooks": [
    {
      "name": "omnimem-briefing",
      "trigger": "SessionStart",
      "action": { "type": "command", "command": "/usr/bin/omnimem hook session-start --agent kiro" },
      "timeout": 15,
      "enabled": true
    },
    {
      "name": "omnimem-guard",
      "trigger": "PreToolUse",
      "action": { "type": "command", "command": "/usr/bin/omnimem hook pre-tool-use --agent kiro" },
      "timeout": 15,
      "enabled": true
    }
  ]
}
```

(the installed file also carries a `description` for each). The guard has no matcher, so Kiro asks it about every tool call. That's on purpose: Kiro's IDE and CLI name their tools differently (`execute_bash` and `fs_write` in the CLI, `executeBash`, `fsWrite` and `strReplace` in the IDE), and OmniMem lets anything it doesn't guard straight through without opening its database, so a read costs no more than starting the program.

The hooks run OmniMem by its full path. If you move or reinstall it, install them again: the menu says **Update hooks in Kiro** when the file on disk isn't the one it would write.

### Limits

- The project is named after the folder Kiro is working in, or `OMNIMEM_PROJECT` when that's set, the same as the Claude Code hooks
- The hooks read OmniMem's database directly, so they need OmniMem on the same machine as Kiro
- Claude Code's third hook, recording what a session did when it ends, isn't installed: Kiro's transcript isn't in the form OmniMem reads
- MCP tools and Kiro's web tools aren't checked, only what it runs and writes locally
- Not available in the Flatpak, whose sandbox can't reach `~/.kiro`

## Known quirks

- **Silent failures**: if OmniMem can't be reached, Kiro can fail to load every MCP server without saying anything. If your tools suddenly disappear, check OmniMem is running first.
- **CLI vs IDE variable syntax**: the IDE uses `${VAR_NAME}` but the Kiro CLI expects `${env:VAR_NAME}`, so one `mcp.json` can't serve both without editing.
- **OAuth redirects**: Kiro hasn't been able to complete localhost OAuth redirects in the versions I've tried. Stick with an access token in `headers`, which works fine. OAuth is for clients like claude.ai that connect from elsewhere.

## Notes

- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should return `{"status": "ok"}`, and the desktop app's tray status line says whether it's running
- Check the [Kiro documentation](https://kiro.dev/docs/mcp/configuration/) for the latest MCP options
