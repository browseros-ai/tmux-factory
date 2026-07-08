# tfmux

`tfmux` is a small synchronous Rust CLI for coordinating named tmux panes. Bind
panes to stable names, send work into them, and let agents report back by
sending text to the mediator pane.

## What tfmux Does

- Names tmux panes as targets such as `mediator`, `agent1`, or `agent2`.
- Sends text, files, or stdin through a tmux buffer, pastes it, presses Enter,
  and verifies the payload was submitted.
- Lets any pane report back with ordinary sends, for example
  `tfmux send mediator --text "done"`.
- Lists bound targets and marks each pane `live`, `stale`, or `dead`.
- Opens detached tmux sessions in a new window of the current tmux client.
- Stores only lightweight session and target JSON under `~/.tfmux` or
  `$TFMUX_HOME`.

No transcript, inbox, dashboard, or background service is included.

## Install

```bash
cargo build
cargo install --path .
```

`cargo build` writes the debug binary to `target/debug/tfmux`. `cargo install
--path .` installs `tfmux` into `~/.cargo/bin`.

`tfmux` requires `tmux` on `PATH`. To use a different binary, set:

```bash
export TFMUX_TMUX_BIN=/path/to/tmux
```

## Quick Start

Run the binding commands from inside tmux.

```bash
# 1. Pick the tfmux session for this factory.
export TFMUX_SESSION=demo

# 2. Bind the mediator to the current pane.
tfmux bind mediator --here --role mediator

# 3. Bind an agent pane by tmux target or pane id.
tfmux bind agent1 --tmux %7 --role agent --kind claude

# 4. Send work to the agent.
tfmux send agent1 --text "Investigate the flaky test in store.rs and report back"

# 5. From the agent pane, send completion back to the mediator.
tfmux send mediator --text "agent1 done"
```

Agents need the same tfmux session context as the mediator. Share it with
`TFMUX_SESSION=demo`, pass `--session demo`, or add a `.llm/tfmux-session`
marker in the working directory.

## Session Model

A tfmux session groups the targets for one factory. There is no global current
session file; session identity comes from the command context.

Stateful commands resolve the session in this order:

1. `--session NAME`
2. `TFMUX_SESSION`
3. First line of `.llm/tfmux-session` in the current directory

`bind` creates the session the first time it is used. `send`, `targets`, and
`unbind` require an existing session.

Target names and tfmux session names must be single path-safe tokens: no spaces,
slashes, backslashes, tabs, or newlines.

## Commands

On failure, `tfmux` prints `error: <message>` to stderr and exits with status 1.

### Bind a Pane

```bash
tfmux bind <NAME> (--here | --tmux <TARGET>) [--role mediator|agent] [--kind claude|codex|generic]
```

Examples:

```bash
tfmux bind mediator --here --role mediator
tfmux bind agent1 --tmux %5 --role agent --kind claude
tfmux bind agent2 --tmux demo:1.0 --session demo --json
```

Use exactly one pane source:

- `--here` reads `TMUX_PANE` and binds the current pane.
- `--tmux <TARGET>` resolves any tmux target string, such as `%5` or
  `demo:1.0`.

`--role` defaults to `agent`; `--kind` defaults to `generic`. Both are stored as
metadata. `--json` prints the stored target record instead of the text summary.

### Send to a Target

```bash
tfmux send <NAME> (--text <TEXT> | --file <FILE> | -) [--session NAME]
```

Examples:

```bash
tfmux send agent1 --text "Investigate the flaky test in store.rs"
tfmux send agent1 --file plan.md
echo "build and report back" | tfmux send agent1 -
```

Use exactly one input source. Empty payloads fail.

Before sending, tfmux re-resolves the stored pane id and checks that the pane
metadata still matches the binding. If the pane is gone or changed, `send`
fails and asks you to rebind.

Delivery uses a named tmux buffer, paste, Enter, and a short scrollback check.
If a Claude/Codex pasted-content marker is still visible, tfmux sends one more
Enter and checks again.

### List Targets

```bash
tfmux targets [--session NAME] [--json]
```

Text output:

```text
NAME       ROLE     KIND     PANE   LOCATION       STATUS
agent1     agent    claude   %5     demo:1.0       live
mediator   mediator generic  %3     demo:0.0       live
```

`targets` reloads all bound targets for the session and checks each pane:

- `live` means the pane id and stored tmux metadata still match.
- `stale` means the pane id resolves, but its session/window/pane metadata
  changed.
- `dead` means the pane no longer resolves.

Use `--json` for machine-readable rows.

### Attach a Tmux Session

```bash
tfmux attach <TMUX_SESSION> [--window-name NAME]
```

Examples:

```bash
tfmux attach worker
tfmux attach worker --window-name agent-worker
```

Run `attach` from inside tmux. It checks `tmux has-session -t <TMUX_SESSION>`,
then opens a new window that runs:

```bash
env -u TMUX tmux attach-session -t <TMUX_SESSION>
```

`attach` is independent of tfmux factory state. It does not read
`TFMUX_SESSION`, `.llm/tfmux-session`, or `~/.tfmux`.

### Unbind a Target

```bash
tfmux unbind <NAME> [--session NAME] [--json]
```

Example:

```bash
tfmux unbind agent1
```

`unbind` removes the target JSON file from the selected session. With `--json`,
it prints a stable summary containing the session, target name, and whether a
file was removed.

## Storage

Default state root:

```text
~/.tfmux
```

Override it with:

```bash
export TFMUX_HOME=/path/to/state
```

Layout:

```text
~/.tfmux/
  2026-06-28/
    demo/
      session.json
      targets/
        mediator.json
        agent1.json
```

The date directory is the local calendar date when the session is created.
`session.json` stores the session name and creation timestamp. Each target JSON
stores the bound name, role, kind, original tmux target input, canonical pane id,
tmux location, and bind timestamp.

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Use `cargo fmt` before committing. See `CLAUDE.md` for contributor rules and
module ownership.
