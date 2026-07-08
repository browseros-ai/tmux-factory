#!/usr/bin/env bash
# codex-go.sh — tfmux-coordinated software factory (codex).
#
# Creates an isolated worktree, spawns a DETACHED codex session running $sf-auto
# (design → implement → review → open PR → squash-merge to main), delivers the
# task through the `tfmux` CLI, and binds the caller's pane as the tfmux
# `mediator` so codex PINGS IT when the work is done. Then RETURNS. The caller
# stays on `main`; codex works in the worktree, merges to main on its own, and
# `tfmux send mediator` lands a one-line status in the caller's pane at the finish
# (or on a blocker). Part of tmux-factory-codex-go.
#
# tfmux has NO global current session: routing is keyed by an explicit named
# session, not by cwd or a shared .llm. We derive ONE session per run
# (TFMUX_SESSION=tfmux-<slug>), bind the mediator + agent targets in it (passing
# --session on every call), and export TFMUX_SESSION into codex's shell. codex
# then pings back with `tfmux send mediator --session "$TFMUX_SESSION" ...` (and a
# plain `tfmux send mediator ...` also resolves, since the env var is exported).
#
# We still give the worktree a project .llm via dotllm — that is codex's project
# root / $sf-auto scratch — but it no longer carries the ping; the named tfmux
# session does.
#
# One command does every mechanical step (worktree → trust → .llm → bind mediator →
# spawn → tfmux send → notify) so the orchestrator fires it in a single shot.
set -euo pipefail
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── CONFIG POINT: codex launch command (must be autonomous — detached, no human
# to approve). Default xhigh launches through the user's codexy shell alias.
# Override wholesale with SF_CODEXGO_CMD, or request a lower effort with
# SF_CODEXGO_EFFORT / --effort:  xhigh (default, deepest) · high · medium · low
EFFORT="${SF_CODEXGO_EFFORT:-xhigh}"

usage() {
  cat <<'EOF'
Usage: codex-go.sh [options] <feature...>

tfmux-coordinated: worktree + detached codex running $sf-auto to a squash-merged
PR on main, task delivered via `tfmux send`, and a `tfmux send mediator` ping back
to your pane when it finishes. Then return.

Options:
  --slug <slug>      branch + worktree label -> feat/<slug>  (default: derived from the feature)
  --effort <level>   codex reasoning effort: low|medium|high|xhigh  (default: xhigh via codexy)
  --task-file <path> read the verbatim feature from a file (multi-line, @refs, code blocks)
  --dry-run          print the steps without spawning anything
  -h, --help         show this help

Examples:
  codex-go.sh "add a dark-mode toggle to settings"
  codex-go.sh --slug dark-mode --effort medium "add a dark-mode toggle"
  codex-go.sh --task-file /tmp/feature.md
EOF
}

die() { printf 'codex-go: %s\n' "$1" >&2; exit "${2:-1}"; }

# slugify: lowercase, non-alnum -> dash, squeeze/trim, cap ~6 words.
slugify() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' \
    | sed -E 's/[^a-z0-9]+/-/g; s/-+/-/g; s/^-//; s/-$//' | cut -d- -f1-6
}

has_git_marker_upward() {
  local dir="$1"
  while [ -n "$dir" ]; do
    [ -e "$dir/.git" ] && return 0
    [ "$dir" = "/" ] && break
    dir="$(dirname "$dir")"
  done
  return 1
}

has_explicit_git_env() {
  [ -n "${GIT_DIR:-}" ] || [ -n "${GIT_WORK_TREE:-}" ] || [ -n "${GIT_COMMON_DIR:-}" ]
}

# wtc: create a worktree + feat/<name> branch off current HEAD. Mirrors ~/.zshrc
# (zsh defs aren't visible in a bash script, so we replicate it here).
wtc() { wt switch -c "feat/$1" --base=@ --yes "${@:2}"; }   # --yes: approve project hooks non-interactively

# quarantine_hook_copied_dotllm WT: wt post-create hooks may copy the main
# checkout's dotllm symlink target into the worktree as a real .llm directory.
# dotllm init should own the final link, so move obvious copied scratch/status
# mirrors aside while refusing unknown user-owned .llm content.
quarantine_hook_copied_dotllm() {
  local wt="$1" llm quarantine_root quarantine_dest marker=""
  llm="$wt/.llm"

  [ ! -e "$llm" ] && return 0
  [ -L "$llm" ] && return 0

  if [ ! -d "$llm" ]; then
    die "refusing to replace non-directory $llm before dotllm init; move it aside and retry" 1
  fi

  if [ -e "$llm/.sf" ]; then
    marker=".sf"
  elif find "$llm" -mindepth 1 -maxdepth 1 -type d -name '20[0-9][0-9]-[01][0-9]-[0-3][0-9]' -print -quit | grep -q .; then
    marker="date bucket"
  elif find "$llm" -mindepth 1 -maxdepth 1 -type d -name '[0-9][0-9][0-9][0-9][0-9][0-9]-[0-2][0-9][0-5][0-9]_*' -print -quit | grep -q .; then
    marker="task bucket"
  elif [ ! "$(find "$llm" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
    marker="empty directory"
  fi

  if [ -z "$marker" ]; then
    die "refusing to adopt existing real $llm: it does not look like a copied dotllm scratch mirror. Move it aside if it is hook-generated, then retry." 1
  fi

  quarantine_root="$wt/.llm.quarantine"
  mkdir -p "$quarantine_root"
  quarantine_dest="$quarantine_root/$(date +%Y%m%d-%H%M%S)-$$"
  mv "$llm" "$quarantine_dest"
  printf 'codex-go: quarantined hook-copied .llm (%s) at %s before dotllm init\n' "$marker" "$quarantine_dest" >&2
}

# wait_ready NAME: poll the agent pane until codex's TUI is up (bounded ~60s).
# Best-effort — verified_send's working-check is the real guard, so a timeout here
# is non-fatal (we send anyway and let the retry loop confirm).
wait_ready() {
  local n="$1" i pane
  sleep 3   # let the shell launch the agent before the first capture
  for i in $(seq 1 28); do   # ~56s @ 2s
    pane="$(tmux capture-pane -p -t "$n" 2>/dev/null || true)"
    if printf '%s' "$pane" | grep -qiE 'codex|openai|esc to interrupt|tokens|context left|⏎'; then
      return 0
    fi
    sleep 2
  done
  return 1
}

# verified_send NAME FILE: deliver the prompt in FILE via tfmux (robust bracketed
# paste + Enter into the agent target in TFMUX_SESSION), then confirm codex actually
# STARTED on it; retry if not. Guards a readiness race — if the composer isn't ready,
# the paste can land without being submitted and the $sf-auto loop never starts
# (observed: a run sat idle, codex reported "I haven't run any commands yet", zero
# commits). tfmux send handles the paste robustly; this loop confirms work began and
# re-sends if it did not. $TFMUX_SESSION is set in the main flow before this is called.
verified_send() {
  local n="$1" f="$2" i
  for i in 1 2 3 4 5; do
    tfmux send "$n" --session "$TFMUX_SESSION" --text "$(cat "$f")" >/dev/null 2>&1 || true
    sleep 5
    if tmux capture-pane -p -t "$n" 2>/dev/null | grep -qiE 'esc to interrupt|working \(|Worked for|tokens used|thinking'; then
      return 0
    fi
    tmux send-keys -t "$n" C-u >/dev/null 2>&1 || true   # clear any half-typed composer line before retry
  done
  return 1
}

# assert_worktree_ready WT: hard precondition for the worktree-first invariant. We
# spawn codex with its cwd already inside the worktree (tmux new-session -c "$WT"),
# so the worktree MUST already exist and have its .llm initialized before we launch
# — otherwise codex would come up in a missing/uninitialized directory (or, worse,
# the main checkout). This runs immediately before the spawn: if the steps above ever
# get reordered so codex would start first, it aborts loudly instead of failing
# silently. (Ping routing is by the tfmux session, but $sf-auto still needs the
# worktree .llm as its project root, so the precondition stands.)
assert_worktree_ready() {
  local wt="$1"
  [ -n "$wt" ] || die "worktree-first invariant violated: no worktree path resolved before launch" 1
  [ -d "$wt" ] || die "worktree-first invariant violated: worktree '$wt' does not exist before launch (create it before spawning the agent)" 1
  [ -e "$wt/.llm" ] || [ -L "$wt/.llm" ] || die "worktree-first invariant violated: '$wt/.llm' not initialized before launch (run dotllm init before spawning the agent)" 1
}

init_dotllm_best_effort() {
  local dir="$1"
  if ! command -v dotllm >/dev/null 2>&1; then
    printf 'codex-go: NOTE - dotllm not found; continuing without .llm init in %s\n' "$dir" >&2
    return 0
  fi
  dotllm trust "$dir" >/dev/null 2>&1 || true
  if ! ( cd "$dir" && dotllm init -q >/dev/null 2>&1 ); then
    printf 'codex-go: NOTE - dotllm init skipped in %s (plain directory unsupported or init failed)\n' "$dir" >&2
  fi
}

resolve_cleanup_helper() {
  local c
  for c in \
    "$SELF_DIR/factory-cleanup.sh" \
    "$SELF_DIR/../../tmux-factory-codex-go/scripts/factory-cleanup.sh" \
    "$HOME/.claude/skills/tmux-factory-codex-go/scripts/factory-cleanup.sh" \
    "$HOME/.codex/skills/tmux-factory-codex-go/scripts/factory-cleanup.sh" \
    "$HOME/.config/skl/library/skills/tmux-factory/tmux-factory-codex-go/scripts/factory-cleanup.sh"; do
    [ -x "$c" ] && { printf '%s/%s' "$(cd "$(dirname "$c")" && pwd)" "$(basename "$c")"; return 0; }
  done
  return 1
}

if [ "${SF_CODEX_TEST_HELPERS:-0}" = 1 ]; then
  return 0 2>/dev/null || exit 0
fi

SLUG="" FEATURE="" TASK_FILE="" DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --slug)      [ $# -ge 2 ] || die "--slug needs a value" 2; SLUG="$2"; shift 2 ;;
    --effort)    [ $# -ge 2 ] || die "--effort needs a value" 2; EFFORT="$2"; shift 2 ;;
    --task-file) [ $# -ge 2 ] || die "--task-file needs a value" 2; TASK_FILE="$2"; shift 2 ;;
    --dry-run)   DRY=1; shift ;;
    -h|--help)   usage; exit 0 ;;
    --)          shift; FEATURE="$*"; break ;;
    -*)          usage >&2; die "unknown flag: $1" 2 ;;
    *)           FEATURE="$*"; break ;;
  esac
done

if [ -n "$TASK_FILE" ]; then
  [ -f "$TASK_FILE" ] || die "task file not found: $TASK_FILE" 2
  FEATURE="$(cat "$TASK_FILE")"
fi
[ -n "$FEATURE" ] || { usage >&2; die "feature required (positional <feature...> or --task-file)" 2; }

# slug: explicit (normalized) or derived from the feature's first line
[ -n "$SLUG" ] || SLUG="$(slugify "$(printf '%s' "$FEATURE" | sed -n '1p')")"
SLUG="$(slugify "$SLUG")"
[ -n "$SLUG" ] || die "could not derive a slug from the feature; pass --slug" 2

BRANCH="feat/$SLUG"
NAME="sf_$(printf '%s' "$SLUG" | tr '-' '_')_codex"   # sf names: only sf_ + [a-z0-9_]
# tfmux session: deterministic, path-safe (slug is already [a-z0-9-]). One session
# per run, so concurrent launches never collide. Override with SF_TFMUX_SESSION.
TFMUX_SESSION="${SF_TFMUX_SESSION:-tfmux-$SLUG}"
AGENT_SHELL="${SF_AGENT_SHELL:-${SHELL:-/bin/zsh}}"
if [ -n "${SF_CODEXGO_CMD:-}" ]; then
  CODEX_CMD="$SF_CODEXGO_CMD"
else
  case "$EFFORT" in
    low|medium|high|xhigh) ;;
    *) die "invalid --effort '$EFFORT' (use low|medium|high|xhigh)" 2 ;;
  esac
  if [ "$EFFORT" = "xhigh" ]; then
    CODEX_CMD="codexy"
  else
    CODEX_CMD="codex --ask-for-approval never --sandbox danger-full-access -c model_reasoning_effort=$EFFORT"
  fi
fi

# ping-back needs tmux (tfmux bind --here records this pane via TMUX_PANE). The
# orchestrator is normally in tmux; if not, we warn and skip the ping (tfmux has no
# pane to bind). IN_TMUX gates the mediator bind + the notify instruction.
IN_TMUX=0; [ -n "${TMUX:-}" ] && IN_TMUX=1

RUN_DIR="${PWD:-$(pwd)}"
REPO=""
NO_GIT=1
if command -v git >/dev/null 2>&1; then
  if git_root="$(git rev-parse --show-toplevel 2>&1)"; then
    REPO="$git_root"
    NO_GIT=0
  elif has_explicit_git_env || has_git_marker_upward "$RUN_DIR" || ! printf '%s' "$git_root" | grep -qi 'not a git repository'; then
    die "git root detection failed: $git_root" 2
  fi
elif has_explicit_git_env || has_git_marker_upward "$RUN_DIR"; then
  die "missing dependency: git" 127
fi

# preflight (tfmux is the delivery + ping path; python3 resolves the worktree path
# only in git mode; gh is how codex opens + squash-merges the PR in git mode).
if [ "$NO_GIT" = 0 ]; then
  for bin in git wt tmux dotllm tfmux python3 gh; do command -v "$bin" >/dev/null 2>&1 || die "missing dependency: $bin" 127; done
  git remote | grep -q . || die "no git remote set — codex opens + squash-merges a PR, which needs one. Add a remote (e.g. 'gh repo create --source . --remote origin --push') and retry." 2
  if ! CLEAN_HELPER="$(resolve_cleanup_helper)"; then
    CLEAN_HELPER="$HOME/.config/skl/library/skills/tmux-factory/tmux-factory-codex-go/scripts/factory-cleanup.sh"
  fi
else
  for bin in tmux tfmux; do command -v "$bin" >/dev/null 2>&1 || die "missing dependency: $bin" 127; done
fi
command -v "$AGENT_SHELL" >/dev/null 2>&1 || die "missing agent shell: $AGENT_SHELL" 127

if [ "$DRY" = 1 ]; then
  if [ "$NO_GIT" = 1 ]; then
    cat <<EOF
# codex-go: no git repo: running in place, no PR/merge; no worktree isolation, workers edit this directory directly
+ WORK_DIR=$RUN_DIR
+ dotllm trust "\$WORK_DIR" || true             # best-effort; skipped if dotllm is unavailable
+ ( cd "\$WORK_DIR" && dotllm init -q ) || true # best-effort for plain directories
$([ "$IN_TMUX" = 1 ] && echo "+ tfmux bind mediator --here --session $TFMUX_SESSION --role mediator --kind generic   # ping target = this pane, in session $TFMUX_SESSION" || echo "# (not in tmux → cannot bind mediator → no ping-back this run)")
+ tmux new-session -d -s "$NAME" -e TFMUX_SESSION=$TFMUX_SESSION -c "\$WORK_DIR" -- "$AGENT_SHELL" -ic '$CODEX_CMD'   # spawn detached codex in the current directory, TFMUX_SESSION exported
+ wait_ready $NAME                             # poll until codex TUI is up
+ APANE=\$(tmux list-panes -t $NAME -F '#{pane_id}' | head -1)
+ tfmux bind $NAME --tmux "\$APANE" --session $TFMUX_SESSION --role agent --kind codex
+ verified_send $NAME <prompt>                 # tfmux send plain task + in-place done/blocked ping instruction
+ tfmux attach "$NAME"                         # attach to new tmux window (if in tmux)
session: $NAME   tfmux: $TFMUX_SESSION   workdir: $RUN_DIR   effort: $EFFORT   ping-back: $([ "$IN_TMUX" = 1 ] && echo "yes (tfmux send mediator)" || echo "no (not in tmux)")
EOF
  else
    cat <<EOF
+ wtc $SLUG     # = wt switch -c feat/$SLUG --base=@   (mirrors ~/.zshrc)
+ WT=\$(wt list --format json | resolve path for $BRANCH)
+ quarantine_hook_copied_dotllm "\$WT"         # move hook-copied real .llm aside before init
+ dotllm trust "\$WT"
+ ( cd "\$WT" && dotllm init -q )              # worktree .llm = codex's project root for \$sf-auto
$([ "$IN_TMUX" = 1 ] && echo "+ tfmux bind mediator --here --session $TFMUX_SESSION --role mediator --kind generic   # ping target = this pane, in session $TFMUX_SESSION" || echo "# (not in tmux → cannot bind mediator → no ping-back this run)")
+ assert_worktree_ready "\$WT"                 # invariant: worktree exists + .llm initialized BEFORE launch
+ tmux new-session -d -s "$NAME" -e TFMUX_SESSION=$TFMUX_SESSION -c "\$WT" -- "$AGENT_SHELL" -ic '$CODEX_CMD'   # spawn detached codex in the worktree (cwd=\$WT), TFMUX_SESSION exported
+ wait_ready $NAME                             # poll until codex TUI is up
+ APANE=\$(tmux list-panes -t $NAME -F '#{pane_id}' | head -1)
+ tfmux bind $NAME --tmux "\$APANE" --session $TFMUX_SESSION --role agent --kind codex
+ verified_send $NAME <prompt>                 # tfmux send '\$sf-auto <feature> …'$([ "$IN_TMUX" = 1 ] && echo " + 'when done: tfmux send mediator …'")
+ tfmux attach "$NAME"                         # attach to new tmux window (if in tmux)
session: $NAME   tfmux: $TFMUX_SESSION   branch: $BRANCH   effort: $EFFORT   ping-back: $([ "$IN_TMUX" = 1 ] && echo "yes (tfmux send mediator)" || echo "no (not in tmux)")
cleanup after merged PR: $CLEAN_HELPER --session $NAME --worktree "\$WT" --branch $BRANCH --repo "$REPO" --pr <PR_URL_OR_NUMBER>
EOF
  fi
  exit 0
fi

if [ "$NO_GIT" = 0 ]; then
  git show-ref --verify --quiet "refs/heads/$BRANCH" && die "branch '$BRANCH' already exists — pass --slug to pick another" 1

  # 1. worktree — wtc (mirrors ~/.zshrc: wt switch -c feat/<slug> --base=@) makes the
  #    branch+worktree off current HEAD; non-interactive (no shell hook) means it does
  #    NOT cd, so the caller stays on main (a subprocess regardless).
  if ! wt_err="$(wtc "$SLUG" 2>&1)"; then
    die "worktree creation failed (wt switch -c feat/$SLUG --base=@ --yes):
$wt_err" 1
  fi
  WT="$(wt list --format json | python3 -c "import sys,json;print(next(w['path'] for w in json.load(sys.stdin) if w.get('kind')=='worktree' and w.get('branch')=='$BRANCH'))")" \
    || die "could not resolve the worktree path for $BRANCH" 1
  [ -n "$WT" ] || die "empty worktree path for $BRANCH" 1

  # 2. prep the worktree: pre-approve (skip codex's trust prompt) + give $sf-auto a
  #    .llm project root in the worktree.
  quarantine_hook_copied_dotllm "$WT"
  dotllm trust "$WT" >/dev/null 2>&1 || true
  ( cd "$WT" && dotllm init -q ) || die "dotllm init failed in $WT" 1
  WORK_DIR="$WT"
else
  WORK_DIR="$RUN_DIR"
  printf 'codex-go: no git repo: running in place, no PR/merge; no worktree isolation, workers edit this directory directly\n' >&2
  init_dotllm_best_effort "$WORK_DIR"
fi

# 3. bind the caller's pane as the tfmux mediator (the ping target) IN this run's
#    session, so codex reaches it later with `tfmux send mediator --session $TFMUX_SESSION`.
#    Idempotent — re-binds the current orchestrator pane on every run. Each run uses
#    its own per-slug session, so concurrent launches never re-point one another's
#    mediator; fire as many as you like.
NOTIFY=0
if [ "$IN_TMUX" = 1 ]; then
  if tfmux bind mediator --here --session "$TFMUX_SESSION" --role mediator --kind generic >/dev/null 2>&1; then
    NOTIFY=1
  else
    printf 'codex-go: WARNING — could not bind mediator (no ping-back this run); continuing\n' >&2
  fi
else
  if [ "$NO_GIT" = 1 ]; then
    printf 'codex-go: NOTE — not in tmux; codex cannot ping back. Check the directory manually.\n' >&2
  else
    printf 'codex-go: NOTE — not in tmux; codex cannot ping back. Check the PR manually.\n' >&2
  fi
fi

# 4. spawn the DETACHED codex session and wait for its composer to come up. The
#    worktree-first invariant is load-bearing here: steps 1-2 already created the
#    worktree and initialized its .llm, and we launch with cwd already inside it
#    (-c "$WT") and TFMUX_SESSION exported (-e) — never in the main checkout with a
#    later cd. assert_worktree_ready turns any future reordering (spawn before the
#    worktree exists/initializes) into a loud abort instead of a wrong-directory launch.
if [ "$NO_GIT" = 0 ]; then
  assert_worktree_ready "$WT"
  if ! tmux new-session -d -s "$NAME" -e TFMUX_SESSION="$TFMUX_SESSION" -c "$WT" -- "$AGENT_SHELL" -ic "$CODEX_CMD"; then
    die "tmux session creation failed for $NAME" 1
  fi
else
  if ! tmux new-session -d -s "$NAME" -e TFMUX_SESSION="$TFMUX_SESSION" -c "$WORK_DIR" -- "$AGENT_SHELL" -ic "$CODEX_CMD"; then
    die "tmux session creation failed for $NAME" 1
  fi
fi
wait_ready "$NAME" || printf 'codex-go: NOTE — codex readiness not confirmed within timeout; sending anyway (verified_send will retry)\n' >&2

# 5. bind the agent's pane into this run's tfmux session for robust delivery. On
#    failure here the session is a zombie (idle, no task, never pings), so kill it
#    before dying — and point at the worktree to clean up.
APANE="$(tmux list-panes -t "$NAME" -F '#{pane_id}' 2>/dev/null | head -1)"
if [ -z "$APANE" ]; then
  tmux kill-session -t "$NAME" 2>/dev/null || true
  if [ "$NO_GIT" = 1 ]; then
    die "could not resolve a pane id for session $NAME (killed it). Directory left at: $WORK_DIR" 1
  else
    die "could not resolve a pane id for session $NAME (killed it). Remove the worktree with: wtr $BRANCH" 1
  fi
fi
if ! tfmux bind "$NAME" --tmux "$APANE" --session "$TFMUX_SESSION" --role agent --kind codex >/dev/null 2>&1; then
  tmux kill-session -t "$NAME" 2>/dev/null || true
  if [ "$NO_GIT" = 1 ]; then
    die "tfmux bind failed for $NAME ($APANE) (killed the session). Directory left at: $WORK_DIR" 1
  else
    die "tfmux bind failed for $NAME ($APANE) (killed the session). Remove the worktree with: wtr $BRANCH" 1
  fi
fi

# 6. build the prompt (verbatim $sf-auto feature + the done-notify instruction when
#    a mediator is bound) and deliver it via tfmux, confirming codex starts.
PROMPT_FILE="$(mktemp -t codexgo-prompt.XXXXXX)"
trap 'rm -f "$PROMPT_FILE"' EXIT
{
  if [ "$NO_GIT" = 1 ]; then
    printf '%s\n\n' "$FEATURE"
    printf 'Do not use the software-factory PR/merge loop for this task: this directory is not a git repo, so there is no PR or merge loop. Work directly in the current directory and make the requested changes here.\n'
    if [ "$NOTIFY" = 1 ]; then
      printf '\n---\n'
      printf 'When you are completely finished with this task — OR you hit a true blocker you cannot resolve — your FINAL action MUST be to notify the mediator by running this exact command (run it verbatim, just replace <status>):\n\n'
      printf "tfmux send mediator --session %s --text '<status>'\n\n" "$TFMUX_SESSION"
      printf '<status> is ONE line, no single quotes: on success  \xe2\x9c\x85 %s done: <one-line summary>  ·  on blocker  \xe2\x9a\xa0\xef\xb8\x8f %s blocked: <one-line reason>. Do not skip this final step.\n' "$SLUG" "$SLUG"
    fi
  else
    printf '$sf-auto %s\n' "$FEATURE"             # literal $sf-auto + the verbatim feature
    if [ "$NOTIFY" = 1 ]; then
      printf '\n---\n'
      printf 'When you are completely finished — the PR is squash-merged to main, OR you hit a true blocker you cannot resolve — your FINAL action MUST be to notify the mediator by running this exact command (run it verbatim, just replace <status>):\n\n'
      printf "tfmux send mediator --session %s --text '<status>'\n\n" "$TFMUX_SESSION"
      printf '<status> is ONE line, no single quotes: on success  \xe2\x9c\x85 %s merged: <PR url>  ·  on blocker  \xe2\x9a\xa0\xef\xb8\x8f %s blocked: <one-line reason>. Do not skip this final step.\n' "$SLUG" "$SLUG"
    fi
  fi
} > "$PROMPT_FILE"

if ! verified_send "$NAME" "$PROMPT_FILE"; then
  if [ "$NO_GIT" = 1 ]; then
    printf 'codex-go: ERROR — prompt delivery was not confirmed for %s after retries\n' "$NAME" >&2
    printf 'codex-go: last pane capture follows:\n' >&2
    tmux capture-pane -p -t "$NAME" 2>/dev/null | tail -40 >&2 || true
    tmux kill-session -t "$NAME" 2>/dev/null || true
    die "killed unstarted session $NAME. Directory is left at $WORK_DIR; retry after inspecting it." 1
  fi
  printf 'codex-go: WARNING — prompt may not have registered for %s; inspect with: tmux capture-pane -p -t %s\n' "$NAME" "$NAME" >&2
fi

# 7. attach to the current tmux session in a new window (best-effort, tfmux-native)
ATTACHED=""
if [ "$IN_TMUX" = 1 ]; then
  if tfmux attach "$NAME" >/dev/null 2>&1; then
    ATTACHED="✓ attached to new window"
  fi
fi

# 8. report the handles
if [ "$NO_GIT" = 0 ]; then
  printf -v CLEANUP_CMD '%q --session %q --worktree %q --branch %q --repo %q --pr <PR_URL_OR_NUMBER>' "$CLEAN_HELPER" "$NAME" "$WT" "$BRANCH" "$REPO"
  if [ "$NOTIFY" = 1 ]; then
    PING_LINE="→ ping-back:    codex 'tfmux send mediator's a status line to THIS pane when done (stay idle to catch it; if none in ~15 min, check the PR below — the ping depends on codex running the final step)"
  elif [ "$IN_TMUX" = 1 ]; then
    PING_LINE="→ no ping-back: mediator bind failed (see warning above) — check the PR manually below"
  else
    PING_LINE="→ no ping-back: not in tmux — codex can't ping; check the PR manually below"
  fi
  cat <<EOF

spawned:  $NAME
worktree: $WT
branch:   $BRANCH
tfmux:    $TFMUX_SESSION
codex is running \$sf-auto to a squash-merged PR on main, unattended.
→ status:       $ATTACHED
$PING_LINE
→ check later:  gh pr list --head $BRANCH --state all     ·   watch: tmux attach -t $NAME
→ after merge:  $CLEANUP_CMD
EOF
else
  if [ "$NOTIFY" = 1 ]; then
    PING_LINE="→ ping-back:    codex 'tfmux send mediator's a done/blocked status line to THIS pane when done (stay idle to catch it; if none in ~15 min, check the directory below — the ping depends on codex running the final step)"
  elif [ "$IN_TMUX" = 1 ]; then
    PING_LINE="→ no ping-back: mediator bind failed (see warning above) — check the directory manually below"
  else
    PING_LINE="→ no ping-back: not in tmux — codex can't ping; check the directory manually below"
  fi
  cat <<EOF

spawned:  $NAME
workdir:  $WORK_DIR
tfmux:    $TFMUX_SESSION
codex is running in-place in this directory. No PR or squash-merge will be created.
→ status:       $ATTACHED
$PING_LINE
→ check later:  cd "$WORK_DIR"       (worker edits this directory directly)   ·   watch: tmux attach -t $NAME
EOF
fi
