# Setting Up OmniMem on macOS

On a Mac, OmniMem is a menu bar app. You drag it into Applications, open it, and it sits up there next to the clock running your memory server. No Docker Desktop, no terminal, no `.env` file to get wrong.

The DMG is a universal build, so it runs natively on Apple Silicon and Intel.

---

## What you need

- **macOS 13 (Ventura) or later**
- Somewhere around a few hundred MB of free RAM for the embedding model. Any Mac from the last several years is fine
- An Anthropic API key if you want the Claude-powered extras (optional)

---

## Step 1: Install the app

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](../docs/quick-start.md#build-from-source)).

1. Download `OmniMem-<version>.dmg` from the [releases page](https://code.squarecows.com/ric/omnimem/releases)
2. Open it and drag **OmniMem** into **Applications**
3. Open OmniMem from Applications

The app is signed and notarised, so macOS opens it without the "unidentified developer" nonsense.

There's no Dock icon. OmniMem lives in the menu bar, and that's deliberate: it's a background service you glance at, not a window you manage.

---

## Step 2: First start

The first time it runs, OmniMem downloads the embedding model (all-MiniLM-L6-v2) and loads it. The menu bar menu says it's starting while that happens. After that it reads the model from the cache and starts in a second or so.

Click the menu bar icon and you'll see:

- **A status line**: whether it's running, how many memories it holds, and the MCP address
- **Settings…**: opens the settings window
- **Copy MCP URL**: puts `http://127.0.0.1:8765/mcp` on your clipboard for your client config
- **Start at login**: tick it and OmniMem comes up whenever you log in
- **Quit OmniMem**

Tick **Start at login**. You want your memory there before you start work, not after you've remembered to open it.

Only one copy runs at a time. Open OmniMem again while it's running and it just brings the settings window forward.

---

## Step 3: Settings

**Settings…** opens a window with everything the old 6.x web dashboard had: memories, projects, experience and the graveyard, skills, feeds, telemetry and backups. It isn't a website though. The pages come from the app itself, so there's no port to expose, no login page and nothing else on your network can reach them. See [the settings panel](../docs/settings-panel.md).

The **Configuration** page is where the settings live. The ones you'll most likely touch:

- **Anthropic API key**: switches on fact extraction, query expansion, contradiction checks and RSS summaries. Without it OmniMem still works, it just summarises RSS by truncation and skips the Claude extras
- **Access token**: only needed if something other than this Mac will connect
- **Port**: 8765 unless something else wants it

Secrets (the API key, the access token, the OAuth password) go into your macOS Keychain and aren't shown again once saved. Everything else is written to `omnimem.env` in the data folder. Changes apply when you restart OmniMem, and the page tells you so.

Your data lives in `~/Library/Application Support/com.squarecows.OmniMem`: the database, `feeds.yml` and the `backups` folder.

---

## Step 4: Connect Claude Code

Use **Copy MCP URL**, then:

```bash
claude mcp add --transport http omnimem http://127.0.0.1:8765/mcp --scope user
```

Allow the OmniMem tools without a prompt each time, in `~/.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

The [connection guides](README.md) cover Claude Desktop, Cursor, Copilot and the rest.

---

## Step 5: RSS feeds (optional)

Open **Settings…** and go to **RSS feeds**. Add a feed with its URL, name, topics and licence class, and OmniMem picks it up straight away. It also checks every six hours by default (**Check feeds every (hours)** on the Configuration page).

Prefer a file? The reading list is `feeds.yml` in the data folder, and the feeds page can upload and download it.

---

## Backups

**Settings… → Backups** creates a backup, restores one (re-embedding everything as it goes), and downloads it into your Downloads folder. Your agent can do the same with the `dump_to_file` tool. Backups are JSON files in the `backups` folder beside the database.

---

## Using the Mac from other machines

If a laptop or another computer should use the OmniMem on this Mac, it has to listen beyond localhost. On the Configuration page:

1. Set **Listen address** to `0.0.0.0`
2. Set an **Access token** (generate one with `openssl rand -hex 32`)
3. Restart OmniMem

OmniMem refuses to listen beyond localhost without a token or OAuth, so step 2 isn't optional. Then find the Mac's address:

```bash
ipconfig getifaddr en0
```

and point the other machine's client at `http://<mac-ip>:8765/mcp` with the token as a bearer header. For anything outside your own network, put it behind HTTPS: see [remote access](../docs/remote-access.md), or the [Tailscale Funnel guide](omnimem-setup-linux-tailscale.md) for the idea.

---

## A Mac mini as a headless server

If the Mac is a server in a cupboard and nobody's logged in, the menu bar app is the wrong shape, because it runs in your login session. Run the Docker image instead: see [Running OmniMem in Docker](docker.md). The headless `.deb`, `.rpm` and tarball are Linux only.

---

## Coming from 6.x

1. On 6.x, call `dump_to_file` (or use the old web UI's Backups page)
2. In the new app, open **Settings… → Backups**, upload the file and restore it

Backups don't carry vectors, so every memory gets re-embedded, at roughly 8 ms each. Then change your client configs from `/sse` to `/mcp`, and you can retire Docker Desktop if OmniMem was the only thing using it.

---

## Troubleshooting

**No menu bar icon**: on a Mac with a crowded menu bar, macOS hides icons that don't fit, and a notch doesn't help. Quit a few other menu bar apps, or open OmniMem again from Applications to bring up the settings window.

**The status line says OmniMem failed**: it gives the reason. The usual suspects are the port being in use (change **Port**) or a non-local **Listen address** with no access token.

**Port 8765 is taken**: change **Port** on the Configuration page, restart, then use **Copy MCP URL** again so your clients get the new address.

**The first start sits at "starting"**: it's downloading the embedding model. Give it a minute on a slow connection.
