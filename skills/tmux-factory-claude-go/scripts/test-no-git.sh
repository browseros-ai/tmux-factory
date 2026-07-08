#!/usr/bin/env bash
# test-no-git.sh — verifies claude-go.sh degrades to an in-place launch outside
# git: no worktree/branch, cwd is the firing directory, dotllm is best-effort, and
# tfmux still delivers the prompt + ping instruction.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/claude-go.sh"

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
WORK="$TMP/plain-dir"
STUB="$TMP/bin"
LOG="$TMP/fake.log"
mkdir -p "$WORK" "$STUB" "$TMP/home"

if git -C "$WORK" rev-parse --show-toplevel >/dev/null 2>&1; then
  fail "test temp directory unexpectedly sits inside a git repo: $WORK"
fi

cat >"$STUB/tmux" <<'SH'
#!/usr/bin/env bash
{
  printf 'tmux'
  for arg in "$@"; do printf ' <%s>' "$arg"; done
  printf '\n'
} >>"$LOG"
case "${1:-}" in
  capture-pane) printf 'Claude Code\n✻ 1 tokens\n' ;;
  list-panes) printf '%%1\n' ;;
esac
exit 0
SH
chmod +x "$STUB/tmux"

cat >"$STUB/tfmux" <<'SH'
#!/usr/bin/env bash
{
  printf 'tfmux'
  for arg in "$@"; do printf ' <%s>' "$arg"; done
  printf '\n'
} >>"$LOG"
exit 0
SH
chmod +x "$STUB/tfmux"

cat >"$STUB/dotllm" <<'SH'
#!/usr/bin/env bash
{
  printf 'dotllm'
  for arg in "$@"; do printf ' <%s>' "$arg"; done
  printf '\n'
} >>"$LOG"
exit 7
SH
chmod +x "$STUB/dotllm"

printf '#!/usr/bin/env bash\nexit 0\n' >"$STUB/sleep"
chmod +x "$STUB/sleep"

TEST_PATH="$STUB:/usr/bin:/bin:/usr/sbin:/sbin"
COMMON_ENV=(
  "PATH=$TEST_PATH"
  "HOME=$TMP/home"
  "TMUX=test-tmux"
  "TMUX_PANE=%9"
  "LOG=$LOG"
  "SF_AGENT_SHELL=/bin/sh"
  "SF_CLAUDEGO_CMD=true"
  "GIT_DIR="
  "GIT_WORK_TREE="
  "GIT_COMMON_DIR="
)

BROKEN_GIT_BIN="$TMP/broken-bin"
BROKEN_GIT_WORK="$TMP/broken-git"
mkdir -p "$BROKEN_GIT_BIN" "$BROKEN_GIT_WORK/.git"
cat >"$BROKEN_GIT_BIN/git" <<'SH'
#!/usr/bin/env bash
printf 'fatal: detected dubious ownership in repository\n' >&2
exit 128
SH
chmod +x "$BROKEN_GIT_BIN/git"

if broken_out="$(
  cd "$BROKEN_GIT_WORK"
  env \
    "PATH=$BROKEN_GIT_BIN:$TEST_PATH" \
    "HOME=$TMP/home" \
    "TMUX=test-tmux" \
    "TMUX_PANE=%9" \
    "LOG=$LOG" \
    "SF_AGENT_SHELL=/bin/sh" \
    "SF_CLAUDEGO_CMD=true" \
    "$SCRIPT" --dry-run --slug broken-git "probe" 2>&1
)"; then
  fail "broken git metadata silently fell back to no-git mode"
fi
printf '%s\n' "$broken_out" | grep -qF 'git root detection failed' \
  || fail "broken git metadata did not produce a clear git detection error"

BROKEN_ENV_WORK="$TMP/broken-git-env"
mkdir -p "$BROKEN_ENV_WORK"
if broken_env_out="$(
  cd "$BROKEN_ENV_WORK"
  env \
    "PATH=$TEST_PATH" \
    "HOME=$TMP/home" \
    "TMUX=test-tmux" \
    "TMUX_PANE=%9" \
    "LOG=$LOG" \
    "SF_AGENT_SHELL=/bin/sh" \
    "SF_CLAUDEGO_CMD=true" \
    "GIT_DIR=$TMP/missing-git-dir" \
    "$SCRIPT" --dry-run --slug broken-git-env "probe" 2>&1
)"; then
  fail "broken GIT_DIR silently fell back to no-git mode"
fi
printf '%s\n' "$broken_env_out" | grep -qF 'git root detection failed' \
  || fail "broken GIT_DIR did not produce a clear git detection error"

dry="$(
  cd "$WORK"
  env "${COMMON_ENV[@]}" "$SCRIPT" --dry-run --slug plain-run "create hello.txt containing hi, then ping" 2>&1
)" || fail "dry-run failed outside git"

printf '%s\n' "$dry" | grep -qF 'no git repo: running in place' \
  || fail "dry-run did not disclose no-git in-place mode"
printf '%s\n' "$dry" | grep -qF 'workers edit this directory directly' \
  || fail "dry-run did not disclose lack of isolation"
if printf '%s\n' "$dry" | grep -qF 'wtc plain-run'; then
  fail "dry-run preview created a worktree outside git"
fi
if printf '%s\n' "$dry" | grep -qF 'branch:'; then
  fail "dry-run preview printed a branch outside git"
fi
if printf '%s\n' "$dry" | grep -qF 'cleanup after merged PR:'; then
  fail "dry-run preview printed cleanup hints outside git"
fi
printf '%s\n' "$dry" | grep -qF -- '-c "$WORK_DIR"' \
  || fail "dry-run preview did not spawn with cwd=\$WORK_DIR"

: >"$LOG"
out="$(
  cd "$WORK"
  env "${COMMON_ENV[@]}" "$SCRIPT" --slug plain-run "create hello.txt containing hi, then ping" 2>&1
)" || fail "launch failed outside git"

printf '%s\n' "$out" | grep -qF 'no git repo: running in place' \
  || fail "launch output did not disclose no-git in-place mode"
printf '%s\n' "$out" | grep -qF 'workers edit this directory directly' \
  || fail "launch output did not disclose direct edits"
grep -qF "tmux <new-session>" "$LOG" \
  || fail "launcher did not spawn a tmux worker"
grep -qF "<-c> <$WORK>" "$LOG" \
  || fail "worker was not spawned with cwd set to the firing directory"
grep -qF "tfmux <bind> <mediator>" "$LOG" \
  || fail "launcher did not bind the mediator"
grep -qF "tfmux <send> <sf_plain_run_claude>" "$LOG" \
  || fail "launcher did not deliver the task through tfmux"
grep -qF 'create hello.txt containing hi, then ping' "$LOG" \
  || fail "tfmux send did not include the verbatim task"
grep -qF 'tfmux send mediator --session tfmux-plain-run --text' "$LOG" \
  || fail "tfmux send did not include the done/blocked ping instruction"
grep -qF "dotllm <trust> <$WORK>" "$LOG" \
  || fail "launcher did not attempt best-effort dotllm trust"

printf 'ok - claude-go no-git mode\n'
