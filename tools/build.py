#!/usr/bin/env python3
"""Build the site's generated pages: the setup guides, the Claude Code memory
page and the 404 page.

The guides come from guides/*.md on a git ref of the main project (origin/main
by default, which is what's released), so the site never carries its own copy
to drift. The other pages come from tools/content/*.md. Each page gets the
homepage's own header and footer, lifted out of index.html at build time, so
there is one place to change them.

    python3 tools/build.py              # guides from origin/main
    python3 tools/build.py v7.0.x       # or any other ref

Needs pandoc. Writes guides/, claude-code-memory/, 404.html, assets/site.js
and the sitemap. Run `git fetch` first so the ref is current.
"""

import datetime
import hashlib
import html
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = "https://omnimem.org"
FORGEJO = "https://code.squarecows.com/ric/omnimem"
REF = sys.argv[1] if len(sys.argv) > 1 else "origin/main"

# Meta descriptions for the guides, written for search results rather than
# cut from each guide's first paragraph (which ends mid-sentence). A guide
# missing here falls back to that cut, so a new guide still builds.
DESCRIPTIONS = {
    "claude-code": "Connect Claude Code to OmniMem over HTTP: one CLI command, access tokens, global or project config, and the hooks that brief every session.",
    "claude-desktop": "Connect Claude Desktop to OmniMem with the mcp-remote bridge: where the config file lives on macOS and Windows, the setup, and troubleshooting.",
    "claude-ai": "Use OmniMem from claude.ai in the browser: switch on OAuth 2.1, put it behind a public HTTPS address, add the connector and sign in.",
    "cursor": "Connect Cursor to OmniMem over Streamable HTTP: the mcp.json entry, global or project config, enabling the tools, and known quirks.",
    "codex": "Connect OpenAI's Codex CLI to OmniMem: two lines of TOML, where the config file lives, passing a token from an environment variable, and quirks.",
    "github-copilot": "Connect GitHub Copilot in VS Code to OmniMem: the MCP config, user or workspace settings, why Agent mode is needed, and known quirks.",
    "gitlab-duo": "Connect GitLab Duo in VS Code or JetBrains to OmniMem: the MCP config, its authentication catch with custom headers, and known quirks.",
    "kiro": "Connect Kiro, AWS's AI IDE, to OmniMem over Streamable HTTP: the MCP config, user or project scope, Kiro's MCP panel and known quirks.",
    "opencode": "Connect OpenCode to OmniMem as a remote MCP server, with an access token or OAuth: the config file, where it lives, and known quirks.",
    "open-design": "Connect Open Design to OmniMem over OAuth 2.1 as a public client: switch on OAuth, add HTTPS, add the server in Open Design and sign in.",
    "docker": "Run OmniMem in Docker: one container, one volume, one port. Quick start, Compose, RSS feeds, reverse proxy or OAuth, pinning, updates and backups.",
    "macos": "Set up OmniMem on a Mac: install the menu bar app from the DMG, first start, settings, connecting Claude Code, RSS feeds, backups and headless use.",
    "raspberry-pi": "Run OmniMem on a Raspberry Pi: the arm64 .deb and its systemd service, configuration, connecting your agent, RSS feeds and keeping it running.",
    "aws-linux": "Set up OmniMem on an AWS EC2 instance, or any Linux server: install the .deb or .rpm, configure it, add HTTPS with Caddy and connect your agent.",
    "gcp-linux": "Set up OmniMem on a Google Cloud Compute Engine VM: firewall, the .deb, configuration, HTTPS with Caddy, connecting your agent and RSS feeds.",
    "linux-tailscale": "Give OmniMem a public HTTPS address with Tailscale Funnel, so claude.ai and other OAuth clients can reach it. No domain, certificates or port forwarding.",
}

# Which guides point at which: a client guide needs OmniMem running somewhere,
# a host guide ends with connecting an agent.
HOSTS = ["macos", "docker", "raspberry-pi", "aws-linux", "gcp-linux", "linux-tailscale"]
CLIENTS = ["claude-code", "claude-desktop", "claude-ai", "cursor", "codex", "github-copilot", "gitlab-duo", "kiro", "opencode", "open-design"]
NEEDS_PUBLIC_HTTPS = {"claude-ai", "open-design"}

# The homepage's JSON-LD defines these; other pages point at them by @id.
RIC = {"@id": SITE + "/#ric", "@type": "Person", "name": "Ric Harvey"}
SQUARECOWS = {"@id": "https://squarecows.com/#org", "@type": "Organization", "name": "SquareCows Ltd"}
SOFTWARE = {"@id": SITE + "/#software", "@type": "SoftwareApplication", "name": "OmniMem"}
WEBSITE = {"@id": SITE + "/#website"}
IMAGE = SITE + "/omnimem-social.png"


def article_ld(path, headline, description, published, modified):
    return {
        "@type": "TechArticle",
        "@id": SITE + path + "#article",
        "headline": headline,
        "description": description,
        "url": SITE + path,
        "mainEntityOfPage": {"@type": "WebPage", "@id": SITE + path},
        "image": [IMAGE],
        "datePublished": published,
        "dateModified": modified,
        "inLanguage": "en-GB",
        "author": RIC,
        "publisher": SQUARECOWS,
        "about": SOFTWARE,
        "isPartOf": WEBSITE,
    }


def byline(updated):
    when = datetime.date.fromisoformat(updated)
    return (f'<span>By <a href="https://www.linkedin.com/in/richarvey">Ric Harvey</a></span>'
            f'<span>Updated <time datetime="{updated}">{when:%-d %B %Y}</time></span>')


# Guide files become /guides/<slug>/: the setup ones lose their prefix.
def slug_for(name):
    return name.removesuffix(".md").removeprefix("omnimem-setup-")


def git(*args):
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout


def pandoc(markdown):
    out = subprocess.run(
        ["pandoc", "-f", "gfm", "-t", "html5", "--no-highlight", "--wrap=none"],
        input=markdown, check=True, capture_output=True, text=True,
    ).stdout
    return out


def plain(markup):
    text = re.sub(r"<[^>]+>", "", markup)
    return re.sub(r"\s+", " ", html.unescape(text)).strip()


def summary(text, limit=158):
    if len(text) <= limit:
        return text
    cut = text[:limit].rsplit(" ", 1)[0].rstrip(",;:")
    return cut + "…"


# ---------------------------------------------------------------- chrome

INDEX = (ROOT / "index.html").read_text()


def lift(start, end):
    i = INDEX.index(start)
    j = INDEX.index(end, i) + len(end)
    # The homepage can use paths relative to /, but these pages live deeper,
    # so anything relative is made absolute.
    return re.sub(r'((?:src|href)=")(?!https?:|/|#|mailto:)', r"\1/", INDEX[i:j])


def site_header():
    h = lift('<header class="omm-header"', "</header>")
    h = re.sub(r' data-spy="[^"]*"', "", h)
    h = h.replace('href="#top"', 'href="/"').replace('aria-label="OmniMem, back to top"', 'aria-label="OmniMem home"')
    h = re.sub(r'href="#([a-z-]+)"', r'href="/#\1"', h)
    return h


def site_footer():
    f = lift('<footer class="omm-footer"', "</footer>")
    return f


def site_js():
    blocks = re.findall(r"<script>\n(/\* Site (?:header|footer): a port.*?)</script>", INDEX, re.S)
    assert len(blocks) == 2, "header and footer scripts not found in index.html"
    copy = """/* Code blocks get a header bar with the language and a copy button, so
   the button never sits on top of the code, however narrow the screen. */
(function(){
  document.querySelectorAll('.omm-prose pre').forEach(function(pre){
    var lang = (pre.className || '').split(' ')[0];
    var bar = document.createElement('div');
    bar.className = 'omm-code-bar';
    var label = document.createElement('span');
    label.className = 'omm-code-lang';
    label.textContent = lang;
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'omm-copy';
    btn.textContent = 'copy';
    btn.setAttribute('aria-label', lang ? 'Copy this ' + lang + ' code' : 'Copy this code');
    btn.addEventListener('click', function(){
      var code = pre.querySelector('code') || pre;
      if (navigator.clipboard) navigator.clipboard.writeText(code.innerText);
      btn.textContent = 'copied \\u2713';
      setTimeout(function(){ btn.textContent = 'copy'; }, 1400);
    });
    var wrap = document.createElement('div');
    wrap.className = 'omm-code';
    pre.parentNode.insertBefore(wrap, pre);
    bar.appendChild(label);
    bar.appendChild(btn);
    wrap.appendChild(bar);
    wrap.appendChild(pre);
  });
})();
"""
    return "/* Generated by tools/build.py from index.html. Don't edit by hand. */\n" + "\n".join(blocks) + copy


HEADER = site_header()
FOOTER = site_footer()


def asset(name):
    """/assets/<name> with a hash of its content, so a changed file is fetched
    fresh rather than served from a browser's four-hour cache."""
    digest = hashlib.sha256((ROOT / "assets" / name).read_bytes()).hexdigest()[:10]
    return f"/assets/{name}?v={digest}"


def page(*, path, title, description, body, crumbs=None, ld=None, robots="index, follow, max-image-preview:large"):
    url = SITE + path
    graph = []
    if crumbs:
        graph.append({
            "@type": "BreadcrumbList",
            "itemListElement": [
                {"@type": "ListItem", "position": i + 1, "name": name, "item": SITE + href}
                for i, (name, href) in enumerate(crumbs)
            ],
        })
    if ld:
        graph.append(ld)
    ld_tag = ""
    if graph:
        ld_tag = '\n  <script type="application/ld+json">\n' + json.dumps(
            {"@context": "https://schema.org", "@graph": graph}, indent=2, ensure_ascii=False
        ) + "\n  </script>"
    crumb_nav = ""
    if crumbs:
        items = []
        for i, (name, href) in enumerate(crumbs):
            if i == len(crumbs) - 1:
                items.append(f'<li aria-current="page">{html.escape(name)}</li>')
            else:
                items.append(f'<li><a href="{href}">{html.escape(name)}</a></li>')
        crumb_nav = f'<nav class="omm-crumbs" aria-label="Breadcrumb"><ol>{"".join(items)}</ol></nav>\n'
    canonical = f'\n  <link rel="canonical" href="{url}">' if robots.startswith("index") else ""
    t = html.escape(title, quote=True)
    d = html.escape(description, quote=True)
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{t}</title>
  <meta name="description" content="{d}">
  <meta name="author" content="Ric Harvey, SquareCows Ltd">
  <meta name="robots" content="{robots}">
  <meta name="theme-color" content="#0b0c0e">{canonical}
  <meta property="og:type" content="article">
  <meta property="og:site_name" content="OmniMem">
  <meta property="og:url" content="{url}">
  <meta property="og:title" content="{t}">
  <meta property="og:description" content="{d}">
  <meta property="og:image" content="{SITE}/omnimem-social.png">
  <meta property="og:image:width" content="1200">
  <meta property="og:image:height" content="630">
  <meta property="og:locale" content="en_GB">
  <meta name="twitter:card" content="summary_large_image">
  <meta name="twitter:title" content="{t}">
  <meta name="twitter:description" content="{d}">
  <meta name="twitter:image" content="{SITE}/omnimem-social.png">
  <link rel="icon" href="/favicon.ico" sizes="any">
  <link rel="icon" href="/favicon-32.png" sizes="32x32" type="image/png">
  <link rel="apple-touch-icon" href="/apple-touch-icon.png">
  <link rel="preload" href="/fonts/ubuntu-sans-400-600.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="preload" href="/fonts/ubuntu-700.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="stylesheet" href="{asset('site.css')}">{ld_tag}
</head>
<body>
{HEADER}
<main class="omm-page">
{crumb_nav}{body}
</main>
{FOOTER}
<script src="{asset('site.js')}"></script>
</body>
</html>
"""


def tidy(markup):
    # Tables scroll on their own on a phone rather than widening the page.
    markup = markup.replace("<table>", '<div class="omm-table"><table>').replace("</table>", "</table></div>")
    return markup


# ---------------------------------------------------------------- guides

def rewrite_links(markup, names):
    def fix(m):
        href = m.group(1)
        if re.match(r"[a-z]+:|#|/", href):
            return m.group(0)
        target, _, frag = href.partition("#")
        frag = "#" + frag if frag else ""
        if target == "README.md":
            return f'href="/guides/{frag}"'
        if target in names:
            return f'href="/guides/{slug_for(target)}/{frag}"'
        # Anything else in the repo (docs, scripts) stays on Forgejo.
        resolved = Path("guides", target)
        parts = []
        for part in resolved.parts:
            if part == "..":
                parts.pop()
            else:
                parts.append(part)
        return f'href="{FORGEJO}/src/branch/main/{"/".join(parts)}{frag}"'
    return re.sub(r'href="([^"]*)"', fix, markup)


def guide_date(name):
    out = git("log", "-1", "--format=%cs", REF, "--", f"guides/{name}").strip()
    return out or datetime.date.today().isoformat()


def guide_first_date(name):
    # Following renames, so a guide keeps its date when its file moved.
    out = git("log", "--follow", "--format=%cs", REF, "--", f"guides/{name}").split()
    return out[-1] if out else guide_date(name)


def related(slug, titles):
    """Links at the foot of a guide to the guides it leads to or from."""
    def items(slugs):
        return "".join(f'<li><a href="/guides/{s}/">{html.escape(titles[s])}</a></li>' for s in slugs if s in titles and s != slug)
    parts = []
    if slug in CLIENTS:
        if slug in NEEDS_PUBLIC_HTTPS:
            parts.append('<p>This needs OmniMem on a public HTTPS address. <a href="/guides/linux-tailscale/">Tailscale Funnel</a> is the easiest route.</p>')
        parts.append(f"<p>OmniMem needs to be running first:</p><ul>{items(HOSTS)}</ul>")
        parts.append(f"<p>Connecting another agent as well:</p><ul>{items([c for c in CLIENTS if c != slug][:5])}</ul>")
        if slug == "claude-code":
            parts.append('<p>Why bother? <a href="/claude-code-memory/">Persistent memory for Claude Code, beyond CLAUDE.md</a> covers what Claude Code keeps on its own and what OmniMem adds.</p>')
    else:
        parts.append(f"<p>Then connect your agent:</p><ul>{items(CLIENTS[:6])}</ul>")
        parts.append(f"<p>Or run it somewhere else:</p><ul>{items(HOSTS)}</ul>")
    return '<aside class="omm-related" aria-labelledby="omm-related-h"><h2 id="omm-related-h">Related guides</h2>' + "".join(parts) + "</aside>"


def build_guides():
    names = sorted(
        n.removeprefix("guides/") for n in git("ls-tree", "--name-only", REF, "guides/").split()
        if n.endswith(".md")
    )
    readme = git("show", f"{REF}:guides/README.md")
    names = [n for n in names if n != "README.md"]
    out_dir = ROOT / "guides"
    for old in out_dir.glob("*/index.html"):
        old.unlink()
    pages = []
    # Short names for the related links: "Connecting OmniMem to Cursor" is "Cursor".
    titles = {}
    for name in names:
        first = git("show", f"{REF}:guides/{name}").splitlines()[0].lstrip("# ").strip()
        short = first.removeprefix("Connecting OmniMem to ").removeprefix("Setting Up OmniMem on ").removeprefix("Running OmniMem in ")
        short = short.removeprefix("a ").removesuffix(" (Linux)").replace("Google Cloud Platform", "Google Cloud").replace("OpenAI Codex CLI", "Codex CLI")
        titles[slug_for(name)] = short

    for name in names:
        md = git("show", f"{REF}:guides/{name}")
        body = rewrite_links(tidy(pandoc(md)), names)
        h1 = re.search(r"<h1[^>]*>(.*?)</h1>", body, re.S)
        title = plain(h1.group(1))
        first_p = re.search(r"<p>(.*?)</p>", body, re.S)
        slug = slug_for(name)
        desc = DESCRIPTIONS.get(slug) or summary(plain(first_p.group(1)))
        updated = guide_date(name)
        published = guide_first_date(name)
        path = f"/guides/{slug}/"
        body = body.replace(h1.group(0), f'<span class="omm-eyebrow">Setup guide</span>\n{h1.group(0)}', 1)
        meta = (
            f'<p class="omm-meta">{byline(updated)}'
            f'<a href="/guides/">All guides</a>'
            f'<a href="{FORGEJO}/src/branch/main/guides/{name}">This guide on Forgejo</a></p>'
        )
        ld = article_ld(path, title, desc, published, updated)
        html_out = page(
            path=path,
            title=f"{title} | OmniMem",
            description=desc,
            body=f'<article class="omm-prose">\n{body}\n{related(slug, titles)}\n{meta}\n</article>',
            crumbs=[("Home", "/"), ("Guides", "/guides/"), (titles[slug], path)],
            ld=ld,
        )
        (out_dir / slug).mkdir(parents=True, exist_ok=True)
        (out_dir / slug / "index.html").write_text(html_out)
        pages.append((path, updated))

    # The index: README's own words, with its two tables as cards.
    intro = pandoc(readme.split("\n## ", 1)[0])
    intro = re.sub(r"<h1[^>]*>.*?</h1>", "", intro, flags=re.S).strip()
    sections = []
    for block in re.findall(r"\n## (.+?)\n(.*?)(?=\n## |\Z)", readme, re.S):
        heading, text = block
        cards = []
        for link_text, target, what in re.findall(r"^\| \[([^\]]+)\]\(([^)]+)\) \| (.+?) \|$", text, re.M):
            what_html = pandoc(what).strip().removeprefix("<p>").removesuffix("</p>")
            cards.append(
                f'<li><a class="omm-card" href="/guides/{slug_for(target)}/">'
                f'<span class="omm-card-title">{html.escape(link_text)}</span>'
                f'<span class="omm-card-text">{what_html}</span></a></li>'
            )
        tail = re.sub(r"^\|.*\|$", "", text, flags=re.M).strip()
        tail_html = rewrite_links(pandoc(tail), names) if tail else ""
        sections.append(f'<h2>{html.escape(heading)}</h2>\n<ul class="omm-cards">{"".join(cards)}</ul>\n{tail_html}')
    newest = max(d for _, d in pages)
    body = (
        '<article class="omm-prose">\n<span class="omm-eyebrow">Setup guides</span>\n'
        "<h1>OmniMem setup guides</h1>\n"
        f"{intro}\n" + "\n".join(sections) +
        '\n<p>New to it all? <a href="/claude-code-memory/">Why Claude Code needs more than CLAUDE.md</a> explains what OmniMem adds, and the homepage has the <a href="/#install">downloads</a>.</p>\n'
        f'<p class="omm-meta">{byline(newest)}</p>\n</article>'
    )
    (out_dir / "index.html").write_text(page(
        path="/guides/",
        title="Setup guides: connect Claude Code, Cursor, Codex and more | OmniMem",
        description="Guides for connecting Claude Code, Cursor, Codex, Copilot and other agents to OmniMem, and for running it on macOS, a Raspberry Pi, AWS, GCP or Docker.",
        body=body,
        crumbs=[("Home", "/"), ("Guides", "/guides/")],
        ld={
            "@type": "CollectionPage",
            "@id": SITE + "/guides/",
            "name": "OmniMem setup guides",
            "url": SITE + "/guides/",
            "dateModified": newest,
            "inLanguage": "en-GB",
            "isPartOf": WEBSITE,
            "about": SOFTWARE,
            "publisher": SQUARECOWS,
        },
    ))
    pages.insert(0, ("/guides/", newest))
    return pages


# ---------------------------------------------------------------- content pages

def front_matter(text):
    m = re.match(r"---\n(.*?)\n---\n", text, re.S)
    meta = dict(line.split(": ", 1) for line in m.group(1).splitlines())
    return meta, text[m.end():]


def build_content():
    pages = []
    for src in sorted((ROOT / "tools" / "content").glob("*.md")):
        meta, md = front_matter(src.read_text())
        body = tidy(pandoc(md))
        if meta.get("eyebrow"):
            body = body.replace("<h1", f'<span class="omm-eyebrow">{html.escape(meta["eyebrow"])}</span>\n<h1', 1)
        path = meta["path"]
        crumbs = None
        ld = None
        robots = "index, follow, max-image-preview:large"
        if path == "/404.html":
            robots = "noindex"
        else:
            crumbs = [("Home", "/"), (meta["crumb"], path)]
            headline = plain(re.search(r"<h1[^>]*>(.*?)</h1>", body, re.S).group(1))
            ld = article_ld(path, headline, meta["description"], meta["published"], meta["updated"])
            body += f'\n<p class="omm-meta">{byline(meta["updated"])}<a href="/guides/">Setup guides</a></p>'
        out = page(path=path, title=meta["title"], description=meta["description"],
                   body=f'<article class="omm-prose">\n{body}\n</article>', crumbs=crumbs, ld=ld, robots=robots)
        target = ROOT / (path.strip("/") if path.endswith(".html") else path.strip("/") + "/index.html")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(out)
        if robots.startswith("index"):
            pages.append((path, meta["updated"]))
    return pages


def build_sitemap(pages):
    # An index.html with uncommitted changes is about to be committed today.
    if git("status", "--porcelain", "--", "index.html").strip():
        home = datetime.date.today().isoformat()
    else:
        home = git("log", "-1", "--format=%cs", "--", "index.html").strip()
    rows = [("/", home)] + pages
    urls = "\n".join(
        f"  <url>\n    <loc>{SITE}{p}</loc>\n    <lastmod>{d}</lastmod>\n  </url>" for p, d in rows
    )
    (ROOT / "sitemap.xml").write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n' + urls + "\n</urlset>\n"
    )


if __name__ == "__main__":
    (ROOT / "assets" / "site.js").write_text(site_js())
    pages = build_guides() + build_content()
    build_sitemap(pages)
    for p, d in pages:
        print(d, p)
