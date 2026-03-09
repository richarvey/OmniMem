# OmniMem

A self-hosted semantic memory system for Claude Code, exposed as an MCP server. OmniMem gives Claude Code persistent, cross-session, cross-project memory backed by Valkey (Redis fork) with vector search. It also ingests RSS feeds on a schedule to build passive base knowledge that can surface during conversations.

## Overview

## Quick Start

### Prerequisites
- Docker and Docker Compose

### Setup
```bash
git clone <repo-url> omnimem && cd omnimem
cp .env.example .env
# Edit .env — set VALKEY_PASSWORD and ANTHROPIC_API_KEY
docker compose up -d
```

Verify the server is running by calling the `health` MCP tool.

## Configuration

## MCP Tools Reference

## RSS Configuration

## Memory Lifecycle

## Experience Scoring

## Backup & Restore

## CLAUDE.md Integration

Copy `claude_config/CLAUDE.md` into any project directory to give Claude Code access to OmniMem.

Add the MCP server to your Claude Code config (`~/.claude.json` or project `.mcp.json`):

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse",
      "description": "Persistent semantic memory with lifecycle management and experience scoring"
    }
  }
}
```

For Claude Desktop, add the same config to `claude_desktop_config.json`.

## Accessing from Multiple Machines

## Architecture
