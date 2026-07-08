#!/usr/bin/env bash
# test-launch-order.sh — guards the worktree-first invariant for claude-go.sh:
# the worktree must be created and its .llm initialized BEFORE Claude is spawned,
# and the agent must start with its cwd already inside the worktree (tmux
# new-session -c "$WT"), never in the main checkout with a later cd.
#
# Each check below fails if a future edit moves the spawn ahead of worktree
# creation/init, drops the cwd, or weakens the runtime guard.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/claude-go.sh"

fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

# first line number whose text contains the fixed string $1. We anchor on the REAL
# launch flow; the dry-run preview lines escape the worktree var (\$WT) and differ,
# so these fixed strings never collide with the heredoc.
line_of() {
  local n
  n="$(grep -nF -- "$1" "$SCRIPT" | head -1 | cut -d: -f1)" || true
  [ -n "$n" ] || fail "anchor not found in claude-go.sh: $1"
  printf '%s' "$n"
}

# ── 1. real-flow ordering: worktree → quarantine → dotllm init → guard → spawn ──
wt_line="$(line_of 'wt_err="$(wtc')"
quar_line="$(line_of 'quarantine_hook_copied_dotllm "$WT"')"
init_line="$(line_of 'dotllm init -q ) || die')"
guard_line="$(line_of 'assert_worktree_ready "$WT"')"
spawn_line="$(line_of 'if ! tmux new-session')"

[ "$wt_line"    -lt "$quar_line"  ] || fail "worktree creation must precede .llm quarantine ($wt_line !< $quar_line)"
[ "$quar_line"  -lt "$init_line"  ] || fail "quarantine must precede dotllm init ($quar_line !< $init_line)"
[ "$init_line"  -lt "$guard_line" ] || fail "dotllm init must precede the worktree-ready guard ($init_line !< $guard_line)"
[ "$guard_line" -lt "$spawn_line" ] || fail "the worktree-ready guard must run before the agent spawn ($guard_line !< $spawn_line)"
[ "$wt_line"    -lt "$spawn_line" ] || fail "worktree must be created before the agent is spawned ($wt_line !< $spawn_line)"

# ── 2. the real spawn sets cwd to the worktree (-c "$WT") ──────────────────────
spawn_text="$(sed -n "${spawn_line}p" "$SCRIPT")"
printf '%s' "$spawn_text" | grep -qF -- '-c "$WT"' \
  || fail "tmux new-session must launch the agent with cwd=\$WT (missing -c \"\$WT\"): $spawn_text"

# ── 3. behavioral guard: assert_worktree_ready enforces the precondition ───────
SF_CLAUDE_TEST_HELPERS=1 source "$SCRIPT"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# empty path, missing dir, and dir-without-.llm are all the "agent would start
# before the worktree is ready" regression — each must abort.
if ( assert_worktree_ready "" )         2>/dev/null; then fail "guard accepted an empty worktree path"; fi
if ( assert_worktree_ready "$TMP/nope" ) 2>/dev/null; then fail "guard accepted a missing worktree dir"; fi
mkdir -p "$TMP/wt"
if ( assert_worktree_ready "$TMP/wt" )  2>/dev/null; then fail "guard accepted a worktree with no initialized .llm"; fi

# a real .llm directory and a symlinked .llm (what dotllm init actually produces)
# are both "initialized" → accept.
mkdir -p "$TMP/wt/.llm"
( assert_worktree_ready "$TMP/wt" ) || fail "guard rejected a worktree whose .llm is a real directory"
rm -rf "$TMP/wt/.llm"; mkdir -p "$TMP/shared"; ln -s "$TMP/shared" "$TMP/wt/.llm"
( assert_worktree_ready "$TMP/wt" ) || fail "guard rejected a worktree whose .llm is a symlink"

# ── 4. dry-run preview shows the same ordering (worktree + guard before spawn) ──
# Stub the preflight binaries so --dry-run runs anywhere; git/python3 stay real.
# If the preview can't be produced (e.g. preflight still unhappy), skip rather than
# emit a false failure — parts 1-3 already pin the invariant deterministically.
STUB="$(mktemp -d)"
for b in wt dotllm tfmux tmux; do printf '#!/bin/sh\nexit 0\n' >"$STUB/$b"; chmod +x "$STUB/$b"; done
trap 'rm -rf "$TMP" "$STUB"' EXIT
dry=""
dry="$(PATH="$STUB:$PATH" "$SCRIPT" --dry-run --slug probe_launch_order "probe task" 2>/dev/null)" || dry=""
if [ -n "$dry" ]; then
  d_wt="$(printf '%s\n'  "$dry" | grep -nF 'wtc probe'        | head -1 | cut -d: -f1)" || true
  d_gd="$(printf '%s\n'  "$dry" | grep -nF 'assert_worktree_ready' | head -1 | cut -d: -f1)" || true
  d_sp="$(printf '%s\n'  "$dry" | grep -nF 'tmux new-session' | head -1 | cut -d: -f1)" || true
  [ -n "$d_wt" ] && [ -n "$d_sp" ] || fail "dry-run preview missing the worktree or spawn step"
  [ "$d_wt" -lt "$d_sp" ] || fail "dry-run preview shows the agent spawn before worktree creation ($d_wt !< $d_sp)"
  [ -n "$d_gd" ] && [ "$d_gd" -lt "$d_sp" ] || fail "dry-run preview missing the worktree-ready guard before the spawn"
  printf '%s\n' "$dry" | grep -F 'tmux new-session' | grep -qF -- '-c "$WT"' \
    || fail "dry-run spawn preview does not set cwd to the worktree (-c \"\$WT\")"
else
  printf 'note: claude-go --dry-run unavailable in this environment; skipped dry-run ordering check\n'
fi

# ── 5. auto-attach uses the tfmux-native `tfmux attach`, not a helper script ────
# The launcher best-effort opens the spawned session in a new tmux window via the
# Rust `tfmux attach` subcommand. Pin both the real flow and the dry-run preview,
# and forbid shelling out to an external attach helper script, so a future edit
# can't silently revert the auto-attach path.
grep -qF -- 'if tfmux attach "$NAME"' "$SCRIPT" \
  || fail "real flow must auto-attach via 'tfmux attach \"\$NAME\"'"
grep -qF -- '+ tfmux attach "$NAME"' "$SCRIPT" \
  || fail "dry-run preview must show 'tfmux attach \"\$NAME\"'"
if grep -qF -- 'attach.sh' "$SCRIPT"; then
  fail "launcher must use tfmux-native attach, not an external attach helper script"
fi

# ── 6. cleanup guidance points at the bundled factory-cleanup.sh helper ───────
grep -qF -- 'factory-cleanup.sh' "$SCRIPT" \
  || fail "launcher must print a factory-cleanup.sh cleanup command template"
grep -qF -- '--session' "$SCRIPT" \
  || fail "cleanup command must pass --session"
if grep -qF -- 'tfmux detach' "$SCRIPT"; then
  fail "launcher must not reference the reverted 'tfmux detach' command"
fi
STALE_HOME="$TMP/stale-home"
STALE_HELPER="$STALE_HOME/.config/skl/library/skills/tmux-factory/tmux-factory-codex-go/scripts/factory-cleanup.sh"
EXPECTED_HELPER="$(cd "$SCRIPT_DIR/../../tmux-factory-codex-go/scripts" && pwd)/factory-cleanup.sh"
mkdir -p "$(dirname "$STALE_HELPER")"
printf '#!/bin/sh\nprintf stale-helper\\n\n' >"$STALE_HELPER"
chmod +x "$STALE_HELPER"
cleanup_dry="$(HOME="$STALE_HOME" PATH="$STUB:$PATH" "$SCRIPT" --dry-run --slug cleanup_helper_order "probe task" 2>/dev/null)" || cleanup_dry=""
[ -n "$cleanup_dry" ] || fail "dry-run unavailable while checking cleanup helper precedence"
printf '%s\n' "$cleanup_dry" | grep -F 'cleanup after merged PR:' | grep -qF -- "$EXPECTED_HELPER --session" \
  || fail "cleanup command must prefer the bundled sibling helper over a stale global source helper"

printf 'ok - claude-go launch order (worktree-first invariant)\n'
