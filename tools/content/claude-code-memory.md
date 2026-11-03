---
path: /claude-code-memory/
title: Persistent memory for Claude Code, beyond CLAUDE.md | OmniMem
description: What CLAUDE.md and auto memory keep between Claude Code sessions, what they miss, and how OmniMem adds searchable memory across projects and machines.
crumb: Claude Code memory
eyebrow: Claude Code memory
published: 2026-11-03
updated: 2026-11-03
---
# Persistent memory for Claude Code, beyond CLAUDE.md

Claude Code starts every session with a fresh context window, so nothing carries across sessions unless something puts it there. It has two ways of doing that: `CLAUDE.md` files that you write, and auto memory, where it writes notes for itself. Both are useful, and both stop short of what a long project needs. This page covers what each one does, where it runs out, and how to give Claude Code memory it can search, across every project and machine you use, with OmniMem.

## What Claude Code remembers on its own

**[CLAUDE.md](https://code.claude.com/docs/en/memory#claude-md-files)** is a markdown file Claude Code reads at the start of every session. There's one for you in `~/.claude/CLAUDE.md` that applies everywhere, and one per project in the repository, usually checked into git. It's the right place for rules: how to run the tests, which branch to work on, how you like commits written. You write it and you keep it up to date.

**[Auto memory](https://code.claude.com/docs/en/memory#auto-memory)** is Claude Code writing its own notes. It saves markdown files in `~/.claude/projects/<project>/memory/`, with a `MEMORY.md` index that gets loaded into the next session. Only the first 200 lines or 25KB of that index are loaded, whichever comes first, so it works best as a short list of pointers. Anthropic's own [memory documentation](https://code.claude.com/docs/en/memory) covers both.

Both are plain files on one machine (Anthropic's docs say of auto memory that "files are not shared across machines"), read whole into the context window. That's their strength (you can open them and see exactly what Claude knows) and their limit.

## Where that runs out

- **Nothing in CLAUDE.md or auto memory is searched.** A file is either loaded or it isn't. Once a project has hundreds of things worth knowing, they can't all be in the opening context, and there's no way to pull up the one that matters for the task in hand.
- **Failures aren't kept.** The most expensive thing to forget is what didn't work: the library that crashed on Alpine, the redirect WebKitGTK ignores. Nothing records that an approach was tried, how much effort it cost, and why it was dropped, so the agent suggests it again next month.
- **One machine, one agent.** Notes on your laptop aren't on your desktop or your server, and Cursor or Codex can't read Claude Code's files.
- **Nothing tidies up.** Old notes don't fade, contradictions sit side by side, and duplicates pile up until someone edits the file by hand.

## What OmniMem adds

OmniMem is an open-source MCP memory server that runs on your own hardware, as a desktop app or one binary on a server. Claude Code talks to it over MCP, so the memory sits outside any one session, project or machine.

- **Semantic recall.** Memories are embedded locally (all-MiniLM-L6-v2, no GPU, no API calls) and searched by meaning, so "markdown editor preferences" finds the note that never used those words. Decisions, project context, preferences and ingested articles are searched together and ranked into one answer.
- **A graveyard of dead ends.** When an approach is abandoned, OmniMem records what it was, why it failed and how much effort it took. Propose it again and recall leads with the warning.
- **Hooks that don't rely on the agent asking.** On [a benchmark](https://code.squarecows.com/ric/omnimem/src/branch/main/docs/claude-code-hook.md#why-these-exist) with the answer sitting in the store, Claude Code made no memory calls at all in twenty runs: it simply didn't think to ask. So OmniMem also answers Claude Code hooks. It briefs each session with what the project already knows, refuses a tool call that repeats a recorded dead end, and records what the session did when it ends. The details are in the [hooks documentation](https://code.squarecows.com/ric/omnimem/src/branch/main/docs/claude-code-hook.md).
- **One memory for every agent and machine.** Run it on a server and every machine points at the same store. Claude Desktop, claude.ai, Cursor, Codex, Copilot and anything else that speaks MCP share it too.
- **It keeps itself in order.** Memories move from active to deprioritised to archived, near-duplicates are caught on write, and contradictions are flagged so you can say which version is true.

## Set it up in two minutes

1. **Install OmniMem.** Download the app for [macOS, Windows or Linux](/#install), or run the server [in Docker](/guides/docker/). It serves MCP at `http://127.0.0.1:8765/mcp`.
2. **Connect Claude Code.** From the app's tray or menu bar icon, pick **Add to Claude Code**. Or run it yourself:

   ```bash
   claude mcp add --transport http --scope user omnimem http://127.0.0.1:8765/mcp
   ```

3. **Install the hooks**, so every session starts briefed and known dead ends are stopped before they run. This adds them to `~/.claude/settings.json` and keeps a backup of the old file:

   ```bash
   omnimem install-hooks
   ```

   With the Mac app, the `omnimem` binary lives inside the app bundle rather than on your PATH, so run `/Applications/OmniMem.app/Contents/MacOS/omnimem install-hooks` instead. The [hooks documentation](https://code.squarecows.com/ric/omnimem/src/branch/main/docs/claude-code-hook.md) covers adding the entries by hand.

The [Claude Code guide](/guides/claude-code/) covers access tokens, project-level config and troubleshooting, and the other [setup guides](/guides/) cover servers, a Raspberry Pi and the cloud.

## Keep CLAUDE.md as well

OmniMem doesn't replace `CLAUDE.md`, and you shouldn't delete yours. A short file of standing rules, loaded every time, is exactly what it's good at. Let `CLAUDE.md` say how the project works, and let OmniMem hold what you've learnt while working on it: the decisions, the bugs, the dead ends and the context that's too big to load whole.

OmniMem is MIT licensed and free. The source is on [Forgejo](https://code.squarecows.com/ric/omnimem), with a [mirror on GitHub](https://github.com/richarvey/OmniMem).
