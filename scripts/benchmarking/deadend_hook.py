#!/usr/bin/env python3
"""PreToolUse hook: catch a known dead end before it executes.

This is the mechanism the dead-end benchmark exists to measure. An agent that
proposes an approach the project already tried and abandoned should be stopped
at the point of proposal, not after it has written the code, run the suite and
watched it go red.

The hook fires on the tools that *do* something (Edit, Write, Bash) and never
on the tools that merely look (Read, Glob, Grep). Reading is how an agent
checks a claim, and blocking it would make stale memory unfalsifiable.

Contract with Claude Code: a PreToolUse hook reads one JSON object on stdin and
writes one on stdout. ``permissionDecision: "deny"`` blocks the call and hands
``permissionDecisionReason`` back to the model as the explanation. The reason
string does all the work here, so it carries both halves of the memory: what
was abandoned and why, and what worked instead. A denial that says only "not
that" leaves the agent to go and rediscover the answer, which is the cost the
graveyard exists to avoid.

Fails open, always. A hook that errors, times out or cannot reach the server
must not block the agent: a benchmark arm that stalls because memory was
briefly unavailable would measure the harness, not the product.
"""

from __future__ import annotations

import json
import os
import sys

# The tools that change something. Read/Glob/Grep are deliberately absent.
GUARDED = {"Edit", "MultiEdit", "Write", "NotebookEdit", "Bash"}

# Where the proposal text lives in each tool's input.
# Only what the call would put in place. old_string is deliberately absent:
# removing kestrel-rs from a file is the opposite of proposing it.
FIELDS = {
    "Edit": ("new_string",),
    "MultiEdit": ("edits",),
    "Write": ("content",),
    "NotebookEdit": ("new_source",),
    "Bash": ("command",),
}

# A Bash command is only a proposal when it changes something. `ls
# vendor/kestrel-rs` is investigation and must never be blocked; `cargo add
# kestrel-rs` is the proposal. Anything not recognised as mutating passes.
MUTATING_BASH = ("cargo add", "cargo install", "sed -i", "tee ", ">", "mv ", "cp ",
                 "patch", "git apply", "perl -i", "python", "echo ")


def bash_mutates(command: str) -> bool:
    return any(marker in command for marker in MUTATING_BASH)


def allow() -> None:
    """Emit nothing and exit clean, which lets the call proceed."""
    sys.exit(0)


def proposal_text(tool: str, tool_input: dict) -> str:
    """Flatten whatever this tool is about to do into one searchable string."""
    parts: list[str] = []
    if tool == "Bash" and not bash_mutates(str(tool_input.get("command", ""))):
        return ""
    for key in FIELDS.get(tool, ()):
        value = tool_input.get(key)
        if isinstance(value, str):
            parts.append(value)
        elif isinstance(value, list):  # MultiEdit
            for edit in value:
                if isinstance(edit, dict):
                    parts.append(str(edit.get("new_string", "")))
    return "\n".join(p for p in parts if p)[:4000]


def warning_line(client, name: str) -> str | None:
    """The full warning for one abandoned approach, including the way out.

    ``warn_if_abandoned`` returns the reason and the effort score but not the
    breakthrough, so on its own it can say "not that" without saying "this
    instead". ``recall`` on the approach name returns the assembled warning,
    which carries both. If that row is missing we fall back to the fields we
    do have rather than staying silent.
    """
    try:
        rows = client.call_ok("recall", {"query": name, "top_k": 4}).json()
        rows = rows if isinstance(rows, list) else rows.get("results", [])
        for row in rows:
            if row.get("result_type") == "abandoned_warning":
                text = str(row.get("content") or "").strip()
                if text:
                    return text
    except Exception:
        pass
    return None


def main() -> None:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        allow()

    tool = payload.get("tool_name", "")
    if tool not in GUARDED:
        allow()

    text = proposal_text(tool, payload.get("tool_input") or {})
    if not text.strip():
        allow()

    endpoint = os.environ.get("OMNIMEM_BENCH_URL", "http://127.0.0.1:8766")
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    try:
        from mcpclient import McpClient

        with McpClient(endpoint) as client:
            result = client.call_ok("warn_if_abandoned", {"query": text}).json()
            if not isinstance(result, dict) or result.get("status") != "warning":
                allow()
            matches = result.get("matches") or []
            if not matches:
                allow()

            lines: list[str] = []
            for match in matches[:3]:
                name = str(match.get("abandoned_name") or "").strip()
                if not name:
                    continue
                detail = warning_line(client, name)
                if not detail:
                    reason = str(match.get("reason") or "").strip().rstrip(".")
                    detail = f"Abandoned approach: {name}" + (f" - {reason}" if reason else "")
                lines.append(detail)
    except SystemExit:
        raise
    except Exception:
        allow()

    if not lines:
        allow()

    body = "\n".join(f"- {line}" for line in lines)
    reason = (
        "Stopped by project memory: this approach was already tried on this "
        "project and abandoned.\n\n"
        f"{body}\n\n"
        "This is recorded experience from earlier work on this codebase, not a "
        "guess. Do not spend turns re-establishing that it fails. Take the "
        "approach that worked instead, and only revisit the abandoned one if "
        "you have a specific reason to believe the situation has changed."
    )
    json.dump(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        },
        sys.stdout,
    )
    sys.exit(0)


if __name__ == "__main__":
    main()
