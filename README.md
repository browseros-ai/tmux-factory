# tmux-factory

A software factory built with tmux: an orchestrator pane that fires off real
coding-agent sessions into isolated git worktrees, and gets pinged back when
they are done.

![tmux-factory demo](assets/demo.gif)

## What This Is

Two pieces:

- **`tfmux`** — a small synchronous Rust CLI that names tmux panes, delivers
  work into them (buffer, paste, Enter, then verifies the payload was
  submitted), and lets any pane send text back to the mediator pane.
- **Drop-in Claude Code skills** — `/tmux-factory-claude-go`,
  `/tmux-factory-claude-opus-go`, `/tmux-factory-codex-go`. They turn `tfmux`
  into a fire-and-forget factory: each one spins up a worktree, spawns a
  detached agent session, delivers the task, and arms a ping back to your pane.

No daemon. No polling. No inbox, transcript, or dashboard. State is a few JSON
files under `~/.tfmux`.

## Why

The orchestrator pattern everyone is talking about: a planner delegates to
workers, and most tokens bill at the cheaper worker rate.

tmux-factory runs that pattern on your laptop, with one difference — the workers
are not API calls. They are **full Claude Code and codex sessions**, each in its
own tmux pane and its own git worktree, on its own branch. You can attach to any
of them and watch the agent work. When a worker finishes, it does not write to a
queue you have to poll: it sends a one-line status **into your pane**, through
`tfmux`. You stay idle and catch it.

Plan in one pane. Delegate. Get pinged.

## Prerequisites

| Requirement | Needed for | Notes |
|---|---|---|
| macOS or Linux | everything | no Windows support |
| `tmux` | everything | must be on `PATH`, or set `TFMUX_TMUX_BIN` |
| Rust toolchain | building `tfmux` | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Claude Code CLI | the skills, and the live demo | [claude.com/claude-code](https://claude.com/claude-code) |
| Codex CLI | `/tmux-factory-codex-go` | optional |
| `gh` | `codex-go` opening and merging PRs | optional |

`tfmux` itself needs only tmux and Rust. The agent CLIs are what make it a
factory; `demo.sh` falls back to plain-shell workers if none are installed.

## Quickstart

```bash
git clone https://github.com/felarof99/tmux-factory && cd tmux-factory
./install.sh
```

`install.sh` checks tmux and cargo, warns if the `claude`/`codex` CLIs are
missing, runs `cargo install --path .`, and copies the skills into
`~/.claude/skills/` (pass `--force` to overwrite existing copies).

Then, **from inside tmux**, the 60-second version:

```bash
./demo.sh
```

It spawns a detached tmux session `tfmux-demo` with one mediator and three
worker panes, bound through `tfmux` under the session `demo-hello`. If the
`claude` CLI is installed, the workers are live Claude sessions: each says hi and
sends `worker N: hi, ready` back to the mediator pane. Watch three agents report
in without a single poll.

Tear it down:

```bash
tmux kill-session -t tfmux-demo
```

### The Real Thing

Open Claude Code in any git repo, inside tmux, and type:

```text
/tmux-factory-codex-go add a --json flag to the targets command
```

Your pane is bound as the tfmux mediator. A worktree and a detached codex
session are created, the task is delivered and verified, and control returns to
you immediately. codex runs the full loop — design, implement, review, open a
PR, squash-merge to main — and then pings you. Minutes later, a line lands in
your pane, as if someone typed it there:

```text
merged: add --json flag to targets (PR #42, squash-merged to main)
```

Then `git pull` and keep going. The other two skills:

| Skill | What it does |
|---|---|
| `/tmux-factory-claude-go <task>` | Fire a Claude Code session in a worktree; get pinged done or blocked. |
| `/tmux-factory-claude-opus-go <task>` | The same, on Opus at max effort. |
| `/tmux-factory-codex-go <feature>` | Full loop: design, implement, review, open PR, squash-merge to main, then ping. |

Fire several. They run in parallel, in separate worktrees, and each one pings you
when it lands. Attach to any of them with `tfmux attach <tmux-session>` to watch.

## How It Works

```text
                        your tmux window
          +-------------------------------------------+
          |  mediator pane                            |
          |  Claude Code, planning and delegating     |
          +-------------------------------------------+
                 |                            ^
   tfmux send    |                            |   tfmux send mediator
   (task in)     |                            |   ("merged" / "blocked")
                 v                            |
          +-------------------------------------------+
          |  detached tmux sessions, one per worker   |
          |                                           |
          |  worker1  claude  ../wt/feat-a   feat-a   |
          |  worker2  codex   ../wt/feat-b   feat-b   |
          |  worker3  claude  ../wt/fix-c    fix-c    |
          +-------------------------------------------+
```

Each worker is bound to a stable name, so the mediator addresses it by name
rather than by a pane id that moves. Delivery goes through a named tmux buffer,
paste, Enter, and a scrollback check; if a Claude/codex pasted-content marker is
still visible, `tfmux` sends one more Enter and checks again. The ping back is
the same mechanism in reverse — a worker simply runs
`tfmux send mediator --text "..."`.

## tfmux Command Reference

Run binding commands from inside tmux. On failure, `tfmux` prints
`error: <message>` to stderr and exits 1.

### bind

```bash
tfmux bind <NAME> (--here | --tmux <TARGET>) [--role mediator|agent]
                  [--kind claude|codex|generic] [--session NAME]
                  [--socket NAME] [--json]
```

Bind a pane to a stable name. Use exactly one pane source: `--here` reads
`TMUX_PANE` and binds the current pane; `--tmux <TARGET>` resolves any tmux
target string such as `%5` or `demo:1.0`. `--role` defaults to `agent`, `--kind`
to `generic`; both are stored as metadata. `bind` creates the tfmux session on
first use.

```bash
tfmux bind mediator --here --role mediator
tfmux bind agent1 --tmux %5 --role agent --kind claude
```

### send

```bash
tfmux send <NAME> (--text <TEXT> | --file <FILE> | -) [--session NAME]
```

Deliver a payload to a bound pane. Use exactly one input source; empty payloads
fail. Before sending, `tfmux` re-resolves the stored pane id and checks the pane
metadata still matches the binding — if the pane is gone or changed, `send` fails
and tells you to rebind.

```bash
tfmux send agent1 --text "Investigate the flaky test in store.rs"
tfmux send agent1 --file plan.md
echo "build and report back" | tfmux send agent1 -
tfmux send mediator --text "agent1 done"
```

### targets

```bash
tfmux targets [--session NAME] [--json]
```

```text
NAME       ROLE     KIND     PANE   LOCATION       STATUS
agent1     agent    claude   %5     demo:1.0       live
mediator   mediator generic  %3     demo:0.0       live
```

`live` means the pane id and stored tmux metadata still match. `stale` means the
pane id resolves but its session/window/pane metadata changed. `dead` means the
pane no longer resolves.

### attach

```bash
tfmux attach <TMUX_SESSION> [--window-name NAME] [--socket NAME]
```

Run from inside tmux. Checks `tmux has-session`, then opens a new window running
`env -u TMUX tmux attach-session -t <TMUX_SESSION>`, so you can watch a detached
worker. `--window-name` defaults to the session name. `attach` is independent of
factory state: it reads no `TFMUX_SESSION`, no marker file, no `~/.tfmux`.

### unbind

```bash
tfmux unbind <NAME> [--session NAME] [--json]
```

Removes the target record from the selected session.

## State

A tfmux session groups the targets for one factory. There is no global "current
session" file — session identity travels with the pane. Stateful commands
(`bind`, `send`, `targets`, `unbind`) resolve it in this order:

1. `--session NAME`
2. `TFMUX_SESSION`
3. First line of `.llm/tfmux-session` in the current directory

Target and session names must be single path-safe tokens: no spaces, slashes,
backslashes, tabs, or newlines. State lives under `$TFMUX_HOME`, else `~/.tfmux`,
and every write is atomic:

```text
~/.tfmux/2026-06-28/demo/
  session.json          # session name + creation timestamp
  targets/mediator.json # name, role, kind, pane id, tmux location, socket, timestamps
  targets/agent1.json
```

The date directory is the local calendar date the session was created.

| Variable | Effect |
|---|---|
| `TFMUX_SESSION` | Default tfmux session for stateful commands. |
| `TFMUX_HOME` | State root (default `~/.tfmux`). |
| `TFMUX_TMUX_BIN` | tmux binary to use (default: `tmux` on `PATH`). |
| `TFMUX_SOCKET` | tmux socket for `bind` and `attach`, below the `--socket` flag. |
| `TFMUX_MAIN_SOCKET` | Socket name treated as the default when derived from `TMUX`. |

Sockets rarely need attention. `bind --here` and `attach` derive the socket from
the `TMUX` environment variable; `--socket` or `TFMUX_SOCKET` override it, and the
chosen socket is stored on the target so later `send`/`targets` calls reach the
right server.

## Troubleshooting

**"attach requires TMUX" / "--here requires TMUX and TMUX_PANE".** These
commands must run inside tmux. Start a tmux session first, or bind by explicit
target with `--tmux %5`.

**A worker pane died, and `send` fails.** Run `tfmux targets` — the pane will
show `dead` or `stale`. Rebind it: `tfmux bind agent1 --tmux <new-target>`.
Bindings are pane ids, not promises.

**"no tfmux session selected".** The command found no session context. Pass
`--session NAME`, export `TFMUX_SESSION`, or drop the name on the first line of
`.llm/tfmux-session` in the working directory. Workers need the *same* session
context as the mediator, or their ping has nowhere to land.

**The `/tmux-factory-*` skills do not show up.** Restart Claude Code. Skills are
read at startup, and `install.sh` copies them into `~/.claude/skills/` after
yours is already running.

**Demo workers sit there doing nothing.** The `claude` CLI is installed but not
authenticated. Run `claude` once in a normal shell, sign in, then re-run
`./demo.sh`. With no agent CLI at all, the demo falls back to plain-shell
workers and still pings the mediator.

**tmux is somewhere unusual.** `export TFMUX_TMUX_BIN=/path/to/tmux`.

## Development

```bash
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Run `cargo fmt` before committing. See `CLAUDE.md` for contributor rules and
module ownership.
