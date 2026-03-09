#!/bin/sh
# Usage: ./scripts/restore_backup.sh <backup_filename>
# Triggers restore_from_file via the MCP tool with dry_run=False
echo "Restoring from backup: $1"
echo "Run restore_from_file tool in Claude with filename='$1' and dry_run=False"
