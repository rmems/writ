# Install the `writ` agent skill

This is the **clone + symlink** install for the portable skill in [`SKILL.md`](../SKILL.md). One clone, many skill-root symlinks. It does not require Graphite; plain `git` is enough. `gh` is optional and unused by the installer.

This is **not** `writ install`, the CLI command that writes the `writ` hook block into `.claude/settings.json` (implemented; held back from shared settings pending the [#124](https://github.com/rmems/writ/issues/124) burn-in). Until hooks are registered, enforcement stays opt-in through `writ git-safe` / `writ gh-safe` / `writ worktree` as described in the [README](../README.md).

Second-platform smoke testing that consumes these paths was tracked in [#17](https://github.com/rmems/writ/issues/17) and closed as not planned. The paths below remain the source of truth for a human or later checklist.

## Recommended clone location

Clone once to a stable absolute path you will not move (relative symlinks break if the clone moves):

```bash
git clone https://github.com/rmems/writ.git "$HOME/src/writ"
cd "$HOME/src/writ"
```

Keep the tree wherever you already work; `$HOME/src/writ` is a suggestion, not a requirement. Override the installer with `--clone-dir` or `WRIT_CLONE` if the checkout is elsewhere.

## Quick install

From the clone:

```bash
./scripts/install-skill.sh
```

Default root is the shared agents hub: `~/.agents/skills/writ`. Fan out to every well-known home root this project cares about:

```bash
./scripts/install-skill.sh --root all
```

## Manual install (docs minimum)

```bash
CLONE="$HOME/src/writ"
git clone https://github.com/rmems/writ.git "$CLONE"
mkdir -p "$HOME/.agents/skills" "$HOME/.grok/skills" "$HOME/.cline/skills"
ln -sfn "$CLONE" "$HOME/.agents/skills/writ"
ln -sfn "$CLONE" "$HOME/.grok/skills/writ"
test -f "$HOME/.agents/skills/writ/SKILL.md" && echo OK
```

`ln -sfn` uses an **absolute** clone path so the link survives a later working-directory change. The link target is the **skill directory** (the clone), not a bare `SKILL.md` file. Agents that require the directory name to match frontmatter `name` look for `writ`.

If `<root>/writ` already exists as a **real directory**, GNU/BSD `ln -sfn "$CLONE" <root>/writ` does not replace it — it creates `<root>/writ/writ`. Check with `ls -ld ~/.agents/skills/writ` first, or use `scripts/install-skill.sh`, which refuses that conflict without `--force`.

## Cross-agent skill roots

| Root | Path | Script `--root` | Priority |
| --- | --- | --- | --- |
| Shared agents / skills CLI | `~/.agents/skills/writ` | `agents` (default) | Primary shared hub |
| Cline | `~/.cline/skills/writ` | `cline` | High |
| Grok user | `~/.grok/skills/writ` | `grok` | High |
| Grok project | `<repo>/.grok/skills/writ` | *(manual)* | Optional per-repo |
| Claude Code | `~/.claude/skills/writ` | `claude` | Medium (also scanned by Grok) |
| Cursor | `~/.cursor/skills/writ` | `cursor` | Medium |
| Codex | `~/.codex/skills/writ` | `codex` | Optional |
| OpenCode, Gemini CLI, Kilo, … | host-specific `*/skills/writ` | *(manual)* | Optional |

`--root all` links `agents`, `grok`, `cline`, `claude`, and `cursor`. Add `codex` explicitly if you want it. Per-repo Grok discovery (`<repo>/.grok/skills/writ`) is a local overlay; create it from the target repository when you need it:

```bash
mkdir -p .grok/skills
ln -sfn "$HOME/src/writ" "$PWD/.grok/skills/writ"
```

Grok's usual search order is `./.grok/skills` → repo `.grok/skills` → `~/.grok/skills` → `~/.claude/skills`, plus any `[skills].paths` you configure.

Optional hosts (confirm the live path on that tool before linking): Codex `~/.codex/skills`, OpenCode often `~/.config/opencode/skills`, Gemini CLI `~/.gemini/skills`. Same rule: symlink the clone directory so `<root>/writ/SKILL.md` exists.

## Installer behavior

`scripts/install-skill.sh`:

- Resolves the clone as `--clone-dir`, else `WRIT_CLONE`, else legacy `WORKTREES_HIVES_CLONE`, else `git rev-parse --show-toplevel` from the script, else the parent of `scripts/`.
- Creates parent directories as needed.
- Symlinks the **directory** that contains `SKILL.md` into each selected root as `writ`.
- Is idempotent: a symlink that already points at this clone is a no-op success.
- Refuses to replace a non-symlink or a symlink to a different path unless you pass `--force`.
- Prints verification commands and restart instructions.
- Never auto-updates other agents on the machine; you choose `--root`.

```text
Usage: install-skill.sh [--root agents|grok|cline|claude|cursor|codex|all] [--force] [--clone-dir PATH]
```

`--root` may be repeated or comma-separated (`--root grok --root cline` or `--root grok,cline`).

## Verify discovery

| Check | Command / action |
| --- | --- |
| File present | `test -f ~/.agents/skills/writ/SKILL.md && echo OK` |
| Symlink healthy | `readlink -f ~/.agents/skills/writ` points at the clone |
| Grok | `/skills writ` or the skills list after a new session |
| Cline | skill picker / skills list |
| Claude | restart the session; confirm `writ` is listed |
| Cursor | restart; confirm `~/.cursor/skills/writ/SKILL.md` is listed |

`readlink -f` is GNU; on macOS `readlink` without `-f` prints the stored target, which should be the absolute clone path the installer wrote.

## Uninstall

Remove the symlink(s). Keep the clone if you still build or edit from it.

```bash
rm -f ~/.agents/skills/writ ~/.grok/skills/writ ~/.cline/skills/writ
rm -f ~/.claude/skills/writ ~/.cursor/skills/writ ~/.codex/skills/writ
```

Do not delete the clone unless you intend to. `--force` replaces a conflicting skill-root *entry* (file or foreign directory). It refuses when that path *is* this clone or *contains* this clone, so a nested checkout under `~/.agents/skills/writ/` cannot be `rm -rf`'d by accident.

## Registry publish (optional, later)

Local symlink is enough for v1. If the skill is later published to a skills registry:

```bash
npx skills add rmems/writ
```

Do not treat marketplace publication as required. Manual symlink remains the source of truth if the CLI and these docs ever drift.

## Platform notes

Linux and macOS are the supported install surfaces. On WSL, use the Linux `$HOME` (`~/.agents/skills/...`), not a Windows `%USERPROFILE%` path. Native Windows without WSL can copy the tree into a skill root instead of symlinking; discovery still needs `<root>/writ/SKILL.md`.

## Binary vs skill vs hooks

| What | How | Status |
| --- | --- | --- |
| `writ` binary | `cargo install --path crates/writ` (see README Build), or a GitHub Release artifact | Shipping |
| Agent skill | this document / `scripts/install-skill.sh` | Shipping |
| Claude Code hooks | `writ install` writing `.claude/settings.json` | Implemented; burn-in outstanding ([#124](https://github.com/rmems/writ/issues/124)) |
