#!/usr/bin/env bash
# claude-go.sh — tfmux-coordinated Claude Code session in an isolated worktree.
#
# Creates an isolated worktree, spawns a DETACHED Claude Code session (claudeo),
# delivers the task through the `tfmux` CLI, and binds the caller's pane as the
# tfmux `mediator` so Claude PINGS IT when the work is done. Then RETURNS. The
# caller stays on `main`; Claude works in the worktree, and `tfmux send mediator`
# lands a one-line status in the caller's pane at the finish (or on a blocker).
# Part of tmux-factory-claude-opus-go.
#
# tfmux has NO global current session: routing is keyed by an explicit named
# session, not by cwd or a shared .llm. We derive ONE session per run
# (TFMUX_SESSION=tfmux-<slug>), bind the mediator + agent targets in it (passing
# --session on every call), and export TFMUX_SESSION into Claude's shell. Claude
# then pings back with `tfmux send mediator --session "$TFMUX_SESSION" ...` (and a
# plain `tfmux send mediator ...` also resolves, since the env var is exported).
#
# We still give the worktree a project .llm via dotllm — that is Claude's project
# root / scratch — but it no longer carries the ping; the named tfmux session does.
#
# One command does every mechanical step (worktree → trust → .llm → bind mediator →
# spawn → tfmux send → notify) so you fire it in a single shot and return. This is
# the tfmux-routed counterpart to software-factory-claude-go.
set -euo pipefail
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── CONFIG POINT: Claude launch command (must be autonomous — detached, no human
# to approve). Default max launches through the user's claudeo shell alias.
# Override wholesale with SF_CLAUDEGO_CMD, or request a lower effort with
# SF_CLAUDEGO_EFFORT / --effort: max (default, deepest) · high · medium · low
EFFORT="${SF_CLAUDEGO_EFFORT:-max}"

usage() {
  cat <<'EOF'
Usage: claude-go.sh [options] <task...>

tfmux-coordinated: worktree + detached Claude Code session through claudeo, task
delivered via `tfmux send`, and a `tfmux send mediator` ping back to your pane when
it finishes. Then return.

Options:
  --slug <slug>      branch + worktree label -> feat/<slug>  (default: derived from task)
  --effort <level>   Claude reasoning effort: low|medium|high|max  (default: max via claudeo)
  --task-file <path> read the verbatim task from a file (multi-line, @refs, code blocks)
  --dry-run          print the steps without spawning anything
  -h, --help         show this help

Examples:
  claude-go.sh "refactor the authentication module"
  claude-go.sh --slug auth-refactor --effort medium "refactor auth"
  claude-go.sh --task-file /tmp/task.md
EOF
}

die() { printf 'claude-go: %s\n' "$1" >&2; exit "${2:-1}"; }

# slugify: lowercase, non-alnum -> dash, squeeze/trim, cap ~6 words.
slugify() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' \
    | sed -E 's/[^a-z0-9]+/-/g; s/-+/-/g; s/^-//; s/-$//' | cut -d- -f1-6
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
  printf 'claude-go: quarantined hook-copied .llm (%s) at %s before dotllm init\n' "$marker" "$quarantine_dest" >&2
}

capture_pane() { tmux capture-pane -p -t "$1" 2>/dev/null || true; }

# claude_ready_text PANE: the TUI is up (composer drawn / welcome shown). Used only
# to stop waiting; submission is confirmed separately by claude_started_text.
claude_ready_text() {
  printf '%s' "$1" | grep -qiE 'claude code|for shortcuts|welcome|bypassing permissions|dangerously skip|what can i help|cwd:|^[[:space:]]*>[[:space:]]*$|[[:space:]]>[[:space:]]*$'
}

# claude_started_text PANE: Claude actually started working on the prompt. Current
# Claude TUI builds show active progress with glyph-led lines (`✻ ... tokens ...`)
# and assistant/tool lines (`⏺ ...`) rather than only "Working"/"Esc" text.
# An idle composer line (`>` / `╰─`) still means the paste landed but was NOT
# submitted, so verified_send re-sends.
claude_started_text() {
  if printf '%s\n' "$1" \
    | grep -qiE '^[[:space:]]*(✻ .*(token|thought|[0-9]+s)|⏺[[:space:]])'; then
    return 0
  fi
  if printf '%s\n' "$1" | grep -qE '^[[:space:]]*(│[[:space:]]*)?>|^[[:space:]]*>|^[[:space:]]*╰─'; then
    return 1
  fi
  printf '%s\n' "$1" \
    | grep -qiE '^[[:space:]]*(esc to interrupt|worked for [0-9]|working \([0-9]+)'
}

# wait_ready NAME: poll the agent pane until Claude's TUI is up (bounded ~60s).
# Best-effort — verified_send's started-check is the real guard, so a timeout here
# is non-fatal (we send anyway and let the retry loop confirm).
wait_ready() {
  local n="$1" i pane
  sleep 3   # let the shell launch the agent before the first capture
  for i in $(seq 1 28); do   # ~56s @ 2s
    pane="$(capture_pane "$n")"
    if claude_ready_text "$pane" || claude_started_text "$pane"; then
      return 0
    fi
    sleep 2
  done
  return 1
}

# verified_send NAME FILE: deliver the prompt in FILE via tfmux (robust bracketed
# paste + Enter into the agent target in TFMUX_SESSION), then confirm Claude actually
# STARTED on it; retry if not. Guards a readiness race — if the composer isn't ready,
# the paste can land without being submitted and Claude never starts the task. tfmux
# send handles the paste robustly; this loop confirms work began and re-sends if it
# did not. $TFMUX_SESSION is set in the main flow before this is called.
verified_send() {
  local n="$1" f="$2" i attempts delay pane
  attempts="${SF_CLAUDE_SEND_ATTEMPTS:-5}"
  delay="${SF_CLAUDE_SEND_SLEEP:-5}"
  for i in $(seq 1 "$attempts"); do
    tfmux send "$n" --session "$TFMUX_SESSION" --text "$(cat "$f")" >/dev/null 2>&1 || true
    sleep "$delay"
    pane="$(capture_pane "$n")"
    if claude_started_text "$pane"; then
      return 0
    fi
    tmux send-keys -t "$n" C-u >/dev/null 2>&1 || true   # clear any half-typed composer line before retry
  done
  return 1
}

# assert_worktree_ready WT: hard precondition for the worktree-first invariant. We
# spawn the agent with its cwd already inside the worktree (tmux new-session -c
# "$WT"), so the worktree MUST already exist and have its .llm initialized before
# we launch — otherwise Claude would come up in a missing/uninitialized directory
# (or, worse, the main checkout). This runs immediately before the spawn: if the
# steps above ever get reordered so the agent would start first, it aborts loudly
# instead of failing silently. (The ping routes by the tfmux session, but Claude
# still needs the worktree .llm as its project root, so the precondition stands.)
assert_worktree_ready() {
  local wt="$1"
  [ -n "$wt" ] || die "worktree-first invariant violated: no worktree path resolved before launch" 1
  [ -d "$wt" ] || die "worktree-first invariant violated: worktree '$wt' does not exist before launch (create it before spawning the agent)" 1
  [ -e "$wt/.llm" ] || [ -L "$wt/.llm" ] || die "worktree-first invariant violated: '$wt/.llm' not initialized before launch (run dotllm init before spawning the agent)" 1
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

# validate_agent_command: confirm the launch command resolves in the agent shell
# before we spawn — a detached session can't prompt, so an unresolved claudeo/claude
# would just sit at a dead shell with the task never delivered.
validate_agent_command() {
  [ -n "${SF_CLAUDEGO_CMD:-}" ] && return 0
  if [ "$EFFORT" = "max" ]; then
    "$AGENT_SHELL" -ic 'type claudeo >/dev/null 2>&1' >/dev/null 2>&1 \
      || die "agent shell cannot resolve claudeo; define it there or set SF_CLAUDEGO_CMD" 127
  else
    "$AGENT_SHELL" -ic 'type claude >/dev/null 2>&1' >/dev/null 2>&1 \
      || die "agent shell cannot resolve claude; define it there or set SF_CLAUDEGO_CMD" 127
  fi
}

if [ "${SF_CLAUDE_TEST_HELPERS:-0}" = 1 ]; then
  return 0 2>/dev/null || exit 0
fi

SLUG="" TASK="" TASK_FILE="" DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --slug)      [ $# -ge 2 ] || die "--slug needs a value" 2; SLUG="$2"; shift 2 ;;
    --effort)    [ $# -ge 2 ] || die "--effort needs a value" 2; EFFORT="$2"; shift 2 ;;
    --task-file) [ $# -ge 2 ] || die "--task-file needs a value" 2; TASK_FILE="$2"; shift 2 ;;
    --dry-run)   DRY=1; shift ;;
    -h|--help)   usage; exit 0 ;;
    --)          shift; TASK="$*"; break ;;
    -*)          usage >&2; die "unknown flag: $1" 2 ;;
    *)           TASK="$*"; break ;;
  esac
done

if [ -n "$TASK_FILE" ]; then
  [ -f "$TASK_FILE" ] || die "task file not found: $TASK_FILE" 2
  TASK="$(cat "$TASK_FILE")"
fi
[ -n "$TASK" ] || { usage >&2; die "task required (positional <task...> or --task-file)" 2; }

# slug: explicit (normalized) or derived from the task's first line
[ -n "$SLUG" ] || SLUG="$(slugify "$(printf '%s' "$TASK" | sed -n '1p')")"
SLUG="$(slugify "$SLUG")"
[ -n "$SLUG" ] || die "could not derive a slug from the task; pass --slug" 2

BRANCH="feat/$SLUG"
NAME="sf_$(printf '%s' "$SLUG" | tr '-' '_')_claude"   # tmux session name: sf_<slug>_claude
# tfmux session: deterministic, path-safe (slug is already [a-z0-9-]). One session
# per run, so concurrent launches never collide. Override with SF_TFMUX_SESSION.
TFMUX_SESSION="${SF_TFMUX_SESSION:-tfmux-$SLUG}"
AGENT_SHELL="${SF_AGENT_SHELL:-${SHELL:-/bin/zsh}}"
if [ -n "${SF_CLAUDEGO_CMD:-}" ]; then
  CLAUDE_CMD="$SF_CLAUDEGO_CMD"
else
  case "$EFFORT" in
    low|medium|high|max) ;;
    *) die "invalid --effort '$EFFORT' (use low|medium|high|max)" 2 ;;
  esac
  if [ "$EFFORT" = "max" ]; then
    CLAUDE_CMD="claudeo"
  else
    CLAUDE_CMD="CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=10 claude --effort $EFFORT --dangerously-skip-permissions"
  fi
fi

# ping-back needs tmux (tfmux bind --here records this pane via TMUX_PANE). The
# orchestrator is normally in tmux; if not, we warn and skip the ping (tfmux has no
# pane to bind). IN_TMUX gates the mediator bind + the notify instruction.
IN_TMUX=0; [ -n "${TMUX:-}" ] && IN_TMUX=1

# preflight (tfmux is the delivery + ping path; python3 resolves the worktree path).
# No gh / git remote requirement — Claude works directly in the worktree, it does
# not open or merge a PR.
for bin in git wt tmux dotllm tfmux python3; do command -v "$bin" >/dev/null 2>&1 || die "missing dependency: $bin" 127; done
command -v "$AGENT_SHELL" >/dev/null 2>&1 || die "missing agent shell: $AGENT_SHELL" 127
REPO="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not a git repo (run from inside the repo to work on)" 2
if ! CLEAN_HELPER="$(resolve_cleanup_helper)"; then
  CLEAN_HELPER="$HOME/.config/skl/library/skills/tmux-factory/tmux-factory-codex-go/scripts/factory-cleanup.sh"
fi

if [ "$DRY" = 1 ]; then
  cat <<EOF
+ wtc $SLUG     # = wt switch -c feat/$SLUG --base=@   (mirrors ~/.zshrc)
+ WT=\$(wt list --format json | resolve path for $BRANCH)
+ quarantine_hook_copied_dotllm "\$WT"         # move hook-copied real .llm aside before init
+ dotllm trust "\$WT"
+ ( cd "\$WT" && dotllm init -q )              # worktree .llm = Claude's project root / scratch
$([ "$IN_TMUX" = 1 ] && echo "+ tfmux bind mediator --here --session $TFMUX_SESSION --role mediator --kind generic   # ping target = this pane, in session $TFMUX_SESSION" || echo "# (not in tmux → cannot bind mediator → no ping-back this run)")
+ assert_worktree_ready "\$WT"                 # invariant: worktree exists + .llm initialized BEFORE launch
+ tmux new-session -d -s "$NAME" -e TFMUX_SESSION=$TFMUX_SESSION -c "\$WT" -- "$AGENT_SHELL" -ic '$CLAUDE_CMD'   # spawn detached Claude in the worktree (cwd=\$WT), TFMUX_SESSION exported
+ wait_ready $NAME                             # poll until Claude TUI is up
+ APANE=\$(tmux list-panes -t $NAME -F '#{pane_id}' | head -1)
+ tfmux bind $NAME --tmux "\$APANE" --session $TFMUX_SESSION --role agent --kind claude
+ verified_send $NAME <prompt>                 # tfmux send '<task>'$([ "$IN_TMUX" = 1 ] && echo " + 'when done: tfmux send mediator …'") (fail hard if not confirmed)
+ tfmux attach "$NAME"                         # attach to new tmux window (if in tmux)
session: $NAME   tfmux: $TFMUX_SESSION   branch: $BRANCH   effort: $EFFORT   ping-back: $([ "$IN_TMUX" = 1 ] && echo "yes (tfmux send mediator)" || echo "no (not in tmux)")
cleanup after merged PR: $CLEAN_HELPER --session $NAME --worktree "\$WT" --branch $BRANCH --repo "$REPO" --pr <PR_URL_OR_NUMBER>
EOF
  exit 0
fi

validate_agent_command

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

# 2. prep the worktree: pre-approve (skip the trust prompt) + give Claude a .llm
#    project root in the worktree for its scratch.
quarantine_hook_copied_dotllm "$WT"
dotllm trust "$WT" >/dev/null 2>&1 || true
( cd "$WT" && dotllm init -q ) || die "dotllm init failed in $WT" 1

# 3. bind the caller's pane as the tfmux mediator (the ping target) IN this run's
#    session, so Claude reaches it later with `tfmux send mediator --session $TFMUX_SESSION`.
#    Idempotent — re-binds the current orchestrator pane on every run. Each run uses
#    its own per-slug session, so concurrent launches never re-point one another's
#    mediator; fire as many as you like.
NOTIFY=0
if [ "$IN_TMUX" = 1 ]; then
  if tfmux bind mediator --here --session "$TFMUX_SESSION" --role mediator --kind generic >/dev/null 2>&1; then
    NOTIFY=1
  else
    printf 'claude-go: WARNING — could not bind mediator (no ping-back this run); continuing\n' >&2
  fi
else
  printf 'claude-go: NOTE — not in tmux; Claude cannot ping back. Check the worktree manually.\n' >&2
fi

# 4. spawn the DETACHED Claude session and wait for its composer to come up. The
#    worktree-first invariant is load-bearing here: steps 1-2 already created the
#    worktree and initialized its .llm, and we launch with cwd already inside it
#    (-c "$WT") and TFMUX_SESSION exported (-e) — never in the main checkout with a
#    later cd. assert_worktree_ready turns any future reordering (spawn before the
#    worktree exists/initializes) into a loud abort instead of a wrong-directory launch.
assert_worktree_ready "$WT"
if ! tmux new-session -d -s "$NAME" -e TFMUX_SESSION="$TFMUX_SESSION" -c "$WT" -- "$AGENT_SHELL" -ic "$CLAUDE_CMD"; then
  die "tmux session creation failed for $NAME" 1
fi
wait_ready "$NAME" || printf 'claude-go: NOTE — Claude readiness not confirmed within timeout; sending anyway (verified_send will retry)\n' >&2

# 5. bind the agent's pane into this run's tfmux session for robust delivery. On
#    failure here the session is a zombie (idle, no task, never pings), so kill it
#    before dying — and point at the worktree to clean up.
APANE="$(tmux list-panes -t "$NAME" -F '#{pane_id}' 2>/dev/null | head -1)"
if [ -z "$APANE" ]; then
  tmux kill-session -t "$NAME" 2>/dev/null || true
  die "could not resolve a pane id for session $NAME (killed it). Remove the worktree with: wtr $BRANCH" 1
fi
if ! tfmux bind "$NAME" --tmux "$APANE" --session "$TFMUX_SESSION" --role agent --kind claude >/dev/null 2>&1; then
  tmux kill-session -t "$NAME" 2>/dev/null || true
  die "tfmux bind failed for $NAME ($APANE) (killed the session). Remove the worktree with: wtr $BRANCH" 1
fi

# 6. build the prompt (verbatim task + the done-notify instruction when a mediator
#    is bound) and deliver it via tfmux, confirming Claude starts. If delivery is
#    never confirmed we kill the unstarted session and exit non-zero rather than
#    pretend the task started.
PROMPT_FILE="$(mktemp -t claudego-prompt.XXXXXX)"
trap 'rm -f "$PROMPT_FILE"' EXIT
{
  printf '%s\n' "$TASK"
  if [ "$NOTIFY" = 1 ]; then
    printf '\n---\n'
    printf 'When you are completely finished with this task — OR you hit a true blocker you cannot resolve — your FINAL action MUST be to notify the mediator by running this exact command (run it verbatim, just replace <status>):\n\n'
    printf "tfmux send mediator --session %s --text '<status>'\n\n" "$TFMUX_SESSION"
    printf '<status> is ONE line, no single quotes: on success  \xe2\x9c\x85 %s done: <one-line summary>  ·  on blocker  \xe2\x9a\xa0\xef\xb8\x8f %s blocked: <one-line reason>. Do not skip this final step.\n' "$SLUG" "$SLUG"
  fi
} > "$PROMPT_FILE"

if ! verified_send "$NAME" "$PROMPT_FILE"; then
  printf 'claude-go: ERROR — prompt delivery was not confirmed for %s after retries\n' "$NAME" >&2
  printf 'claude-go: last pane capture follows:\n' >&2
  capture_pane "$NAME" | tail -40 >&2 || true
  tmux kill-session -t "$NAME" 2>/dev/null || true
  die "killed unstarted session $NAME. Worktree is left at $WT; retry after inspecting with: wts $SLUG" 1
fi

# 7. attach to the current tmux session in a new window (best-effort, tfmux-native)
ATTACHED=""
if [ "$IN_TMUX" = 1 ]; then
  if tfmux attach "$NAME" >/dev/null 2>&1; then
    ATTACHED="✓ attached to new window"
  fi
fi

# 8. report the handles
printf -v CLEANUP_CMD '%q --session %q --worktree %q --branch %q --repo %q --pr <PR_URL_OR_NUMBER>' "$CLEAN_HELPER" "$NAME" "$WT" "$BRANCH" "$REPO"
if [ "$NOTIFY" = 1 ]; then
  PING_LINE="→ ping-back:    Claude 'tfmux send mediator's a status line to THIS pane when done (stay idle to catch it; if none in ~15 min, check the worktree below — the ping depends on Claude running the final step)"
elif [ "$IN_TMUX" = 1 ]; then
  PING_LINE="→ no ping-back: mediator bind failed (see warning above) — check the worktree manually below"
else
  PING_LINE="→ no ping-back: not in tmux — Claude can't ping; check the worktree manually below"
fi
cat <<EOF

spawned:  $NAME
worktree: $WT
branch:   $BRANCH
tfmux:    $TFMUX_SESSION
Claude is running independently with configured reasoning effort (default: claudeo/max).
→ status:       $ATTACHED
$PING_LINE
→ check:        wts $SLUG       (switch to worktree)   ·   git diff main (see changes)
→ watch live:   tmux attach -t $NAME   ·   tmux capture-pane -p -t $NAME | tail -80
→ after merge:  $CLEANUP_CMD
EOF
