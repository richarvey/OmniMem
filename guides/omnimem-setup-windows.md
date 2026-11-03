# Setting Up OmniMem on Windows

On Windows, OmniMem is a tray app. You run the installer, open it from the Start menu, and it sits down by the clock running your memory server. No Docker Desktop, no WSL, no terminal, and no administrator rights either: it installs for you alone.

One thing to know up front: **the installer isn't signed yet.** Code-signing certificates for Windows cost real money every year, and OmniMem doesn't have one at the moment, so Windows will warn you before it runs it. The steps below show you how to get past that, and how to check you've got the genuine file first. The Mac app is signed and notarised; the Windows one will be once there's a certificate.

---

## What you need

- **Windows 10 or 11 on an x64 PC.** There's no Arm build of the installer yet
- **The Microsoft Visual C++ Redistributable (x64).** Most PCs already have it, because so many apps install it. If yours doesn't, the installer says so and stops, and Step 2 covers it
- Somewhere around a few hundred MB of free RAM for the embedding model
- An Anthropic API key if you want the Claude-powered extras (optional)

---

## Step 1: Download the installer

Download `OmniMem-<version>-x64.msi` from the [latest release](https://code.squarecows.com/ric/omnimem/releases/latest), or use the **Windows** button on [omnimem.org](https://omnimem.org/#install).

Your browser may object first. Edge in particular says the file "isn't commonly downloaded". That's because it's unsigned and new, not because anything's wrong with it. In Edge, hover over the download, click the **…** menu, then **Keep**, then **Show more** and **Keep anyway**.

### Check it's the real thing

Because the installer isn't signed, Windows can't tell you who made it. You can check for yourself. Every file on the release has a `.sha256` file beside it holding its checksum. Download `OmniMem-<version>-x64.msi.sha256` too, then in PowerShell, in your Downloads folder:

```powershell
Get-FileHash .\OmniMem-<version>-x64.msi -Algorithm SHA256
Get-Content .\OmniMem-<version>-x64.msi.sha256
```

The long hex string from the first command should match the one at the start of the second. If it doesn't, don't run the installer: download it again, and if it still doesn't match, [open an issue](https://code.squarecows.com/ric/omnimem/issues).

---

## Step 2: Run the installer

Double-click the `.msi`. This is where the unsigned bit shows up properly: Windows SmartScreen puts up a blue box saying **"Windows protected your PC"**, with only a **Don't run** button.

1. Click **More info**. The publisher shows as **Unknown publisher**, which is what unsigned means
2. Click **Run anyway**

The installer then runs without asking for an administrator, because it only installs for your account:

- The app goes into `%LOCALAPPDATA%\Programs\OmniMem`, alongside `DirectML.dll`, which ONNX Runtime needs for the embeddings (Windows' own copy can be too old)
- **OmniMem** appears in the Start menu

**If it stops with a message about the Visual C++ Redistributable**, OmniMem's embedding engine needs Microsoft's C++ runtime and your PC hasn't got it. Download it from Microsoft at [aka.ms/vs/17/release/vc_redist.x64.exe](https://aka.ms/vs/17/release/vc_redist.x64.exe), run that (this one does want an administrator, and it is signed), then run the OmniMem installer again.

---

## Step 3: First start

Open **OmniMem** from the Start menu. There's no window to manage: it lives in the tray as `<o>`, green brackets around a ring with a green dot in it.

Windows tucks new tray icons away, so if you can't see it, click the **^** arrow by the clock. To keep it in view, drag it out onto the taskbar, or on Windows 11 go to **Settings → Personalization → Taskbar → Other system tray icons** and switch OmniMem on.

The first time it runs, OmniMem downloads the embedding model (all-MiniLM-L6-v2) and loads it. The tray menu says it's starting while that happens. After that it reads the model from the cache and starts in a second or so.

Click the tray icon and you'll see:

- **A status line**: whether it's running and how many memories it holds
- **Settings…**: opens the settings window
- **Copy MCP URL**: puts `http://127.0.0.1:8765/mcp` on your clipboard for your client config
- **Add to Claude Desktop** and **Add to Claude Code**: does the setup for you. Each one checks the client's settings first, so OmniMem is never added twice: once it's there the item reads **✓ In Claude Desktop**, and if the client already has an `omnimem` server pointing somewhere else (your old 6.x server, say) it offers to switch it here instead. Restart the client afterwards so it connects
- **Start at login**: tick it and OmniMem comes up whenever you sign in
- **Quit OmniMem**

Tick **Start at login**. You want your memory there before you start work, not after you've remembered to open it.

Only one copy runs at a time. Open OmniMem again while it's running and it just brings the settings window forward.

---

## Step 4: Settings

**Settings…** opens a window with everything the old 6.x web dashboard had: memories, projects, experience and the graveyard, skills, feeds, telemetry and backups. It isn't a website though. The pages come from the app itself (shown through Microsoft's WebView2, which comes with Windows 11 and up-to-date Windows 10), so there's no port to expose, no login page and nothing else on your network can reach them. See [the settings panel](../docs/settings-panel.md).

The **Configuration** page is where the settings live. The ones you'll most likely touch:

- **Anthropic API key**: switches on fact extraction, query expansion, contradiction checks and RSS summaries. Without it OmniMem still works, it just summarises RSS by truncation and skips the Claude extras
- **Access token**: only needed if something other than this PC will connect
- **Port**: 8765 unless something else wants it

Secrets (the API key, the access token, the OAuth password) go into Windows Credential Manager and aren't shown again once saved. Everything else is written to `omnimem.env` in the data folder. Changes apply when you restart OmniMem, and the page tells you so.

Your data lives in `%APPDATA%\squarecows\OmniMem\data`: the database, `feeds.yml` and the `backups` folder. Paste that into File Explorer's address bar to go straight there.

---

## Step 5: Connect Claude Code

Pick **Add to Claude Code** from the tray menu and you're done. Or by hand, with **Copy MCP URL**:

```powershell
claude mcp add --transport http omnimem http://127.0.0.1:8765/mcp --scope user
```

Allow the OmniMem tools without a prompt each time, in `%USERPROFILE%\.claude\settings.json`:

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

The installer doesn't put `omnimem.exe` on your PATH, so to install the [Claude Code hooks](../docs/claude-code-hook.md), which brief every session and stop known dead ends, run it from where it's installed:

```powershell
& "$env:LOCALAPPDATA\Programs\OmniMem\omnimem.exe" install-hooks
```

The [connection guides](README.md) cover Claude Desktop, Cursor, Copilot and the rest.

---

## Step 6: RSS feeds (optional)

Open **Settings…** and go to **RSS feeds**. Add a feed with its URL, name, topics and licence class, and OmniMem picks it up straight away. It also checks every six hours by default (**Check feeds every (hours)** on the Configuration page).

Prefer a file? The reading list is `feeds.yml` in the data folder, and the feeds page can upload and download it.

---

## Backups

**Settings… → Backups** creates a backup, restores one (re-embedding everything as it goes), and saves a copy into your Downloads folder. Your agent can do the same with the `dump_to_file` tool. Backups are JSON files in the `backups` folder beside the database.

---

## Updating

Quit OmniMem from the tray, download the newer `.msi` and run it, SmartScreen and all. It replaces the old version in place and leaves your data alone. Then open OmniMem from the Start menu again.

---

## Using this PC from other machines

If a laptop or another computer should use the OmniMem on this PC, it has to listen beyond localhost. On the Configuration page:

1. Set **Listen address** to `0.0.0.0`
2. Set an **Access token**. Any long random string will do; in PowerShell, `-join ((1..32) | % { '{0:x2}' -f (Get-Random -Max 256) })` makes one
3. Restart OmniMem

OmniMem refuses to listen beyond localhost without a token or OAuth, so step 2 isn't optional. When it restarts, Windows Defender Firewall may ask whether to let OmniMem through: allow it on private networks only. Then find the PC's address with `ipconfig` (the **IPv4 Address** line), and point the other machine's client at `http://<pc-ip>:8765/mcp` with the token as a bearer header.

For anything outside your own network, put it behind HTTPS: see [remote access](../docs/remote-access.md), or the [Tailscale Funnel guide](omnimem-setup-linux-tailscale.md) for the idea.

---

## Coming from 6.x

1. On 6.x, call `dump_to_file` (or use the old web UI's Backups page)
2. In the new app, open **Settings… → Backups**, upload the file and restore it

Backups don't carry vectors, so every memory gets re-embedded, at roughly 8 ms each. Then change your client configs from `/sse` to `/mcp`, and you can retire Docker Desktop if OmniMem was the only thing using it.

---

## Uninstalling

**Settings → Apps → Installed apps**, find **OmniMem**, and choose **Uninstall**. Quit it from the tray first. The uninstall removes the app but leaves your memories in `%APPDATA%\squarecows\OmniMem\data`, so a reinstall picks up where you left off. Delete that folder too if you want them gone.

---

## Troubleshooting

**"Windows protected your PC" with no Run button**: click **More info** first. **Run anyway** only appears after that. If your PC is managed by work and there's still no button, your IT department has blocked unsigned apps, and there's nothing OmniMem can do about that until it's signed. The [Docker guide](docker.md) is the way round it, if Docker's allowed.

**The installer stops about the Visual C++ Redistributable**: install it from [Microsoft](https://aka.ms/vs/17/release/vc_redist.x64.exe), then run the OmniMem installer again.

**No tray icon**: look under the **^** arrow by the clock. If it's not there either, open OmniMem from the Start menu again to bring up the settings window and see what the status says.

**The status line says OmniMem failed**: it gives the reason. The usual suspects are the port being in use (change **Port**) or a non-local **Listen address** with no access token.

**Port 8765 is taken**: change **Port** on the Configuration page, restart, then use **Copy MCP URL** again so your clients get the new address.

**The first start sits at "starting"**: it's downloading the embedding model. Give it a minute on a slow connection, and check a proxy or firewall isn't blocking huggingface.co.

**The settings window is blank**: it needs Microsoft Edge WebView2. Windows 11 has it built in; on Windows 10, install the **Evergreen Runtime** from Microsoft's [WebView2 page](https://developer.microsoft.com/microsoft-edge/webview2/), then restart OmniMem.
