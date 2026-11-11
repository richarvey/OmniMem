// OmniMem for OpenCode, written by OmniMem (`omnimem install-hooks --opencode`
// or the tray's "Add hooks to OpenCode"). Writing it again replaces this file.
//
// It does two of the jobs OmniMem's Claude Code hooks do, by running the same
// `omnimem hook` command with the same JSON:
//
//   - stops a tool call that repeats something this project already tried and
//     abandoned, and tells the model what worked instead
//   - opens each session with what OmniMem already knows about the project
//
// It fails open: if OmniMem can't be run or says nothing, the work carries on.
//
// One file for OpenCode 1.x and 2.x: 2.x reads the default export's `id` and
// `setup()`, 1.x (from 1.18.29) calls its `server()`, and each ignores the other.
import { spawn } from "node:child_process";

// The omnimem program and its arguments before `hook`.
const OMNIMEM = __OMNIMEM_COMMAND__;
const TIMEOUT_MS = 15000;
// The tools that put something in place, which are all the guard checks
// (OPENCODE_GUARDED in omnimem's hook.rs). Reading is never stopped, so a
// read doesn't start omnimem at all.
const GUARDED = new Set(["bash", "shell", "edit", "multiedit", "write", "apply_patch", "patch"]);

/** Run `omnimem hook <args>` with `payload` on stdin, and parse what it prints. */
function hook(args, payload) {
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(OMNIMEM[0], [...OMNIMEM.slice(1), "hook", ...args], {
        stdio: ["pipe", "pipe", "ignore"],
        windowsHide: true,
      });
    } catch {
      resolve(null);
      return;
    }
    let out = "";
    const timer = setTimeout(() => child.kill(), TIMEOUT_MS);
    child.stdout.on("data", (chunk) => {
      out += chunk;
    });
    child.on("error", () => {
      clearTimeout(timer);
      resolve(null);
    });
    child.on("close", () => {
      clearTimeout(timer);
      try {
        resolve(out.trim() ? JSON.parse(out) : null);
      } catch {
        resolve(null);
      }
    });
    child.stdin.on("error", () => {});
    child.stdin.end(JSON.stringify(payload));
  });
}

/** The two hooks, for a project in `directory`, whichever API calls them. */
function omnimem(directory) {
  // One briefing per session: the system prompt is rebuilt for every request.
  const briefings = new Map();
  return {
    /** Throws, with what worked instead, when the call repeats a dead end. */
    async guard(tool, args, sessionID) {
      if (!GUARDED.has(tool)) return;
      const reply = await hook([], {
        hook_event_name: "PreToolUse",
        tool_name: tool,
        tool_input: args ?? {},
        cwd: directory,
        session_id: sessionID,
      });
      const decision = reply?.hookSpecificOutput;
      if (decision?.permissionDecision === "deny") {
        throw new Error(decision.permissionDecisionReason);
      }
    },
    /** What OmniMem knows about the project, or "" when there's nothing. */
    briefing(sessionID) {
      const id = sessionID ?? "";
      if (!briefings.has(id)) {
        briefings.set(
          id,
          hook(["session-start"], {
            hook_event_name: "SessionStart",
            cwd: directory,
            session_id: id,
          }).then((reply) => reply?.hookSpecificOutput?.additionalContext ?? ""),
        );
      }
      return briefings.get(id);
    },
  };
}

// The only export: OpenCode refuses a plugin file with any other.
export default {
  id: "omnimem",
  // OpenCode 2.x.
  async setup(ctx) {
    const memory = omnimem(ctx.location?.directory);
    await ctx.tool.hook("execute.before", (event) =>
      memory.guard(event.tool, event.input, event.sessionID),
    );
    await ctx.session.hook("context", async (event) => {
      const text = await memory.briefing(event.sessionID);
      if (text) event.system.push({ type: "text", text });
    });
  },
  // OpenCode 1.x.
  async server({ directory }) {
    const memory = omnimem(directory);
    return {
      "tool.execute.before": (input, output) =>
        memory.guard(input.tool, output.args, input.sessionID),
      "experimental.chat.system.transform": async (input, output) => {
        const text = await memory.briefing(input.sessionID);
        if (text) output.system.push(text);
      },
    };
  },
};
