#!/usr/bin/env bash
# Optional Beads SessionStart/PreCompact helper.
# Missing `bd` must not fail session start. Unrelated hooks are not this script.
set -eu
if ! command -v bd >/dev/null 2>&1; then
  exit 0
fi
exec bd prime "$@"
