# OmniMem Development Guide

## What is this?

Self-hosted semantic memory for AI agents, served over MCP. Memories live in five namespaces: episodic (decisions, bugs, patterns), project context (stack, goals, state, work-type domains), knowledge (RSS articles and extracted facts), preferences (prescriptive rules pulled from conversation) and compiled skills (SKILL.md documents distilled from experience, written only through a propose-and-accept gate). Every memory carries a `licence` (own, open, restricted, unknown) and a `provenance` (retrieved, concluded, asserted).

**Version**: 7.0.3-dev, on branch `v7.0.x`. OmniMem 7 is a Rust rewrite of the 6.x Python stack (Valkey, FastMCP server, Starlette web UI, RSS worker) as **one binary**: SQLite with exact in-memory vector search, the MCP server, the enrichment worker and RSS scheduler in one process, and a desktop app whose settings panel replaces the web UI. The 6.x Python lives on the `v6.x` branches only.

**Stack**: Rust (edition 2024, see `rust-version` in `Cargo.toml`), rusqlite (bundled SQLite), ONNX Runtime via `ort` plus `tokenizers` running all-MiniLM-L6-v2 (384-dim), rmcp over streamable HTTP on axum, minijinja for the panel, tao + wry + tray-icon for the desktop app, reqwest (rustls) for the Anthropic API and feeds, feed-rs, keyring.

The working plan and the evidence for each phase is `docs/rust-port-plan.md`; the Mycelium adapter spec is `docs/v7-change-spec.md`. Update the plan's progress table as phases land.

## Workspace

```
crates/
  omnimem-core/      Namespaces, keys, record types, validation, licence and provenance vocabularies,
                     content hash, env (settings read environment first, then the desktop overlay)
  omnimem-embed/     ONNX embedder: model resolution (local dir, HF cache, pinned download), pooling
  omnimem-store/     SQLite schema and migrations (PRAGMA user_version), vector matrix, filtered exact k-NN,
                     kv (what were meta:* keys), enrichment queue, OAuth tables, 6.x backup import/export
  omnimem-engine/    Recall pipeline, lifecycle, dedup, contradictions, experience, projects and domains,
                     lineage, chunking, temporal, briefing, maintenance, skills compiler/scan/transfer, enrichment
  omnimem-llm/       Blocking Anthropic Messages client behind the core LanguageModel trait
  omnimem-mcp/       The 48 tools (descriptions trimmed of their Args: blocks; the full guidance lives in docs/agent-guide.md), instructions, streamable HTTP,
                     bearer auth, the OAuth 2.1 authorisation server (src/oauth/), Host/Origin guards
  omnimem-rss/       Feed fetch and parse, summary and digest modes, licence gate, scheduler, feed influence
  omnimem-settings/  The settings panel: axum routes never bound to a socket, minijinja templates and static
                     files embedded by build.rs, the Configuration page and the SecretStore trait
  omnimem-app/       What server and desktop share: open the engine, run the services, data folder, instance lock
  omnimem-desktop/   Tray or menu bar icon, settings window on the omnimem:// scheme with IPC, keychain, start at login
  omnimem/           The binary: serve, import, export, stats, search, embed, rss, hook (Claude Code hooks), and desktop (default with the feature)
ci/desktop/          Container with GTK/WebKitGTK/AppIndicator for building and smoke-testing the desktop crate
ci/flatpak/          Container with flatpak-builder, Xvfb and D-Bus for building and smoke-testing the Flatpak
ci/checks/           Container with llvm-tools, cargo-llvm-cov and cargo-audit for coverage and the dependency audit
packaging/           What the installers carry: linux/ (systemd unit, env file, scripts), windows/ (WiX source, icon), flatpak/ (manifest, desktop file, metainfo), docker/ (the server image)
claude_config/       CLAUDE.md template and MCP config for end users
examples/feeds.yml   Sample reading list
docs/, guides/       User documentation
```

`omnimem-desktop` is outside the workspace's `default-members`, so plain `cargo build` and `cargo test` need no GUI libraries.

## Running locally

```bash
cargo run -p omnimem -- serve                   # MCP at http://127.0.0.1:8765/mcp, data in ./data/omnimem.db
cargo run -p omnimem -- import backup.json      # a 6.x dump_to_file backup, re-embedded
OMNIMEM_DB=/tmp/x.db cargo run -p omnimem -- stats
```

Configuration is the 6.x environment variables (see `crates/omnimem-settings/src/configuration.rs` for the catalogue the panel shows). The desktop app reads `omnimem.env` from its data folder and secrets from the OS keychain into `omnimem_core::env`'s overlay at start; `serve` never does. The production 6.x stack on this host holds ports 8080 and 8765, so use another `MCP_PORT` for live checks.

## Tests and checks

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
scripts/desktop-check.sh     # desktop crate: clippy, tests and a smoke test under Xvfb, in ci/desktop
scripts/macos-package.sh     # on a Mac: OmniMem.app in a DMG plus a binary tarball, in dist/macos
scripts/linux-package.sh     # anywhere with Docker: the server .deb, .rpm and tarball, and the tray app tarball, for this arch
scripts/windows-package.sh   # anywhere with Docker: the Windows exe cross-built with cargo-xwin and an MSI from wixl, in dist/windows
scripts/flatpak-package.sh   # anywhere with Docker: the tray app as a Flatpak bundle for this arch, installed and smoke-tested in its sandbox
scripts/docker-image.sh      # after linux-package.sh: the server image from the .deb, for this arch, tested (--push, --manifest for CI)
scripts/coverage.sh          # anywhere with Docker: line coverage of the default members, in dist/coverage, with a badge
scripts/security-check.sh    # anywhere with Docker: cargo audit of Cargo.lock and a gitleaks scan of the history
```

- CI is `.forgejo/workflows/build.yml`, on pushes to any branch but `main`, `v7.*` pull requests and `v7.*` tags. The `linux` jobs (`[linux, amd64]` and `[linux, arm64]`) run `scripts/desktop-check.sh --workspace` (format, clippy, tests, then the desktop checks and smoke test), `scripts/linux-package.sh`, `scripts/flatpak-package.sh` (its GNOME SDK, flatpak-builder state and cargo caches in `~/.cache/omnimem-flatpak` on the runner, so only the first run downloads them) and `scripts/docker-image.sh` (a v7 tag also pushes `richarvey/omnimem:<version>-<arch>`, which `release` joins into `<version>`, plus `latest` for a version without a `-`); the `macos` job runs the same checks, `scripts/macos-package.sh` and the smoke test; the `windows` job (`[linux, arm64]`) runs `scripts/windows-package.sh`, with no smoke test since nothing there runs Windows. Once all of them pass, `release` publishes every package: a branch push replaces `dev-<branch>`, a tag attaches to its release. Everything uses the Rust version in `ci/desktop/Dockerfile`, so moving the toolchain is that one line. Beside it, `.forgejo/workflows/checks.yml` runs `scripts/coverage.sh` (`[linux, arm64]`, report-only, with `coverage-v7.svg` pushed to the `badges` branch from `v7.*` branches) and `scripts/security-check.sh` (`[linux, amd64]`, failing on a RustSec vulnerability or a leaked secret) without gating the packages. `[skip ci]` in a commit message skips both workflows; Forgejo doesn't recognise `[skip-ci]`

- The dev host runs out of memory on parallel builds: set `CARGO_BUILD_JOBS=2` and run the desktop check on its own, never alongside another cargo job
- SQLite runs in memory in tests; there are no store fakes, so a fake can't diverge from the real thing
- Golden fixtures pin byte-level contracts with 6.x: reference vectors (`omnimem-embed/tests/fixtures`), skill bodies (`omnimem-engine/tests/fixtures/skills_golden.json`), the OAuth HTTP surface (`omnimem-mcp/tests/fixtures/oauth_golden.json`). The capture scripts beside them need a `v6.7.x` checkout
- Tests that need the real embedding model run only when the Hugging Face cache has it

## Compatibility contract with 6.x

Kept exactly: the 48 tool names, parameters, defaults and result JSON (including `_compact()` dropping empty values); vectors; recall scoring (floor, weak band, experience weight for every outcome but abandoned, temporal boost, reinstate 0.6, fact collapse, ordering); rendered skill bodies byte for byte; backup JSON, skill bundle zips (format v2, reads v1) and `feeds.yml`; every 6.x environment variable except the Valkey, web UI and PyTorch ones.

Deliberately different (each in the CHANGELOG): tool descriptions trimmed of their `Args:` blocks; recency declines from day one to 0.9 at `RECENCY_DECAY_DAYS`, so past it the multiplier is 0.1 below 6.x (the floor and weak band read raw similarity, so they don't move with it); an abandoned outcome weighs 1.0, not 0.1; abandonment no longer auto-suppresses; project-namespace hits carry `tags`, which 6.x's project return fields never listed; query expansion without `ANTHROPIC_API_KEY` logs once instead of nothing. A 6.x import embeds every memory from its text, never trusting a stored vector, which is why a memory 6.x's KNN had lost (a missing or stale vector on the hash) recalls normally here.

Deliberately gone: Valkey, the web UI and `/metrics` over HTTP, SSE transport, `EMBEDDING_BACKEND=torch`. Over HTTP the binary serves `/mcp`, the OAuth routes and `/healthz`, nothing else.

## Design rules that still bind

- **Skill compilation is deterministic**: the same sources render a byte-identical body except the `compiled_at` line. No randomness, map-order dependence or extra timestamps in rendering, or every recompile proposes a noise diff. The skill banner still says "(Valkey)" until cut-over for the same reason
- **Skill writes are gated**: `compile_skill(mode="propose")` stashes the draft; `mode="write"` commits that stash verbatim and refuses if the stored skill changed. Experience and graveyard writes stay ungated; that asymmetry is the design
- **Domains route, they never label memories**: a project's domains narrow which projects a recall searches; memories don't inherit them. An unmatched domain is always reported, and an empty domain and project intersection returns nothing rather than searching everything
- **Licence and provenance are decided at write time and never scored on**. A fact inherits its source's licence and provenance. Reclassifying goes through the lineage stamp, cascades to chunks and extracted facts, and never bumps `updated_at` (the skill compiler reads that as "source changed")
- **Promotion and feed influence feed skills; ordinary knowledge doesn't.** Refs and feed rules bypass the reinforcement gate but never count towards it or bootstrap a skill
- **Settings are read through `omnimem_core::env`**, never `std::env::var` directly, and nothing writes to the process environment (not sound once the keychain has started threads)
- **The fail-closed rule**: a non-loopback `MCP_HOST` needs `MCP_AUTH_TOKEN` or OAuth; OAuth switched on but incomplete refuses to start
- **OAuth**: refresh tokens rotate with a grace window (`OAUTH_REFRESH_GRACE_SECONDS`) in which the old token replays the same successor pair, which stops claude.ai's concurrent refreshes being signed out. Codes and tokens are stored as SHA-256 hashes; a code is consumed and a token rotated inside one `Store::with_oauth` transaction

## Gotchas

- **WebKitGTK needs wry's `linux-body` feature** (WebKitGTK 2.40+) or the custom scheme gets empty POST bodies, and it doesn't follow redirects from a custom scheme, so the panel answers a form with a page that replaces itself (`see_other`) rather than a 303
- **Webview downloads are unreliable** (WebKitGTK saves silently, macOS won't report), so the panel writes exports and backups into the Downloads folder itself and says where
- **The panel is never bound to a socket**: `Panel::handle` runs axum routes in-process for the window's `omnimem://` handler. Don't add HTTP routes for it
- **GNOME shows no tray icon without the AppIndicator extension**; the app opens its window instead
- **No Intel Mac build**: `ort`'s `download-binaries` has no ONNX Runtime for `x86_64-apple-darwin`, so the DMG is arm64 only. Its static library is built for macOS 13.4, which is the app's minimum. A Homebrew Rust has only its host target, so cross-target builds need rustup's toolchain first in PATH
- **All three Forgejo runners carry the `sqcows` label** (Linux amd64, Linux arm64 and the Mac), so `runs-on: sqcows` can land on any of them. Use `macos` or `linux` when the platform matters
- **Release builds keep proc macros unstripped** (`[profile.release.build-override] strip = false`): with Rust 1.96 on macOS, `strip = "symbols"` reaching proc-macro dylibs fails the build with E0463 "can't find crate for `zerofrom_derive`" (or `tokio_macros`, whichever comes first). Debug builds never strip, which is why clippy and the tests pass regardless
- **Every push to a branch other than `main` replaces that branch's `dev-<branch>` pre-release** with the macOS and Linux packages (`scripts/forgejo-release.sh --replace`, from the `release` job only). Version tags (`v7.*`, checked against `Cargo.toml`) attach to their own release instead. Publishing stays in that one job after `needs`: a build job that published on its own would race the `--replace` and lose its files
- **Linux builds need glibc 2.38 or newer, and can't be built on anything older than Debian 13**: ort's prebuilt `libonnxruntime.a` calls the `__isoc23_*` functions (glibc 2.38) and needs GCC 14's libstdc++ (`__cxa_call_terminate`), so a Debian 12 build fails to link and a binary from anywhere won't start on Debian 12 or Ubuntu 22.04. `scripts/linux-package.sh` builds both in `ci/desktop`; the headless one with `CXXSTDLIB=static=stdc++` (plus `-L native=` GCC's folder, which rustc doesn't search) in its own target directory, so it needs only glibc, and runs it in `ubuntu:24.04` before packaging
- **Tarballs made on a Mac need `COPYFILE_DISABLE=1` and `--no-xattrs`** (and `--no-mac-metadata` for bsdtar), or they carry `._omnimem` and `._LICENSE` files and Apple xattr headers that macOS's own tar hides from you but every other tar shows
- **Never write `~/.claude.json` from the app**: running Claude Code sessions rewrite it constantly, so a direct edit can lose theirs or have yours lost. The tray's Add to Claude Code only reads it and changes things through `claude mcp add` / `remove`, run with the usual bin folders on PATH because an app launched from Finder doesn't get the shell's. Claude Desktop's config is fine to edit (only it writes the file, at start): `clients.rs` keeps a backup and renames a complete file into place
- **On Linux a tray icon is "created" even when nothing will show it**: libayatana-appindicator succeeds with no StatusNotifierItem host, which is stock GNOME without the AppIndicator extension, so the app checks the session bus for `org.kde.StatusNotifierWatcher` (`tray_host_running`, through gtk's gio) and, without one, opens the window with `model::NO_TRAY` set as the panel's notice (`Panel::set_notice`, shown on every page). Linux and Windows trays get `icon::tray`, the compact square `<o>`, since their hosts fit icons into a square
- **The Windows exe needs `DirectML.dll` beside it and the Visual C++ runtime**: ONNX Runtime's Windows build imports both (and `MSVCP140` comes from its C++, so `+crt-static` can't remove the runtime). The MSI ships DirectML (in the target folder it's a symlink into the ort cache that only resolves inside the container) and has a launch condition on HKLM `SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64` `Installed`. `windows-package.sh` fails on any imported DLL outside its allowlist. `wixl` is its own Debian package, not part of `msitools`
- **The Flatpak builds with the network on**: `build-args: --share=network` lets cargo fetch crates and ort fetch ONNX Runtime as on any other build, with cargo's home and target in `/cache/flatpak-cargo` (mounted in with `--filesystem`) so rebuilds are incremental. Fine for a bundle we publish ourselves; Flathub requires offline builds, which means vendoring the crates (`flatpak-cargo-generator`) and ONNX Runtime as an archive source with `ORT_LIB_LOCATION`. The GNOME runtime has no libxdo, which tray-icon's default `muda-libxdo` feature links, so tray-icon is built with `default-features = false, features = ["libappindicator"]` (libxdo only fakes key presses for predefined Copy/Paste menu items, which the tray doesn't use)
- **Inside the Flatpak, the tray icon's file goes in `$XDG_RUNTIME_DIR/app/$FLATPAK_ID`** (`model::flatpak_icon_dir`, through `TrayIconBuilder::with_temp_dir_path`): libayatana hands the tray host a file path, and that folder is the only part of the sandbox the desktop outside can read. Start at login writes `~/.config/autostart/OmniMem.desktop` through the `xdg-config/autostart:create` permission, with `flatpak run --command=omnimem com.squarecows.OmniMem desktop` as its command (`model::login_command`), since `/app/bin` doesn't exist outside. Add to Claude Code is disabled (`model::CODE_IN_FLATPAK`): seeing and running the host's `claude` would take `--talk-name=org.freedesktop.Flatpak`, which is the whole sandbox
- **Installing a Flatpak bundle in a container needs three things the container lacks** (`flatpak-package.sh`): a passwd entry for the calling uid (D-Bus won't start a session without one), a D-Bus session around both `flatpak install` and `flatpak run`, and a system bus, which install asks for parental controls (malcontent). Pointing `DBUS_SYSTEM_BUS_ADDRESS` at the session bus does it: the accounts service is simply absent there, which flatpak takes as "no controls". The error for each is the same unhelpful "Could not connect: No such file or directory"; `flatpak -vv` shows which step it is. Docker Desktop also shows the bind-mounted checkout with another owner, so git needs `safe.directory` (set through `GIT_CONFIG_*` env) before flatpak-builder will read it
- **The desktop exe is a windowed program on Windows** (`windows_subsystem = "windows"` when built with `desktop`), so a Start menu launch opens no console; `attach_parent_console` lets commands run from a terminal print to it
- **The server packages depend on `ca-certificates`**: reqwest verifies TLS through `rustls-platform-verifier`, which on Linux reads the system CA store, so without it the model download, Claude and every feed fail TLS. Slim images (`debian:13-slim`, the Docker image's base) don't have it, which is also why the `.deb` test installs with `apt-get install ./…deb`, not `dpkg -i`
- **The Docker image is the `.deb` on Debian 13**, built natively per architecture and joined with `docker manifest`, so nothing builds under QEMU. Only `v7.*` tags push, and only a version without a `-` moves `latest`: 6.x's workflow pushed `:latest` on every run, so a manual run on `main` would have overwritten the public images. It runs as uid 1000 (as 6.x did), created before the package so postinst finds the user
- **Linux package versions use `~` for `-`** (`7.0.2~dev`) so a dev build sorts before its release in dpkg and rpm. Use `tr`, not `${v//-/\~}`: bash 3.2 keeps the backslash, 5.2 doesn't
- **Clippy's `result_large_err` fires on `Result<T, Response>`** (axum's response is at least 128 bytes). Where the error is the response every caller returns at once, it's allowed per function with the reason
- **Numbers rendered where 6.x output is compared** are formatted to match Python (`str(float)` and friends), not Rust's defaults

## Validation constraints

- Project names: alphanumeric, hyphens, underscores, dots, spaces
- Content: max 50 KB per memory; tags: max 20, each 100 characters or fewer
- Project domains: max 20, same charset as skill domains, aliases resolve
- Skill domains: lowercase kebab-case, 1 to 64 characters of `[a-z0-9._-]`
- `set_licence` and `set_provenance` take at most 200 keys per call
- Key prefixes: `mem:episodic:`, `mem:project:`, `mem:knowledge:`, `mem:preference:`, `mem:skill:`

## Committing

Commit after each meaningful piece of work. The repo lives on Forgejo at `code.squarecows.com` (owner `ric`), pushed over SSH on port 222; the Forgejo MCP points at it for PRs, releases and CI jobs.

**Branch policy**: version lines are developed and released from their own branch (`v7.0.x` here). Cut tags and releases on the version branch. Never merge a version branch into `main` unless Ric asks.

## Writing style

- British English (colour, summarised, centre)
- Conversational and human, no em dashes, no marketing fluff
- Docs and guides in Ric's own voice (his published articles are the reference), lighter on reference pages
- Technical but accessible, with concrete numbers where possible
