#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: ./install.sh [--force]

Installs tfmux with cargo and copies packaged tmux-factory skills into
$HOME/.claude/skills. Existing skill directories are skipped unless --force is
provided.
USAGE
}

force=0
for arg in "$@"; do
  case "$arg" in
    --force)
      force=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
skills_src="$script_dir/skills"
skills_dest="$HOME/.claude/skills"

missing_required=0
if ! command -v tmux >/dev/null 2>&1; then
  echo "error: tmux is required but was not found on PATH." >&2
  echo "       Install tmux with your system package manager, then rerun ./install.sh." >&2
  missing_required=1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required but was not found on PATH." >&2
  echo "       Install Rust with: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  missing_required=1
fi

if [[ "$missing_required" -ne 0 ]]; then
  exit 1
fi

if ! command -v claude >/dev/null 2>&1; then
  echo "warn: claude CLI not found; tmux-factory-claude-* skills need Claude Code." >&2
  echo "      Install Claude Code: https://docs.anthropic.com/en/docs/claude-code/setup" >&2
fi

if ! command -v codex >/dev/null 2>&1; then
  echo "warn: codex CLI not found; tmux-factory-codex-go needs the Codex CLI." >&2
  echo "      Install Codex CLI: https://github.com/openai/codex" >&2
fi

if [[ ! -d "$skills_src" ]]; then
  echo "error: packaged skills directory not found: $skills_src" >&2
  exit 1
fi

echo "Installing tfmux with cargo..."
(cd "$script_dir" && cargo install --path .)

cargo_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
tfmux_candidate="$cargo_root/bin/tfmux"
if [[ -x "$tfmux_candidate" ]]; then
  tfmux_bin="$tfmux_candidate"
elif command -v tfmux >/dev/null 2>&1; then
  tfmux_bin="$(command -v tfmux)"
else
  tfmux_bin="$tfmux_candidate"
fi
echo "tfmux binary: $tfmux_bin"

mkdir -p "$skills_dest"

skill_names=(
  tmux-factory-claude-go
  tmux-factory-claude-opus-go
  tmux-factory-codex-go
)

for skill_name in "${skill_names[@]}"; do
  src="$skills_src/$skill_name"
  dest="$skills_dest/$skill_name"

  if [[ ! -d "$src" ]]; then
    echo "error: packaged skill missing: $src" >&2
    exit 1
  fi

  if [[ -e "$dest" || -L "$dest" ]]; then
    if [[ "$force" -eq 1 ]]; then
      echo "warn: replacing existing skill: $dest" >&2
      rm -rf "$dest"
    else
      echo "warn: skill already exists, skipping: $dest" >&2
      continue
    fi
  fi

  cp -R "$src" "$dest"
  echo "installed skill: $dest"
done

cat <<'NEXT'

Install complete.

Try it now:
  ./demo.sh

From any Claude Code session inside tmux:
  /tmux-factory-codex-go <task>
  /tmux-factory-claude-go <task>
NEXT
