# Setting Up OmniMem on a Raspberry Pi

A Raspberry Pi under the desk is a lovely home for OmniMem. It's on all the time, it sips power, and OmniMem 7 is a single arm64 binary that doesn't care that it's not a proper server.

In 6.x this meant four containers and a vector database squeezed onto a Pi. Now it's a `.deb` and a systemd service.

---

## What you need

- **Raspberry Pi 4 or Pi 5** with 2 GB of RAM or more. OmniMem itself uses a few hundred MB, most of which is the embedding model
- **Raspberry Pi OS (64-bit)**, Bookworm or later. The 32-bit OS won't do, the build is arm64
- **An SD card or SSD with a few GB free.** The binary, the model and a database of thousands of memories are all small; an SSD just makes everything nicer
- An Anthropic API key if you want the Claude-powered extras (optional)

---

## Step 1: Install the package

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](../docs/quick-start.md#build-from-source)).

Download the arm64 `.deb` from the [releases page](https://code.squarecows.com/ric/omnimem/releases) and install it:

```bash
sudo apt install ./omnimem_<version>_arm64.deb
```

That gives you:

- `omnimem` on your path
- a systemd service, `omnimem.service`, running `omnimem serve` as its own `omnimem` system user
- configuration in `/etc/omnimem/omnimem.env`
- data in `/var/lib/omnimem`: the database, `feeds.yml`, backups and the model cache

It's the headless build: no desktop libraries, no tray icon, no settings window. You configure it with the environment file and talk to it through your agent and the CLI.

---

## Step 2: Configure it

Open the environment file:

```bash
sudo nano /etc/omnimem/omnimem.env
```

By default OmniMem only listens on `127.0.0.1`, which is no good if your laptop is the thing connecting. To use it from other machines on your network, listen on everything and set an access token:

```bash
MCP_HOST=0.0.0.0
MCP_AUTH_TOKEN=paste-a-long-random-token-here
```

Generate a token with:

```bash
openssl rand -hex 32
```

The token isn't optional here. OmniMem refuses to start listening beyond localhost without a token or OAuth, which is the kind of nagging I'm happy to have.

If you want AI-powered RSS summaries, fact extraction and contradiction checks, add:

```bash
ANTHROPIC_API_KEY=sk-ant-your-key-here
```

Without it OmniMem still works: RSS summaries fall back to truncation and the Claude extras stay off.

Every other setting is in the [configuration reference](../docs/configuration.md).

---

## Step 3: Start it

```bash
sudo systemctl enable --now omnimem
```

The first start downloads the embedding model into `/var/lib/omnimem`, which takes a minute on a Pi. Watch it:

```bash
journalctl -u omnimem -f
```

You're looking for the line saying the MCP server is listening on `/mcp`. Press `Ctrl+C` to stop following the log; OmniMem keeps running, and comes back after a reboot.

---

## Step 4: Check it's running

```bash
curl http://localhost:8765/healthz
```

That should answer `{"status": "ok"}`. From another machine, swap `localhost` for the Pi's address (`hostname -I` on the Pi tells you).

---

## Step 5: Connect your agent

On the machine where you run Claude Code:

```bash
claude mcp add --transport http omnimem http://<your-pi-ip>:8765/mcp \
  --header "Authorization: Bearer your-token-here" \
  --scope user
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

The [connection guides](README.md) cover the other agents.

---

## Step 6: RSS feeds (optional)

The reading list is `/var/lib/omnimem/feeds.yml`:

```bash
sudo -u omnimem nano /var/lib/omnimem/feeds.yml
```

```yaml
feeds:
  - name: "Rust Blog"
    url: "https://blog.rust-lang.org/feed.xml"
    topics: ["rust", "programming"]
  - name: "Raspberry Pi News"
    url: "https://www.raspberrypi.com/news/feed/"
    topics: ["raspberry-pi", "hardware"]
```

OmniMem notices the change and checks the feeds. It also checks when it starts and every six hours (`RSS_SCHEDULE_HOURS`). To see what a feed would bring in without storing anything:

```bash
sudo -u omnimem omnimem --db /var/lib/omnimem/omnimem.db rss --dry-run
```

See [RSS and knowledge](../docs/rss-knowledge.md) for licences, digests and skill influence.

---

## Keeping it running

### Updating

Download the new `.deb` and install it over the top:

```bash
sudo apt install ./omnimem_<new-version>_arm64.deb
sudo systemctl restart omnimem
```

The database migrates itself when OmniMem starts.

### Backups

Ask your agent to call `dump_to_file`, which writes a JSON backup into `/var/lib/omnimem/backups`. Or from the CLI:

```bash
sudo -u omnimem omnimem --db /var/lib/omnimem/omnimem.db export /var/lib/omnimem/backups/omnimem-$(date +%Y%m%d).json
```

Copy those somewhere that isn't the Pi's SD card. SD cards die, usually on the day you needed them.

---

## Docker instead

If you already run everything on the Pi in containers, the `richarvey/omnimem` image is multi-arch and runs just as happily. See [Running OmniMem in Docker](docker.md).

---

## Coming from 6.x

1. On 6.x, call `dump_to_file` (or use the web UI's Backups page) and copy the JSON file to the Pi
2. Stop the old stack: `docker compose down` in the 6.x folder
3. Import the backup:

```bash
sudo -u omnimem omnimem --db /var/lib/omnimem/omnimem.db import backup.json
sudo systemctl restart omnimem
```

The import re-embeds every memory. At roughly 8 ms each on a desktop CPU a few thousand memories take under a minute; a Pi is slower, so give it a bit longer. Then change your clients from `/sse` to `/mcp`.

---

## Troubleshooting

**The service won't start**: `journalctl -u omnimem -e`. The most common reason is `MCP_HOST=0.0.0.0` without `MCP_AUTH_TOKEN`, which OmniMem refuses on purpose; the log says so.

**The first start takes ages**: it's downloading the embedding model. On a slow connection, go and make a cup of tea.

**Can't connect from another machine**: check `MCP_HOST=0.0.0.0` is set, the service was restarted, and your client is sending the token. `curl http://<pi-ip>:8765/healthz` from the other machine tells you whether the network part works.

**Clients get 401**: the token after `Bearer` has to match `MCP_AUTH_TOKEN` exactly.
