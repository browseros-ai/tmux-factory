# tfmux

A small, synchronous Rust CLI for driving a fleet of agents that each live in
their own tmux pane: bind panes to names, send prompts into them, and let agents
report back.

*Inspired by `riff3`; shares no state, paths, or commands with it.*

## What it does

- **Names tmux panes as targets** — `mediator`, `agent1`, `agent2`, ….
- **Sends prompts/text into those panes** via a tmux buffer + bracketed paste +
  Enter, then verifies the paste actually landed.
- **Lets agents report back** by sending to the `mediator` target — the "I'm
  done" callback is just `tfmux send mediator --text "done"`, no special command.
- **Keeps state under `~/.tfmux`** — one `session.json` per session, one JSON
  file per bound target.
- **No transcript, inbox, or dashboard.** It is a transport, nothing more.

## Install / build

```bash
cargo build                 # debug binary at target/debug/tfmux
cargo install --path .      # optional: install tfmux into ~/.cargo/bin
```

Requires a `tmux` binary on `PATH` (override with `TFMUX_TMUX_BIN`).

## Session model

A session groups the targets of one factory. There is no global "current
session" — session identity travels with the pane, so many factories can run at
once.

- **State root:** `$TFMUX_HOME` if set, otherwise `~/.tfmux`.
- **Session selection precedence** (used by every command):
  1. `--session NAME` flag
  2. `TFMUX_SESSION` environment variable
  3. `.llm/tfmux-session` marker (first line) in the current directory
- `bind` **creates** the session on first use; `send`, `targets`, and `unbind`
  require it to already exist.
- There is deliberately **no** `~/.tfmux/current` file.

## Commands

Names (targets and sessions) must be single path-safe tokens. On error, tfmux
prints `error: <msg>` to stderr and exits 1.

### `tfmux bind <NAME>`

Register a tmux pane under `NAME` in the current session.

```bash
# Bind the mediator to the current pane (run from inside tmux).
tfmux bind mediator --here --role mediator
# -> bound mediator -> %3 (demo:0.0)

# Bind an agent pane by tmux target.
tfmux bind agent1 --tmux %5 --role agent --kind claude
# -> bound agent1 -> %5 (demo:1.0)
```

Pick exactly one pane source: `--here` (binds the current pane via `TMUX_PANE`)
or `--tmux <TARGET>` (any tmux target string, e.g. `%5` or `sess:1.0`).
`--role` is `mediator` or `agent` (default `agent`); `--kind` is `claude`,
`codex`, or `generic` (default `generic`) — both are light metadata. Add
`--json` to print the stored target instead of the text summary.

### `tfmux send <NAME>`

Deliver a payload to a bound target's pane and verify it was submitted.

```bash
tfmux send agent1 --text "Investigate the flaky test in store.rs"
# -> sent 38 bytes to "agent1" (%5)

tfmux send agent1 --file plan.md      # send a file's contents
echo "build and report back" | tfmux send agent1 -   # read payload from stdin
```

Pass exactly one input source: `--text`, `--file`, or `-` (stdin). If the pane
is gone or no longer resolves to the same id, `send` fails and tells you to
rebind.

### `tfmux targets`

List bound targets in the session and re-check each pane's live/stale/dead
status.

```bash
tfmux targets
# NAME       ROLE     KIND     PANE   LOCATION       STATUS
# agent1     agent    claude   %5     demo:1.0       live
# mediator   mediator generic  %3     demo:0.0       live

tfmux targets --json    # machine-readable rows for the mediator
```

### `tfmux unbind <NAME>`

Remove one target from the session.

```bash
tfmux unbind agent1
# -> unbound "agent1" from session demo
```

Add `--json` for a stable summary.

Every command accepts `--session NAME` to override session selection.

## Minimal workflow

```bash
# 1. In the mediator pane (inside tmux), name the session and bind yourself.
export TFMUX_SESSION=demo
tfmux bind mediator --here --role mediator

# 2. Bind an agent pane you spawned (split/new-window) by its tmux pane id.
tfmux bind agent1 --tmux %7 --role agent --kind claude

# 3. Send work to the agent.
tfmux send agent1 --text "Investigate the flaky test in store.rs and report back"

# 4. From the agent pane (sharing the session via TFMUX_SESSION=demo or a
#    .llm/tfmux-session marker), report completion back to the mediator.
tfmux send mediator --text "agent1 done"
```

## Storage layout

```
~/.tfmux/                       # or $TFMUX_HOME
  2026-06-28/                   # session creation date (local calendar date)
    demo/                       # session name
      session.json              # { name, created_at }
      targets/
        mediator.json           # one file per bound target
        agent1.json
```

## Development

```bash
cargo fmt --check                           # formatting is the source of truth
cargo test                                  # unit tests + tests/cli.rs
cargo clippy --all-targets -- -D warnings   # must pass clean
```

See `CLAUDE.md` for the full contributor and architecture guidelines.
