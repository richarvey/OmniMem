//! The settings window's page, until the full settings panel (phase 7)
//! replaces it: whether OmniMem is running and where to point an MCP client.

pub const INDEX: &str = r#"<!doctype html>
<html lang="en-GB">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>OmniMem</title>
<style>
  :root {
    --paper: #f7f5f0; --ink: #1b2233; --muted: #5b6478; --line: #d9d4c7;
    --accent: #ff8e01; --accent-text: #9a5a00; --good: #2f7d4f; --bad: #b3261e;
    color-scheme: light dark;
  }
  @media (prefers-color-scheme: dark) {
    :root { --paper: #141a26; --ink: #eef0f5; --muted: #a3abbd; --line: #2c3547;
            --accent-text: #ff8e01; --good: #6cc38f; --bad: #ff8a80; }
  }
  body { margin: 0; background: var(--paper); color: var(--ink);
         font: 15px/1.5 system-ui, -apple-system, "Segoe UI", Ubuntu, sans-serif; }
  main { max-width: 38rem; margin: 0 auto; padding: 2rem 1.5rem; }
  h1 { font-size: 1.4rem; margin: 0 0 1.5rem; }
  h1 span { color: var(--accent-text); }
  .card { border: 1px solid var(--line); border-radius: 0.6rem; padding: 1rem 1.25rem; margin-bottom: 1rem; }
  .label { color: var(--muted); font-size: 0.85rem; }
  .state { font-weight: 600; }
  .state.running { color: var(--good); }
  .state.failed { color: var(--bad); }
  code { font-family: ui-monospace, "Ubuntu Mono", monospace; word-break: break-all; }
  button { font: inherit; border: 1px solid var(--accent); background: var(--accent); color: #1b2233;
           border-radius: 0.4rem; padding: 0.35rem 0.8rem; cursor: pointer; }
  button:disabled { opacity: 0.5; cursor: default; }
  .row { display: flex; gap: 0.75rem; align-items: center; flex-wrap: wrap; }
  .note { color: var(--muted); font-size: 0.9rem; }
</style>
</head>
<body>
<main>
  <h1>Omni<span>Mem</span></h1>
  <div class="card">
    <div class="label">Status</div>
    <div id="state" class="state">Connecting…</div>
    <div id="detail" class="note"></div>
  </div>
  <div class="card">
    <div class="label">MCP address</div>
    <div class="row">
      <code id="url">not available yet</code>
      <button id="copy" type="button" disabled>Copy</button>
    </div>
    <div id="copied" class="note" hidden>Copied to the clipboard.</div>
  </div>
  <p class="note">The full settings panel is on its way. For now this window shows whether OmniMem
  is running and the address to give your MCP client. <span id="version"></span></p>
</main>
<script>
  const send = (cmd) => window.ipc.postMessage(JSON.stringify({ cmd }));
  const names = { starting: "Starting…", running: "Running", failed: "Stopped with an error", stopped: "Stopped" };
  window.omnimem = {
    receive(status) {
      const state = document.getElementById("state");
      state.textContent = names[status.state] || status.state;
      state.className = "state " + status.state;
      const detail = status.state === "running"
        ? `${status.memories} ${status.memories === 1 ? "memory" : "memories"} stored`
        : status.state === "starting" ? "Loading the embedding model. The first run downloads it."
        : status.error || "";
      document.getElementById("detail").textContent = detail;
      document.getElementById("url").textContent = status.mcp_url || "not available yet";
      document.getElementById("copy").disabled = !status.mcp_url;
      document.getElementById("version").textContent = "Version " + status.version + ".";
      send("ack");
    },
  };
  document.getElementById("copy").addEventListener("click", () => {
    send("copy_mcp_url");
    document.getElementById("copied").hidden = false;
  });
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => send("ready"));
  } else {
    send("ready");
  }
</script>
</body>
</html>
"#;
