#!/usr/bin/env bash
set -euo pipefail

DEMO_TMUX_SESSION="tfmux-demo"
TFMUX_SESSION_NAME="demo-hello"

if [[ -z "${TMUX:-}" ]]; then
  echo "error: demo.sh must be run from inside tmux." >&2
  echo "Start tmux, then run ./demo.sh again." >&2
  exit 1
fi

if ! command -v tfmux >/dev/null 2>&1; then
  echo "error: tfmux was not found on PATH." >&2
  echo "Run ./install.sh first, then try ./demo.sh again." >&2
  exit 1
fi

tfmux_bin="$(command -v tfmux)"
printf -v tfmux_cmd "%q" "$tfmux_bin"
printf -v tfmux_session_arg "%q" "$TFMUX_SESSION_NAME"
tfmux_env_prefix=""
if [[ -n "${TFMUX_HOME:-}" ]]; then
  printf -v tfmux_env_prefix "TFMUX_HOME=%q " "$TFMUX_HOME"
fi

start_mediator() {
  local pane="$1"
  tmux send-keys -t "$pane" \
    "clear; printf 'tfmux demo mediator\nWaiting for worker greetings...\n\n'; stty -echo 2>/dev/null || true; while IFS= read -r line; do printf 'mediator received: %s\n' \"\$line\"; done" \
    C-m
}

bind_target() {
  local name="$1"
  local pane="$2"
  local role="$3"
  tfmux bind "$name" \
    --tmux "$pane" \
    --session "$TFMUX_SESSION_NAME" \
    --role "$role" \
    --kind generic >/dev/null
}

launch_plain_worker() {
  local pane="$1"
  local worker_num="$2"
  local message="worker $worker_num: hi, ready"

  tmux send-keys -t "$pane" \
    "clear; printf 'Worker $worker_num plain-shell fallback\n'; printf '%s\n' '$message'; ${tfmux_env_prefix}$tfmux_cmd send mediator --session $tfmux_session_arg --text '$message'; exec bash" \
    C-m
}

launch_claude_worker() {
  local pane="$1"
  local worker_num="$2"
  tmux send-keys -t "$pane" \
    "clear; printf 'Worker $worker_num starting Claude Code...\n'; claude --dangerously-skip-permissions" \
    C-m
}

send_claude_prompt() {
  local worker_num="$1"
  local target="worker$worker_num"
  local send_command="${tfmux_env_prefix}tfmux send mediator --session $TFMUX_SESSION_NAME --text 'worker $worker_num: hi, ready'"
  local prompt="Say hi in one short line as worker $worker_num of this tmux software factory, then run: $send_command"
  tfmux send "$target" --session "$TFMUX_SESSION_NAME" --text "$prompt"
}

tmux kill-session -t "=$DEMO_TMUX_SESSION" 2>/dev/null || true
created="$(tmux new-session -d -s "$DEMO_TMUX_SESSION" -n factory -P -F '#{window_id} #{pane_id}' "bash --noprofile --norc -i")"
window_id="${created%% *}"
mediator_pane="${created##* }"
worker_one="$(tmux split-window -t "$window_id" -h -P -F '#{pane_id}' "bash --noprofile --norc -i")"
worker_two="$(tmux split-window -t "$mediator_pane" -v -P -F '#{pane_id}' "bash --noprofile --norc -i")"
worker_three="$(tmux split-window -t "$worker_one" -v -P -F '#{pane_id}' "bash --noprofile --norc -i")"
tmux select-layout -t "$window_id" tiled >/dev/null

worker_panes=("$worker_one" "$worker_two" "$worker_three")

sleep 0.5
start_mediator "$mediator_pane"
sleep 0.5
bind_target mediator "$mediator_pane" mediator
for i in 1 2 3; do
  bind_target "worker$i" "${worker_panes[$((i - 1))]}" agent
done

if command -v claude >/dev/null 2>&1; then
  echo "claude found; launching Claude workers."
  for i in 1 2 3; do
    launch_claude_worker "${worker_panes[$((i - 1))]}" "$i"
  done
  echo "Waiting for Claude composers..."
  sleep 8
  for i in 1 2 3; do
    send_claude_prompt "$i"
  done
else
  echo "claude not found; using plain-shell workers."
  for i in 1 2 3; do
    launch_plain_worker "${worker_panes[$((i - 1))]}" "$i"
  done
fi

cat <<NEXT

Demo is ready.
Teardown:
  tmux kill-session -t $DEMO_TMUX_SESSION
NEXT

if [[ "${TFMUX_DEMO_NO_ATTACH:-}" == "1" ]]; then
  echo "Attach manually with: tmux attach-session -t $DEMO_TMUX_SESSION"
  exit 0
fi

if tmux switch-client -t "=$DEMO_TMUX_SESSION" 2>/dev/null; then
  exit 0
fi

if tfmux attach "$DEMO_TMUX_SESSION" 2>/dev/null; then
  exit 0
fi

echo "Attach manually with: tmux attach-session -t $DEMO_TMUX_SESSION"
