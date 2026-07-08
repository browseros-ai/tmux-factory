#!/usr/bin/env bash
# Exercises claude-go.sh's readiness detection and verified_send retry loop:
#   - claude_ready_text / claude_started_text classify TUI states correctly, and
#     an idle composer ('>' line) is explicitly NOT treated as "started" even when
#     the surrounding text contains working/esc markers from prior output;
#   - verified_send re-delivers the prompt via `tfmux send` until Claude actually
#     starts, clearing the half-typed composer between attempts, and gives up after
#     the configured number of attempts when the composer stays idle.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/claude-go.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SF_CLAUDE_TEST_HELPERS=1 source "$SCRIPT"

# verified_send routes through `tfmux send --session "$TFMUX_SESSION"`; define it so
# the expansion is valid under the sourced script's `set -u`.
TFMUX_SESSION=tfmux_test

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

assert_ready() {
  claude_ready_text "$1" || fail "expected ready text to match: $1"
}

assert_not_started() {
  ! claude_started_text "$1" || fail "idle composer was treated as started: $1"
}

assert_started() {
  claude_started_text "$1" || fail "expected started text to match: $1"
}

assert_ready "Claude Code
> "
assert_ready "Bypassing Permissions
What can I help you with?"
assert_not_started "Claude Code
> refactor the thinking module"
assert_not_started "Claude Code
> Please run Bash(git status)"
assert_not_started "Claude Code
> Read(src/auth.ts)"
assert_not_started "Claude Code
> reading"
assert_not_started "Claude Code
> include tokens used in the final report"
assert_not_started "Claude Code
> explain Esc to interrupt behavior"
assert_not_started "Claude Code
> Working (5s)"
assert_not_started "│ > Working (5s)"
assert_not_started "Claude Code
> summarize this output:
Esc to interrupt behavior changed"
assert_not_started "Claude Code
> include this phrase:
Working (5s)"
assert_not_started "Claude Code
> mention previous output:
Worked for 12s"
assert_started "Esc to interrupt"
assert_started "Working (5s)"
assert_started "Worked for 12s"
assert_started "✻ Choreographing… (26s · ↓ 1.8k tokens · thought for 2s)"
assert_started "✻ Scurrying… (26s · ↑ 1.7k tokens)"
assert_started "⏺ I have the task and cli.rs. Now let me read the remaining source files."
assert_started "⏺ Reading 4 files…"

WT="$TMP/worktree"
mkdir -p "$WT"
PROMPT="$TMP/prompt.md"
printf 'do the task\n' >"$PROMPT"

sleep() { :; }

send_count=0
clear_count=0
capture_mode=eventual

tfmux() {
  [ "$1" = send ] || fail "unexpected tfmux command: $*"
  send_count=$((send_count + 1))
}

tmux() {
  case "$1" in
    capture-pane)
      if [ "$capture_mode" = eventual ] && [ "$send_count" -ge 3 ]; then
        printf 'Esc to interrupt\n'
      else
        printf 'Claude Code\n> \n'
      fi
      ;;
    send-keys)
      clear_count=$((clear_count + 1))
      ;;
    *)
      fail "unexpected tmux command: $*"
      ;;
  esac
}

SF_CLAUDE_SEND_ATTEMPTS=5 SF_CLAUDE_SEND_SLEEP=0 verified_send sf_test_claude "$PROMPT" \
  || fail "verified_send did not succeed after started output appeared"
[ "$send_count" = 3 ] || fail "expected 3 send attempts, got $send_count"
[ "$clear_count" = 2 ] || fail "expected 2 composer clears before success, got $clear_count"

send_count=0
clear_count=0
capture_mode=idle
if SF_CLAUDE_SEND_ATTEMPTS=2 SF_CLAUDE_SEND_SLEEP=0 verified_send sf_test_claude "$PROMPT"; then
  fail "verified_send succeeded on idle composer output"
fi
[ "$send_count" = 2 ] || fail "expected 2 failed send attempts, got $send_count"
[ "$clear_count" = 2 ] || fail "expected 2 composer clears on failure, got $clear_count"

printf 'ok - claude-go send state\n'
