#!/bin/sh
# Optional Beads context for Claude session hooks.
# Missing or failing `bd` must not block session start.
# This script does not delete Beads data, task databases, or other hooks.
if command -v bd >/dev/null 2>&1; then
  bd prime || true
fi
exit 0
