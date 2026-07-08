#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: ./install.sh [--force]

From a checkout, installs tfmux with cargo and copies packaged tmux-factory
skills into $HOME/.claude/skills and $HOME/.codex/skills.

When piped from GitHub with curl, downloads the latest prebuilt tfmux binary and
fetches the packaged skills from the main branch tarball.

Existing skill directories are skipped unless --force is provided.
USAGE
}

REPO_URL="https://github.com/browseros-ai/tmux-factory.git"
RELEASE_DOWNLOAD_URL="https://github.com/browseros-ai/tmux-factory/releases/latest/download"
MAIN_TARBALL_URL="https://codeload.github.com/browseros-ai/tmux-factory/tar.gz/refs/heads/main"

force=0
next_command="tfmux --help"
install_tmp_dir=""
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

skill_dest_roots=(
  "$HOME/.claude/skills"
  "$HOME/.codex/skills"
)

required_skill_names=(
  tmux-factory-claude-go
  tmux-factory-codex-go
)

script_dir=""
script_path="${BASH_SOURCE[0]:-}"
if [[ -n "$script_path" && -e "$script_path" ]]; then
  script_dir="$(cd -- "$(dirname -- "$script_path")" && pwd -P)"
fi

clone_skills_src="$script_dir/skills"

is_clone_mode() {
  [[ -n "$script_dir" && -f "$script_dir/Cargo.toml" && -d "$clone_skills_src" ]]
}

require_command() {
  local name="$1"
  local message="$2"

  if ! command -v "$name" >/dev/null 2>&1; then
    echo "$message" >&2
    return 1
  fi
}

cleanup_tmp_dir() {
  if [[ -n "${install_tmp_dir:-}" ]]; then
    rm -rf "$install_tmp_dir"
  fi
}

check_clone_prereqs() {
  local missing_required=0

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
}

warn_runtime_prereqs() {
  if ! command -v tmux >/dev/null 2>&1; then
    echo "warn: tmux not found; tfmux needs tmux at runtime." >&2
  fi

  if ! command -v claude >/dev/null 2>&1; then
    echo "warn: claude CLI not found; tmux-factory-claude-go needs Claude Code." >&2
    echo "      Install Claude Code: https://docs.anthropic.com/en/docs/claude-code/setup" >&2
  fi

  if ! command -v codex >/dev/null 2>&1; then
    echo "warn: codex CLI not found; tmux-factory-codex-go needs the Codex CLI." >&2
    echo "      Install Codex CLI: https://github.com/openai/codex" >&2
  fi
}

install_skills() {
  local skills_src="$1"
  local skill_paths=()
  local skill_path
  local skill_name
  local required_skill_name
  local skills_dest
  local src
  local dest

  if [[ ! -d "$skills_src" ]]; then
    echo "error: packaged skills directory not found: $skills_src" >&2
    exit 1
  fi

  for required_skill_name in "${required_skill_names[@]}"; do
    if [[ ! -d "$skills_src/$required_skill_name" ]]; then
      echo "error: required packaged skill missing: $skills_src/$required_skill_name" >&2
      exit 1
    fi
  done

  for skill_path in "$skills_src"/tmux-factory-*; do
    if [[ -d "$skill_path" ]]; then
      skill_paths+=("$skill_path")
    fi
  done

  if [[ "${#skill_paths[@]}" -eq 0 ]]; then
    echo "error: no tmux-factory skills found in: $skills_src" >&2
    exit 1
  fi

  for skills_dest in "${skill_dest_roots[@]}"; do
    mkdir -p "$skills_dest"

    for src in "${skill_paths[@]}"; do
      skill_name="$(basename -- "$src")"
      dest="$skills_dest/$skill_name"

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
  done
}

cargo_tfmux_bin() {
  local cargo_root
  local tfmux_candidate

  cargo_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
  tfmux_candidate="$cargo_root/bin/tfmux"
  if [[ -x "$tfmux_candidate" ]]; then
    echo "$tfmux_candidate"
  elif command -v tfmux >/dev/null 2>&1; then
    command -v tfmux
  else
    echo "$tfmux_candidate"
  fi
}

user_home_from_passwd() {
  local user_name="${USER:-}"
  local user_home

  if [[ -z "$user_name" ]]; then
    user_name="$(id -un 2>/dev/null || true)"
  fi
  if [[ -z "$user_name" ]]; then
    return 1
  fi

  if command -v getent >/dev/null 2>&1; then
    user_home="$(getent passwd "$user_name" 2>/dev/null | awk -F: '{print $6; exit}' || true)"
    if [[ -n "$user_home" ]]; then
      echo "$user_home"
      return 0
    fi
  fi

  if command -v dscl >/dev/null 2>&1; then
    user_home="$(dscl . -read "/Users/$user_name" NFSHomeDirectory 2>/dev/null | awk '{print $2; exit}' || true)"
    if [[ -n "$user_home" ]]; then
      echo "$user_home"
      return 0
    fi
  fi

  return 1
}

rustup_home_dir() {
  local user_home

  if [[ -n "${RUSTUP_HOME:-}" && -d "$RUSTUP_HOME" ]]; then
    echo "$RUSTUP_HOME"
    return 0
  fi

  if [[ -d "$HOME/.rustup" ]]; then
    echo "$HOME/.rustup"
    return 0
  fi

  user_home="$(user_home_from_passwd || true)"
  if [[ -n "$user_home" && -d "$user_home/.rustup" ]]; then
    echo "$user_home/.rustup"
    return 0
  fi

  return 1
}

cargo_install() {
  local rustup_home

  if command -v rustup >/dev/null 2>&1; then
    rustup_home="$(rustup_home_dir || true)"
    if [[ -n "$rustup_home" ]] \
      && RUSTUP_HOME="$rustup_home" rustup run stable cargo --version >/dev/null 2>&1; then
      RUSTUP_HOME="$rustup_home" rustup run stable cargo install "$@"
      return
    fi
  fi

  cargo install "$@"
}

asset_for_platform() {
  local os_name="$1"
  local arch_name="$2"

  case "$os_name:$arch_name" in
    Darwin:arm64)
      echo "tfmux-macos-arm64"
      ;;
    Darwin:x86_64)
      echo "tfmux-macos-x86_64"
      ;;
    Linux:x86_64|Linux:amd64)
      echo "tfmux-linux-x86_64"
      ;;
    Linux:aarch64|Linux:arm64)
      echo "tfmux-linux-arm64"
      ;;
    *)
      return 1
      ;;
  esac
}

path_has_dir() {
  local dir="$1"

  case ":$PATH:" in
    *":$dir:"*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

install_release_binary() {
  local asset="$1"
  local install_dir="$HOME/.local/bin"
  local tfmux_bin="$install_dir/tfmux"
  local tmp_tfmux_bin
  local url="$RELEASE_DOWNLOAD_URL/$asset"

  echo "Downloading $asset from latest GitHub Release..."
  mkdir -p "$install_dir"
  tmp_tfmux_bin="$(mktemp "$install_dir/.tfmux.XXXXXX")"
  if ! curl -fsSL "$url" -o "$tmp_tfmux_bin"; then
    rm -f "$tmp_tfmux_bin"
    echo "error: failed to download release asset: $url" >&2
    exit 1
  fi
  chmod +x "$tmp_tfmux_bin"
  mv "$tmp_tfmux_bin" "$tfmux_bin"
  echo "tfmux binary: $tfmux_bin"

  if ! path_has_dir "$install_dir"; then
    echo "warn: $install_dir is not on PATH." >&2
    printf '      Add it with: export PATH="%s:$PATH"\n' "$install_dir" >&2
  fi
}

install_git_binary() {
  echo "No prebuilt tfmux binary is available for this platform."
  if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is required for fallback install, but it was not found on PATH." >&2
    echo "       Install Rust, or use macOS/Linux on arm64/x86_64 for a prebuilt binary." >&2
    exit 1
  fi

  echo "Installing tfmux with cargo from $REPO_URL..."
  cargo_install --git "$REPO_URL"
  echo "tfmux binary: $(cargo_tfmux_bin)"
}

download_main_skills() {
  local tmp_dir="$1"
  local tarball="$tmp_dir/tmux-factory-main.tar.gz"
  local repo_dir

  echo "Downloading packaged skills from main..." >&2
  if ! curl -fsSL "$MAIN_TARBALL_URL" -o "$tarball"; then
    echo "error: failed to download main tarball: $MAIN_TARBALL_URL" >&2
    exit 1
  fi
  tar -xzf "$tarball" -C "$tmp_dir"

  for repo_dir in "$tmp_dir"/tmux-factory-*; do
    if [[ -d "$repo_dir/skills" ]]; then
      echo "$repo_dir/skills"
      return 0
    fi
  done

  echo "error: skills directory not found in downloaded tarball." >&2
  exit 1
}

run_clone_mode() {
  check_clone_prereqs
  warn_runtime_prereqs

  next_command="./demo.sh"
  echo "Install mode: checkout"
  echo "Installing tfmux with cargo..."
  (cd "$script_dir" && cargo_install --path .)
  echo "tfmux binary: $(cargo_tfmux_bin)"

  install_skills "$clone_skills_src"
}

run_curl_mode() {
  local os_name
  local arch_name
  local asset
  local tmp_dir
  local downloaded_skills

  require_command curl "error: curl is required to download tfmux and packaged skills." || exit 1
  require_command tar "error: tar is required to unpack packaged skills." || exit 1
  warn_runtime_prereqs

  echo "Install mode: curl"
  os_name="$(uname -s)"
  arch_name="$(uname -m)"
  echo "Detected platform: $os_name $arch_name"

  asset="$(asset_for_platform "$os_name" "$arch_name" || true)"
  if [[ -n "$asset" ]]; then
    install_release_binary "$asset"
  else
    install_git_binary
  fi

  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/tfmux-install.XXXXXX")"
  install_tmp_dir="$tmp_dir"
  trap cleanup_tmp_dir EXIT

  downloaded_skills="$(download_main_skills "$tmp_dir")"
  install_skills "$downloaded_skills"
}

if is_clone_mode; then
  run_clone_mode
else
  run_curl_mode
fi

cat <<NEXT

Install complete.

Try it now:
  $next_command

From any Claude Code session inside tmux:
  /tmux-factory-codex-go <task>
  /tmux-factory-claude-go <task>
NEXT
