#!/usr/bin/env bash
# Exercises claude-go.sh's quarantine_hook_copied_dotllm: copied scratch mirrors
# (the .sf marker, date buckets, task buckets, an empty dir) are moved aside, a
# real symlink (what dotllm init produces) is left untouched, and an unknown
# user-owned real .llm is refused with an actionable error.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/claude-go.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SF_CLAUDE_TEST_HELPERS=1 source "$SCRIPT"

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

assert_mirror_is_quarantined() {
  local marker="$1"
  local wt="$TMP/$marker"
  mkdir -p "$wt/.llm"
  case "$marker" in
    sf) mkdir -p "$wt/.llm/.sf" ;;
    date) mkdir -p "$wt/.llm/2026-06-19" ;;
    task) mkdir -p "$wt/.llm/260619-1539_dashboard_taxonomy_save" ;;
  esac

  quarantine_hook_copied_dotllm "$wt"

  [ ! -e "$wt/.llm" ] || fail "$marker mirror left .llm in place"
  [ -d "$wt/.llm.quarantine" ] || fail "$marker mirror did not create quarantine root"
  [ "$(find "$wt/.llm.quarantine" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" = 1 ] \
    || fail "$marker mirror did not move exactly one directory into quarantine"
}

assert_symlink_is_left_alone() {
  local wt="$TMP/symlink"
  mkdir -p "$TMP/shared" "$wt"
  ln -s "$TMP/shared" "$wt/.llm"

  quarantine_hook_copied_dotllm "$wt"

  [ -L "$wt/.llm" ] || fail "symlink .llm was replaced"
  [ ! -e "$wt/.llm.quarantine" ] || fail "symlink .llm created quarantine"
}

assert_unknown_real_dir_blocks() {
  local wt="$TMP/unknown"
  mkdir -p "$wt/.llm"
  printf 'keep me\n' >"$wt/.llm/user-notes.md"

  if ( quarantine_hook_copied_dotllm "$wt" ) 2>"$wt/error.log"; then
    fail "unknown real .llm was accepted"
  fi

  [ -d "$wt/.llm" ] || fail "unknown real .llm was moved"
  grep -q 'does not look like a copied dotllm scratch mirror' "$wt/error.log" \
    || fail "unknown real .llm error was not actionable"
}

assert_mirror_is_quarantined sf
assert_mirror_is_quarantined date
assert_mirror_is_quarantined task
assert_symlink_is_left_alone
assert_unknown_real_dir_blocks

printf 'ok - claude-go dotllm quarantine\n'
