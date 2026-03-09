#!/bin/sh
# Quick health check — call the health MCP tool and print status
curl -s http://localhost:${MCP_PORT:-8765}/health || echo "MCP server not responding"
