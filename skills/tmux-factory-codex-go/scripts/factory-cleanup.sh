#!/usr/bin/env bash
# factory-cleanup.sh - safe post-merge tmux-factory cleanup.
#
# Verifies a merged GitHub PR before removing the feature worktree, then kills the
# exact tmux session only after worktree removal succeeds.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: factory-cleanup.sh --session NAME --worktree WT --branch BRANCH --repo REPO --pr PR [options]

Options:
  --session <name>    tmux session to kill after successful worktree removal
  --worktree <path>   feature worktree root to remove
  --branch <branch>   expected feature branch / PR head branch
  --repo <path>       main checkout root used to update main and remove worktree
  --pr <pr>           GitHub PR number or URL
  --main <branch>     main branch name (default: main)
  --remote <remote>   git remote name (default: origin)
  --dry-run           run read-only gates and print the planned cleanup
  -h, --help          show this help
EOF
}

die() {
  printf 'factory-cleanup: %s\n' "$1" >&2
  exit "${2:-1}"
}

canon() {
  python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$1"
}

require_non_empty() {
  local name="$1" value="$2"
  [ -n "$value" ] || die "missing required $name" 2
}

git_root() {
  local dir="$1" label="$2" root
  root="$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null)" \
    || die "$label is not a readable git checkout: $dir" 1
  canon "$root"
}

repo_has_worktree() {
  local repo_root="$1" wt_root="$2" list line path
  list="$(git -C "$repo_root" worktree list --porcelain)" \
    || die "listing worktrees from repo $repo_root" 1
  while IFS= read -r line; do
    case "$line" in
      worktree\ *)
        path="${line#worktree }"
        [ "$(canon "$path")" = "$wt_root" ] && return 0
        ;;
    esac
  done <<<"$list"
  return 1
}

git_clean_status() {
  local dir="$1" label="$2" status
  status="$(git -C "$dir" status --porcelain --untracked-files=all)" \
    || die "checking $label status in $dir" 1
  if [ -n "$status" ]; then
    printf 'factory-cleanup: %s status:\n%s\n' "$label" "$status" >&2
    die "$label $dir has uncommitted or untracked changes; commit/stash/remove them before cleanup" 1
  fi
}

SESSION="" WT="" BRANCH="" REPO="" PR="" MAIN="main" REMOTE="origin" DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --session)  [ $# -ge 2 ] || die "--session needs a value" 2; SESSION="$2"; shift 2 ;;
    --worktree) [ $# -ge 2 ] || die "--worktree needs a value" 2; WT="$2"; shift 2 ;;
    --branch)   [ $# -ge 2 ] || die "--branch needs a value" 2; BRANCH="$2"; shift 2 ;;
    --repo)     [ $# -ge 2 ] || die "--repo needs a value" 2; REPO="$2"; shift 2 ;;
    --pr)       [ $# -ge 2 ] || die "--pr needs a value" 2; PR="$2"; shift 2 ;;
    --main)     [ $# -ge 2 ] || die "--main needs a value" 2; MAIN="$2"; shift 2 ;;
    --remote)   [ $# -ge 2 ] || die "--remote needs a value" 2; REMOTE="$2"; shift 2 ;;
    --dry-run)  DRY=1; shift ;;
    -h|--help)  usage; exit 0 ;;
    --)         shift; break ;;
    -*)         usage >&2; die "unknown flag: $1" 2 ;;
    *)          usage >&2; die "unexpected argument: $1" 2 ;;
  esac
done

for bin in git gh tmux python3; do
  command -v "$bin" >/dev/null 2>&1 || die "missing dependency: $bin" 127
done

require_non_empty "--session" "$SESSION"
require_non_empty "--worktree" "$WT"
require_non_empty "--branch" "$BRANCH"
require_non_empty "--repo" "$REPO"
require_non_empty "--pr" "$PR"
require_non_empty "--main" "$MAIN"
require_non_empty "--remote" "$REMOTE"

tmux has-session -t "$SESSION" 2>/dev/null \
  || die "tmux session $SESSION is not available" 1

WT_C="$(canon "$WT")"
[ -d "$WT_C" ] || die "--worktree $WT is not a directory" 1
REPO_C="$(canon "$REPO")"
[ -d "$REPO_C" ] || die "--repo $REPO is not a directory" 1

WT_ROOT="$(git_root "$WT_C" "--worktree")"
REPO_ROOT="$(git_root "$REPO_C" "--repo")"

[ "$WT_ROOT" = "$WT_C" ] \
  || die "--worktree must point at the worktree root; got $WT_C, root is $WT_ROOT" 1
[ "$WT_ROOT" != "$REPO_ROOT" ] \
  || die "--repo and --worktree both resolve to $WT_ROOT; pass --repo to the main checkout" 1
repo_has_worktree "$REPO_ROOT" "$WT_ROOT" \
  || die "--worktree $WT_ROOT is not registered under repo $REPO_ROOT; pass --repo to the main checkout for that worktree" 1

git_clean_status "$WT_ROOT" "worktree"

WT_BRANCH="$(git -C "$WT_ROOT" branch --show-current)" \
  || die "checking worktree branch in $WT_ROOT" 1
[ "$WT_BRANCH" = "$BRANCH" ] \
  || die "worktree is on branch ${WT_BRANCH:-<detached>}, not $BRANCH; pass the expected --branch" 1

REPO_BRANCH="$(git -C "$REPO_ROOT" branch --show-current)" \
  || die "checking repo branch in $REPO_ROOT" 1
[ "$REPO_BRANCH" = "$MAIN" ] \
  || die "repo $REPO_ROOT is on branch ${REPO_BRANCH:-<detached>}, not $MAIN; pass --repo to the main checkout" 1

git_clean_status "$REPO_ROOT" "repo main checkout"

pr_json="$( cd "$REPO_ROOT" && gh pr view "$PR" \
  --json state,mergedAt,mergeCommit,headRefName,baseRefName,url )" \
  || die "loading GitHub PR $PR; pass --pr NUMBER_OR_URL" 1

pr_fields="$(printf '%s' "$pr_json" | python3 -c '
import json,sys
d=json.load(sys.stdin)
mc=d.get("mergeCommit") or {}
for v in (d.get("state",""), d.get("mergedAt") or "", d.get("baseRefName",""),
          d.get("headRefName",""), (mc.get("oid") if isinstance(mc,dict) else "") or "",
          d.get("url","")):
    print(v)
')" || die "decoding gh PR JSON for $PR" 1

{ read -r PR_STATE; read -r PR_MERGED_AT; read -r PR_BASE; read -r PR_HEAD; read -r PR_MERGE_COMMIT; read -r PR_URL; } <<<"$pr_fields"

[ "$PR_STATE" = "MERGED" ] \
  || die "PR $PR is ${PR_STATE:-<unknown>}, not MERGED; wait for merge before cleanup" 1
[ -n "$PR_MERGED_AT" ] \
  || die "PR $PR has no mergedAt timestamp; wait for GitHub merge metadata" 1
[ "$PR_BASE" = "$MAIN" ] \
  || die "PR $PR targets ${PR_BASE:-<unknown>}, not $MAIN; inspect PR before cleanup" 1
[ "$PR_HEAD" = "$BRANCH" ] \
  || die "PR $PR head branch is ${PR_HEAD:-<unknown>}, not $BRANCH; inspect PR before cleanup" 1
[ -n "$PR_MERGE_COMMIT" ] \
  || die "PR $PR has no merge commit; wait for GitHub merge metadata" 1

if [ "$DRY" = 1 ]; then
  cat <<EOF
dry run: would clean up "$SESSION"
verified PR $PR is MERGED into $MAIN (merge commit $PR_MERGE_COMMIT)
would fetch $REMOTE $MAIN in $REPO_ROOT
would pull --ff-only $REMOTE $MAIN in $REPO_ROOT
would verify merge commit is on $MAIN, then remove worktree $WT_ROOT
would kill tmux session $SESSION (only if worktree removal succeeds)
note: dry-run validated preconditions and PR metadata, but cannot prove the
post-update merge-commit ancestry without performing the fetch/pull.
EOF
  exit 0
fi

git -C "$REPO_ROOT" fetch "$REMOTE" "$MAIN" \
  || die "fetching $REMOTE $MAIN in $REPO_ROOT" 1
git -C "$REPO_ROOT" pull --ff-only "$REMOTE" "$MAIN" \
  || die "fast-forward pulling $REMOTE $MAIN in $REPO_ROOT; resolve local main state before cleanup" 1
git -C "$REPO_ROOT" merge-base --is-ancestor "$PR_MERGE_COMMIT" "$MAIN" \
  || die "PR merge commit $PR_MERGE_COMMIT is not on $MAIN after update; inspect PR" 1

if git -C "$REPO_ROOT" worktree remove "$WT_ROOT"; then
  tmux kill-session -t "$SESSION" \
    || die "killing tmux session $SESSION after worktree removal" 1
else
  die "worktree removal failed; left tmux session $SESSION alive for inspection" 1
fi

printf 'cleaned up "%s"\n' "$SESSION"
printf 'worktree removed: %s\n' "$WT_ROOT"
printf 'main updated: %s/%s\n' "$REMOTE" "$MAIN"
printf 'pr verified: %s\n' "$PR_URL"
printf 'branch kept: %s\n' "$BRANCH"
