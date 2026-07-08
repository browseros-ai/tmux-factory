#!/usr/bin/env bash
# Fake-binary tests for factory-cleanup.sh safety gates and mutation ordering.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER="$SCRIPT_DIR/factory-cleanup.sh"

fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

STUB="$TMP/stub"
mkdir -p "$STUB"

cat >"$STUB/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
dir=""
if [ "${1:-}" = "-C" ]; then
  dir="$2"
  shift 2
fi
printf 'git' >>"$CALLOG"
[ -n "$dir" ] && printf ' -C %s' "$dir" >>"$CALLOG"
for arg in "$@"; do printf ' %s' "$arg" >>"$CALLOG"; done
printf '\n' >>"$CALLOG"

cmd="${1:-}"
case "$cmd" in
  rev-parse)
    [ "${2:-}" = "--show-toplevel" ] || exit 99
    if [ "$dir" = "$FAKE_WT_DIR" ]; then
      printf '%s\n' "${FAKE_WT_ROOT:-$FAKE_WT_DIR}"
    elif [ "$dir" = "$FAKE_REPO_DIR" ]; then
      printf '%s\n' "${FAKE_REPO_ROOT:-$FAKE_REPO_DIR}"
    else
      printf '%s\n' "$dir"
    fi
    ;;
  status)
    if [ "$dir" = "$FAKE_WT_DIR" ] || [ "$dir" = "${FAKE_WT_ROOT:-$FAKE_WT_DIR}" ]; then
      printf '%s\n' "${FAKE_WT_STATUS:-}"
    else
      printf '%s\n' "${FAKE_REPO_STATUS:-}"
    fi
    ;;
  branch)
    [ "${2:-}" = "--show-current" ] || exit 99
    if [ "$dir" = "$FAKE_WT_DIR" ] || [ "$dir" = "${FAKE_WT_ROOT:-$FAKE_WT_DIR}" ]; then
      printf '%s\n' "${FAKE_WT_BRANCH:-feat/demo}"
    else
      printf '%s\n' "${FAKE_REPO_BRANCH:-main}"
    fi
    ;;
  fetch)
    exit "${FAKE_FETCH_RC:-0}"
    ;;
  pull)
    exit "${FAKE_PULL_RC:-0}"
    ;;
  merge-base)
    exit "${FAKE_ANCESTOR_RC:-0}"
    ;;
  worktree)
    case "${2:-}" in
      list)
        if [ "${FAKE_WORKTREE_LIST+x}" = x ]; then
          printf '%s\n' "$FAKE_WORKTREE_LIST"
        else
          printf 'worktree %s\nHEAD repohead\nbranch refs/heads/main\n\nworktree %s\nHEAD wthead\nbranch refs/heads/feat/demo\n' "$FAKE_REPO_DIR" "$FAKE_WT_DIR"
        fi
        ;;
      remove)
        exit "${FAKE_REMOVE_RC:-0}"
        ;;
      *)
        exit 99
        ;;
    esac
    ;;
  *)
    exit 99
    ;;
esac
EOF
chmod +x "$STUB/git"

cat >"$STUB/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'gh' >>"$CALLOG"
for arg in "$@"; do printf ' %s' "$arg" >>"$CALLOG"; done
printf '\n' >>"$CALLOG"
rc="${FAKE_GH_RC:-0}"
[ "$rc" = 0 ] || exit "$rc"
printf '%s\n' "$FAKE_PR_JSON"
EOF
chmod +x "$STUB/gh"

cat >"$STUB/tmux" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'tmux' >>"$CALLOG"
for arg in "$@"; do printf ' %s' "$arg" >>"$CALLOG"; done
printf '\n' >>"$CALLOG"
case "${1:-}" in
  has-session) exit "${FAKE_HASSESSION_RC:-0}" ;;
  kill-session) exit "${FAKE_KILL_RC:-0}" ;;
  *) exit 99 ;;
esac
EOF
chmod +x "$STUB/tmux"

realpath_py() {
  python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$1"
}

default_pr_json() {
  printf '{"state":"MERGED","mergedAt":"2026-06-29T00:00:00Z","baseRefName":"main","headRefName":"feat/demo","mergeCommit":{"oid":"abc123"},"url":"https://github.com/example/repo/pull/123"}'
}

setup_case() {
  CASE="$TMP/case-$1"
  mkdir -p "$CASE/wt" "$CASE/repo"
  CALLOG="$CASE/calls.log"; OUT="$CASE/out"; ERR="$CASE/err"; : >"$CALLOG"
  export CALLOG
  export FAKE_WT_DIR FAKE_REPO_DIR FAKE_PR_JSON
  FAKE_WT_DIR="$(realpath_py "$CASE/wt")"
  FAKE_REPO_DIR="$(realpath_py "$CASE/repo")"
  FAKE_PR_JSON="$(default_pr_json)"
  unset FAKE_WT_ROOT FAKE_REPO_ROOT FAKE_WT_STATUS FAKE_REPO_STATUS
  unset FAKE_WT_BRANCH FAKE_REPO_BRANCH FAKE_FETCH_RC FAKE_PULL_RC
  unset FAKE_ANCESTOR_RC FAKE_REMOVE_RC FAKE_HASSESSION_RC FAKE_KILL_RC FAKE_GH_RC
  unset FAKE_WORKTREE_LIST
}

run_helper() {
  set +e
  PATH="$STUB:$PATH" "$HELPER" \
    --session sf_demo \
    --worktree "$FAKE_WT_DIR" \
    --branch feat/demo \
    --repo "$FAKE_REPO_DIR" \
    --pr 123 \
    "$@" >"$OUT" 2>"$ERR"
  RC=$?
  set -e
}

run_helper_with_repo() {
  local repo="$1"; shift
  set +e
  PATH="$STUB:$PATH" "$HELPER" \
    --session sf_demo \
    --worktree "$FAKE_WT_DIR" \
    --branch feat/demo \
    --repo "$repo" \
    --pr 123 \
    "$@" >"$OUT" 2>"$ERR"
  RC=$?
  set -e
}

logged() { grep -qF -- "$1" "$CALLOG"; }
not_logged() { ! logged "$1"; }

line_of() {
  local needle="$1" line
  line="$(grep -nF -- "$needle" "$CALLOG" | head -1 | cut -d: -f1)" || true
  [ -n "$line" ] || fail "missing expected call: $needle"
  printf '%s' "$line"
}

assert_order() {
  local a="$1" b="$2" la lb
  la="$(line_of "$a")"; lb="$(line_of "$b")"
  [ "$la" -lt "$lb" ] || fail "expected '$a' before '$b' ($la !< $lb)"
}

expect_success() {
  [ "$RC" -eq 0 ] || fail "$1 expected success, got $RC: $(cat "$ERR")"
}

expect_failure() {
  [ "$RC" -ne 0 ] || fail "$1 expected failure"
}

# 1. dry-run happy
setup_case dry
run_helper --dry-run
expect_success "dry-run"
logged 'tmux has-session -t sf_demo' || fail "dry-run missed session check"
logged 'gh pr view 123 --json state,mergedAt,mergeCommit,headRefName,baseRefName,url' || fail "dry-run missed PR lookup"
not_logged "git -C $FAKE_REPO_DIR fetch origin main" || fail "dry-run fetched main"
not_logged "git -C $FAKE_REPO_DIR pull --ff-only origin main" || fail "dry-run pulled main"
not_logged "git -C $FAKE_REPO_DIR merge-base --is-ancestor abc123 main" || fail "dry-run checked ancestry"
not_logged "git -C $FAKE_REPO_DIR worktree remove $FAKE_WT_DIR" || fail "dry-run removed worktree"
not_logged 'tmux kill-session -t sf_demo' || fail "dry-run killed tmux"
grep -qF 'cannot prove the' "$OUT" || fail "dry-run must state ancestry proof limitation"

# 2. real happy + order
setup_case happy
run_helper
expect_success "happy path"
assert_order 'tmux has-session -t sf_demo' 'gh pr view 123 --json state,mergedAt,mergeCommit,headRefName,baseRefName,url'
assert_order 'gh pr view 123 --json state,mergedAt,mergeCommit,headRefName,baseRefName,url' "git -C $FAKE_REPO_DIR fetch origin main"
assert_order "git -C $FAKE_REPO_DIR fetch origin main" "git -C $FAKE_REPO_DIR pull --ff-only origin main"
assert_order "git -C $FAKE_REPO_DIR pull --ff-only origin main" "git -C $FAKE_REPO_DIR merge-base --is-ancestor abc123 main"
assert_order "git -C $FAKE_REPO_DIR merge-base --is-ancestor abc123 main" "git -C $FAKE_REPO_DIR worktree remove $FAKE_WT_DIR"
assert_order "git -C $FAKE_REPO_DIR worktree remove $FAKE_WT_DIR" 'tmux kill-session -t sf_demo'

# 3. session missing
setup_case missing_session
export FAKE_HASSESSION_RC=1
run_helper
expect_failure "missing session"
logged 'tmux has-session -t sf_demo' || fail "missing-session did not check tmux"
not_logged 'gh pr view' || fail "missing-session looked up PR"
not_logged 'tmux kill-session -t sf_demo' || fail "missing-session killed tmux"

# 4. dirty worktree
setup_case dirty_wt
export FAKE_WT_STATUS=' M file'
run_helper
expect_failure "dirty worktree"
not_logged 'gh pr view' || fail "dirty worktree looked up PR"
not_logged 'tmux kill-session -t sf_demo' || fail "dirty worktree killed tmux"

# 5. dirty main
setup_case dirty_main
export FAKE_REPO_STATUS=' M file'
run_helper
expect_failure "dirty main"
not_logged 'gh pr view' || fail "dirty main looked up PR"
not_logged 'tmux kill-session -t sf_demo' || fail "dirty main killed tmux"

# 6. repo not on main
setup_case wrong_main
export FAKE_REPO_BRANCH=feature
run_helper
expect_failure "repo not on main"
not_logged 'gh pr view' || fail "wrong main looked up PR"
not_logged 'tmux kill-session -t sf_demo' || fail "wrong main killed tmux"

# 7. branch mismatch
setup_case wrong_branch
export FAKE_WT_BRANCH=other
run_helper
expect_failure "branch mismatch"
not_logged 'gh pr view' || fail "branch mismatch looked up PR"
not_logged 'tmux kill-session -t sf_demo' || fail "branch mismatch killed tmux"

# 8. repo/worktree collision
setup_case collision
FAKE_REPO_DIR="$FAKE_WT_DIR"; export FAKE_REPO_DIR
run_helper_with_repo "$FAKE_WT_DIR"
expect_failure "repo/worktree collision"
not_logged 'gh pr view' || fail "collision looked up PR"
not_logged 'tmux kill-session -t sf_demo' || fail "collision killed tmux"

# 9. worktree not root
setup_case nonroot
export FAKE_WT_ROOT
FAKE_WT_ROOT="$(realpath_py "$CASE")"
run_helper
expect_failure "worktree not root"
grep -qF 'must point at the worktree root' "$ERR" || fail "worktree-not-root error missing"
not_logged 'gh pr view' || fail "worktree-not-root looked up PR"
not_logged 'tmux kill-session -t sf_demo' || fail "worktree-not-root killed tmux"

# 10. worktree not registered under repo
setup_case unregistered
export FAKE_WORKTREE_LIST
FAKE_WORKTREE_LIST="worktree $FAKE_REPO_DIR
HEAD repohead
branch refs/heads/main

worktree $CASE/other
HEAD otherhead
branch refs/heads/feat/other"
run_helper
expect_failure "worktree not registered"
grep -qF 'is not registered under repo' "$ERR" || fail "worktree-not-registered error missing"
not_logged 'gh pr view' || fail "worktree-not-registered looked up PR"
not_logged "git -C $FAKE_REPO_DIR fetch origin main" || fail "worktree-not-registered fetched"
not_logged 'tmux kill-session -t sf_demo' || fail "worktree-not-registered killed tmux"

# 11. PR not merged
setup_case pr_open
export FAKE_PR_JSON='{"state":"OPEN","mergedAt":"","baseRefName":"main","headRefName":"feat/demo","mergeCommit":{"oid":"abc123"},"url":"https://github.com/example/repo/pull/123"}'
run_helper
expect_failure "PR not merged"
not_logged "git -C $FAKE_REPO_DIR fetch origin main" || fail "PR open fetched"
not_logged 'tmux kill-session -t sf_demo' || fail "PR open killed tmux"

# 12. PR base mismatch
setup_case pr_base
export FAKE_PR_JSON='{"state":"MERGED","mergedAt":"2026-06-29T00:00:00Z","baseRefName":"develop","headRefName":"feat/demo","mergeCommit":{"oid":"abc123"},"url":"https://github.com/example/repo/pull/123"}'
run_helper
expect_failure "PR base mismatch"
not_logged "git -C $FAKE_REPO_DIR fetch origin main" || fail "base mismatch fetched"
not_logged 'tmux kill-session -t sf_demo' || fail "base mismatch killed tmux"

# 13. PR head mismatch
setup_case pr_head
export FAKE_PR_JSON='{"state":"MERGED","mergedAt":"2026-06-29T00:00:00Z","baseRefName":"main","headRefName":"other","mergeCommit":{"oid":"abc123"},"url":"https://github.com/example/repo/pull/123"}'
run_helper
expect_failure "PR head mismatch"
not_logged "git -C $FAKE_REPO_DIR fetch origin main" || fail "head mismatch fetched"
not_logged 'tmux kill-session -t sf_demo' || fail "head mismatch killed tmux"

# 14. PR missing merge commit
setup_case pr_no_merge_commit
export FAKE_PR_JSON='{"state":"MERGED","mergedAt":"2026-06-29T00:00:00Z","baseRefName":"main","headRefName":"feat/demo","mergeCommit":null,"url":"https://github.com/example/repo/pull/123"}'
run_helper
expect_failure "PR missing merge commit"
not_logged "git -C $FAKE_REPO_DIR fetch origin main" || fail "missing merge commit fetched"
not_logged 'tmux kill-session -t sf_demo' || fail "missing merge commit killed tmux"

# 15. merge commit not on main
setup_case ancestor_fail
export FAKE_ANCESTOR_RC=1
run_helper
expect_failure "ancestor failure"
logged "git -C $FAKE_REPO_DIR fetch origin main" || fail "ancestor failure did not fetch"
logged "git -C $FAKE_REPO_DIR pull --ff-only origin main" || fail "ancestor failure did not pull"
not_logged "git -C $FAKE_REPO_DIR worktree remove $FAKE_WT_DIR" || fail "ancestor failure removed worktree"
not_logged 'tmux kill-session -t sf_demo' || fail "ancestor failure killed tmux"

# 16. remove fails -> no kill
setup_case remove_fail
export FAKE_REMOVE_RC=1
run_helper
expect_failure "remove failure"
logged "git -C $FAKE_REPO_DIR worktree remove $FAKE_WT_DIR" || fail "remove failure did not try remove"
not_logged 'tmux kill-session -t sf_demo' || fail "remove failure killed tmux"

# 17. branch never deleted
setup_case branch_kept
run_helper
expect_success "branch kept"
not_logged 'branch -d' || fail "helper attempted branch -d"
not_logged 'branch -D' || fail "helper attempted branch -D"
not_logged 'push --delete' || fail "helper attempted remote branch delete"
grep -qF 'branch kept: feat/demo' "$OUT" || fail "success output must say branch kept"

printf 'ok - factory-cleanup\n'
