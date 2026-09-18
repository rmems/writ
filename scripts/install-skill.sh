#!/usr/bin/env bash
# Link this clone of `writ` into agent skill roots so SKILL.md is discoverable.
#
# One real directory, many symlinks. Linux/macOS (and WSL) first.
# Does not register Claude Code hooks; that is the unbuilt `writ install` (#124).
set -euo pipefail

SKILL_NAME="writ"
FORCE=0
CLONE_DIR=""
declare -a ROOT_ARGS=()

usage() {
  cat <<'EOF'
Usage: install-skill.sh [--root ROOT[,ROOT...]] [--force] [--clone-dir PATH]

Symlink this repository into agent skill roots so <root>/writ/SKILL.md resolves.

Options:
  --root ROOT     Skill root(s) to update. Repeatable, or comma-separated.
                  Values: agents, grok, cline, claude, cursor, codex, all
                  Default: agents
  --force         Replace a conflicting path (non-symlink or different symlink).
  --clone-dir P   Repository root that contains SKILL.md.
                  Default: git toplevel of this script, else the parent of scripts/.
                  Override: WRIT_CLONE (legacy: WORKTREES_HIVES_CLONE)
  -h, --help      Show this help.

Examples:
  ./scripts/install-skill.sh
  ./scripts/install-skill.sh --root all
  ./scripts/install-skill.sh --root grok,cline --force
EOF
}

die() {
  echo "install-skill.sh: $*" >&2
  exit 1
}

script_dir() {
  local src="${BASH_SOURCE[0]}"
  (cd "$(dirname "$src")" && pwd)
}

default_clone_dir() {
  local scripts
  scripts="$(script_dir)"
  if git -C "$scripts" rev-parse --show-toplevel >/dev/null 2>&1; then
    git -C "$scripts" rev-parse --show-toplevel
    return
  fi
  (cd "$scripts/.." && pwd)
}

abs_dir() {
  local p=$1
  [[ -d "$p" ]] || die "not a directory: $p"
  (cd "$p" && pwd -P)
}

# Absolute path of a symlink's destination. Missing dest is returned unresolved.
symlink_dest_abs() {
  local link=$1
  local dest dir
  dest="$(readlink "$link")" || return 1
  if [[ "$dest" != /* ]]; then
    dir="$(cd "$(dirname "$link")" && pwd)" || return 1
    dest="$dir/$dest"
  fi
  if [[ -d "$dest" ]]; then
    (cd "$dest" && pwd -P)
  else
    printf '%s\n' "$dest"
  fi
}

points_at_clone() {
  local target=$1
  [[ -L "$target" ]] || return 1
  local dest
  dest="$(readlink "$target")" || return 1
  if [[ "$dest" == "$CLONE_ABS" ]]; then
    return 0
  fi
  dest="$(symlink_dest_abs "$target" || true)"
  [[ "$dest" == "$CLONE_ABS" ]]
}

root_parent() {
  case "$1" in
    agents) printf '%s/.agents/skills\n' "$HOME" ;;
    grok) printf '%s/.grok/skills\n' "$HOME" ;;
    cline) printf '%s/.cline/skills\n' "$HOME" ;;
    claude) printf '%s/.claude/skills\n' "$HOME" ;;
    cursor) printf '%s/.cursor/skills\n' "$HOME" ;;
    codex) printf '%s/.codex/skills\n' "$HOME" ;;
    *) return 1 ;;
  esac
}

root_already_selected() {
  local needle=$1
  local existing
  for existing in "${SELECTED_ROOTS[@]+"${SELECTED_ROOTS[@]}"}"; do
    [[ "$existing" == "$needle" ]] && return 0
  done
  return 1
}

append_root() {
  local name=$1
  if root_already_selected "$name"; then
    return 0
  fi
  SELECTED_ROOTS+=("$name")
}

trim() {
  local value=$1
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  printf '%s\n' "$value"
}

expand_roots() {
  SELECTED_ROOTS=()
  local raw part name
  local -a parts
  if [[ ${#ROOT_ARGS[@]} -eq 0 ]]; then
    ROOT_ARGS=(agents)
  fi
  for raw in "${ROOT_ARGS[@]}"; do
    IFS=',' read -r -a parts <<<"$raw"
    for part in "${parts[@]}"; do
      name="$(trim "$part")"
      [[ -n "$name" ]] || continue
      case "$name" in
        all)
          for name in agents grok cline claude cursor; do
            append_root "$name"
          done
          ;;
        agents | grok | cline | claude | cursor | codex)
          append_root "$name"
          ;;
        *)
          die "unknown --root '$name' (agents|grok|cline|claude|cursor|codex|all)"
          ;;
      esac
    done
  done
}

# True if $1 is $2 or a path under $2. Both arguments must be absolute
# physical paths (no trailing slash). The trailing-slash test avoids
# treating /tmp/writ-extra as inside /tmp/writ.
path_is_inside() {
  local inner=$1
  local outer=$2
  [[ "$inner" == "$outer" || "$inner" == "$outer"/* ]]
}

install_one() {
  local name=$1
  local parent target
  parent="$(root_parent "$name")" || die "unknown root: $name"
  target="$parent/$SKILL_NAME"

  if path_is_inside "$parent" "$CLONE_ABS"; then
    die "refusing to create skill root $parent inside this clone"
  fi

  mkdir -p "$parent"

  if [[ -d "$target" && ! -L "$target" ]]; then
    local existing
    existing="$(cd "$target" && pwd -P)"
    if [[ "$existing" == "$CLONE_ABS" ]]; then
      echo "ok: $target is this clone (no symlink needed)"
      return 0
    fi
  fi

  if [[ -L "$target" || -e "$target" ]]; then
    if points_at_clone "$target"; then
      echo "ok: $target already -> $CLONE_ABS"
      return 0
    fi
    if [[ "$FORCE" -ne 1 ]]; then
      die "$target exists and is not a symlink to this clone; pass --force to replace"
    fi
    if path_is_inside "$CLONE_ABS" "$target"; then
      die "refusing to replace $target because it contains this clone"
    fi
    if [[ -d "$target" && ! -L "$target" ]]; then
      local target_phys
      target_phys="$(cd "$target" && pwd -P)"
      if path_is_inside "$CLONE_ABS" "$target_phys"; then
        die "refusing to replace $target because it contains this clone"
      fi
    fi
    rm -rf "$target"
  fi

  ln -sfn "$CLONE_ABS" "$target"
  if [[ ! -f "$target/SKILL.md" ]]; then
    die "install failed: $target/SKILL.md missing after link"
  fi
  echo "linked: $target -> $CLONE_ABS"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --force)
      FORCE=1
      shift
      ;;
    --root)
      [[ -n "${2:-}" ]] || die "--root requires a value"
      ROOT_ARGS+=("$2")
      shift 2
      ;;
    --root=*)
      ROOT_ARGS+=("${1#--root=}")
      shift
      ;;
    --clone-dir)
      [[ -n "${2:-}" ]] || die "--clone-dir requires a path"
      CLONE_DIR="$2"
      shift 2
      ;;
    --clone-dir=*)
      CLONE_DIR="${1#--clone-dir=}"
      shift
      ;;
    *)
      die "unknown argument: $1 (see --help)"
      ;;
  esac
done

if [[ -z "$CLONE_DIR" ]]; then
  if [[ -n "${WRIT_CLONE:-}" ]]; then
    CLONE_DIR="$WRIT_CLONE"
  elif [[ -n "${WORKTREES_HIVES_CLONE:-}" ]]; then
    echo "install-skill.sh: WORKTREES_HIVES_CLONE is deprecated; use WRIT_CLONE" >&2
    CLONE_DIR="$WORKTREES_HIVES_CLONE"
  else
    CLONE_DIR="$(default_clone_dir)"
  fi
fi

CLONE_ABS="$(abs_dir "$CLONE_DIR")"
[[ -f "$CLONE_ABS/SKILL.md" ]] || die "SKILL.md not found in $CLONE_ABS (is --clone-dir the repo root?)"

[[ -n "${HOME:-}" ]] || die "HOME is unset"
[[ -d "$HOME" ]] || die "HOME is not a directory: $HOME"
HOME="$(cd "$HOME" && pwd -P)"

declare -a SELECTED_ROOTS=()
expand_roots
[[ ${#SELECTED_ROOTS[@]} -gt 0 ]] || die "no skill roots selected"

for name in "${SELECTED_ROOTS[@]}"; do
  install_one "$name"
done

echo
echo "Verify:"
for name in "${SELECTED_ROOTS[@]}"; do
  parent="$(root_parent "$name")"
  echo "  test -f '$parent/$SKILL_NAME/SKILL.md' && echo OK ($name)"
done
echo
echo "Next steps:"
echo "  1. Restart the agent (new session) so it rescans skill roots."
echo "  2. List skills, or run: /skills $SKILL_NAME"
echo "Uninstall: remove the symlink(s) above; keep the clone if you still build from it."
echo "This does not run 'writ install' (Claude Code hooks). That command is not built yet (#124)."
