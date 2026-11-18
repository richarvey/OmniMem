# Security policy

OmniMem holds what your agents have learnt, and it often runs next to code, credentials and private documents, so I take security reports seriously. Thank you for taking the time to send one.

## Reporting a vulnerability

**Please don't open a public issue, discussion or pull request for a vulnerability.**

Report it privately, in whichever of these two ways suits you:

- **Email [security@omnimem.org](mailto:security@omnimem.org).**
- **Use GitHub's private reporting.** Go to the [Security tab](https://github.com/richarvey/OmniMem/security) of the repository, click **Report a vulnerability** and fill in the form. Only the maintainers can see it.

The project's home is on Forgejo, which has no private reporting, so please use one of these rather than a Forgejo issue.

It helps if you can include:

- the version you're running (`omnimem --version`, or the version shown in the settings panel) and how it's installed: desktop app, Docker, `.deb` or `.rpm`, or built from source;
- your platform and anything relevant in your configuration, such as `MCP_HOST`, how authentication is set up, or whether the settings panel is reachable from another machine;
- what an attacker could do, and what they need first (network access, a valid token, a local account, a malicious feed or document);
- steps to reproduce it, or a proof of concept;
- whether you'd like to be credited, and the name to use.

## What happens next

OmniMem is maintained by a small team, so these are what to expect rather than guarantees:

- **Within 5 working days:** I'll confirm I've got your report.
- **Within 14 days:** I'll tell you whether I can reproduce it, how serious I think it is, and what I plan to do.
- **Fixing it:** a fix goes into a new release, with a GitHub security advisory and a line in the [CHANGELOG](CHANGELOG.md). I'll keep you updated along the way, and agree a disclosure date with you. 90 days from your report is the default, sooner once a fix is out.
- **Credit:** you'll be credited in the advisory and the CHANGELOG unless you'd rather not be.

If you don't hear back within 5 working days, please send a reminder the same way you reported it.

## Supported versions

| Version | Security fixes |
| --- | --- |
| 7.0.x, the latest release | Yes |
| 7.1.x, in development | Yes, before it's released |
| 6.x (the Python and Valkey stack) | No. Please move to 7 |

Fixes go into the newest release, so the answer to most reports is to upgrade.

## What's in scope

Anything in this repository and the packages built from it: the `omnimem` binary, the desktop app, the installers, the Docker image and the hooks it installs into agents. The things I most want to hear about are:

- getting to `/mcp`, `/admin` or the OAuth endpoints without the token, session or sign-in they need, or getting round the Host and Origin checks;
- the server listening beyond the machine it's on without authentication, which it's designed to refuse;
- an agent, a feed or a document getting round a step that needs a person: accepting a skill, settling a contradiction or changing a memory's trust;
- memory text ending up somewhere it shouldn't, such as the audit log below `AUDIT_LOG=full`, or text surviving a `forget`;
- a backup, skill bundle or feed that can write outside the data folder, run code or crash the server;
- leaked secrets, or a dependency vulnerability that OmniMem actually reaches.

## Out of scope

- Problems that need someone to already control the machine, the data folder or the configuration file.
- Settings that knowingly turn a protection off, such as `ADMIN_ALLOW_HTTP`, `HUMAN_APPROVAL=agent` or a weak `MCP_AUTH_TOKEN` you chose yourself.
- An agent acting on text it recalled from memory. That's prompt injection in the model, not in OmniMem, unless it gets round one of the steps above that need a person.
- Advisories in dependencies OmniMem can't reach. These are listed, with the reason for each, in [`.cargo/audit.toml`](.cargo/audit.toml). If you think one of them is reachable after all, please do report it.

## How the code is checked

Every push runs `cargo audit` against the RustSec database and a gitleaks scan of the whole history (`scripts/security-check.sh`), alongside formatting, clippy and the test suite. Dependabot watches the dependencies on GitHub.
