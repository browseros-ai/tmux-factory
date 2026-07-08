---
name: tmux-factory-claude-go
description: Fire-and-get-pinged Claude Code session (tmux-factory edition). You stay on main while it spins up an isolated worktree plus a detached Claude Code tmux session, delivers the task through the tfmux CLI, and binds your pane as the tfmux mediator so Claude PINGS YOU with a one-line done or blocked status when it finishes. No polling, no daemon — just return to idle and the status line lands in your pane. Want codex running the full design, implement, review, squash-merge loop instead? use tmux-factory-codex-go. Use the /tmux-factory-claude-go slash command with your task.
---

# tmux-factory — Claude Go (fire-and-get-pinged)

> **Fire, then get pinged.** Kick Claude off in its own worktree with the task **already delivered**, then **get straight back to the main thread** — no polling, no waiting. Claude **pings your pane** with a one-line status the moment it finishes (or hits a blocker). Stay idle to catch it.

> **Then stop — don't poll, don't babysit.** You (the mediator) already got confirmation: the launcher verified the task landed **and** that Claude actually started work — that's the only check you need. So the moment it returns:
> - **No background watcher, no poll loop** — don't fire `gh pr list` / `tmux capture-pane` every N seconds. Busy-waiting right after launch defeats the whole fire-and-get-pinged design.
> - **The nudge comes to you** → Claude `tfmux send`s a one-line ✅ done / ⚠️ blocked status straight into your pane. Just return to idle and catch it.
> - **Want a safety net?** Worst case only, **one** long poll — a single background command that sleeps ~30 min, then checks/reminds **once**. Never short repeated polls.

Spin up Claude Code in its own worktree as a detached tmux session, deliver the task over **`tfmux`**, and bind **your** pane as the tfmux **mediator** so Claude pings you when done. A bundled script does every mechanical step in one shot — worktree → quarantine hook-copied `.llm` → `dotllm trust` + `init` → bind mediator → spawn → **verified `tfmux send`** → arm the done-ping — so you're back on `main` the moment it returns.

**This actually runs the task or fails clearly.** The launcher delivers via `tfmux send` (bracketed paste + Enter), then confirms Claude left the idle composer and started work. If delivery can't be confirmed after retries, it kills the unstarted session, leaves the worktree for inspection, and exits non-zero. (For the full design → implement → review → squash-merge loop on **codex**, use **`tmux-factory-codex-go`**.)

**Run from inside the repo you want to work on, on `main`** for the normal isolated-worktree flow — and ideally inside tmux (the ping-back binds your pane).

## Outside a git repo

If the current directory is not inside a git repo, the launcher degrades to in-place mode: no branch or worktree is created, Claude starts with cwd set to the directory where you fired the command, dotllm setup is best-effort, and the same done/blocked `tfmux send mediator` ping is armed. Broken git metadata or other git errors fail loudly instead of falling back. The launch notice says there is no worktree isolation because Claude edits that directory directly.

## Do this

**1. (Rich task only) capture it verbatim to a file.** Multi-line, `@paths`, or code snippets → `Write` the exact text to a temp file and pass `--task-file` (shell quoting would mangle it). One-liners go inline.

**2. Run the engine — one command does everything:**

```bash
eng=~/.claude/skills/tmux-factory-claude-go/scripts/claude-go.sh
[ -x "$eng" ] || eng=~/.config/skl/library/skills/tmux-factory/tmux-factory-claude-go/scripts/claude-go.sh  # fallback: source tree
"$eng" "<task, verbatim>"
# rich request:   "$eng" --task-file /tmp/task.md
# options:        --slug <label>   --effort low|medium|high|max   --dry-run
```

The script: creates the `feat/<slug>` worktree → quarantines any hook-copied `.llm` → `dotllm trust` + `init` (Claude's project root) → derives a per-run tfmux session `tfmux-<slug>` → (in tmux) **binds THIS pane as the tfmux `mediator`** in that session → verifies the `claudex`/`claude` launch command resolves → spawns a detached Claude tmux session (`sf_<slug>_claude`, `TFMUX_SESSION` exported) → waits for Claude's TUI → binds Claude's pane and delivers the task via **verified `tfmux send`** (confirms Claude started, retries up to 5×) → tells Claude to **`tfmux send mediator`** a status line when done → best-effort opens the session in a new tmux window via `tfmux attach`. Prints `spawned / worktree / branch / tfmux / ping-back`.

**3. Surface the output, then return to idle.** Don't poll or babysit (**see the no-poll callout up top**) — the **ping comes to you**. More tasks? fire the next one; each uses its own `tfmux-<slug>` session and pings back independently.

## What you just fired

| Owner | Does |
|-------|------|
| **the script** (one shot) | worktree on `feat/<slug>` · quarantine hook-copied `.llm` · `dotllm trust` + `init` · derive `tfmux-<slug>` · **bind THIS pane as tfmux `mediator`** · verify `claudex`/`claude` · spawn detached Claude (`TFMUX_SESSION` exported) · **verified `tfmux send` of the task** · arm the done-ping · auto-attach window · return |
| **Claude** (unattended in the worktree) | receive the task → work independently → commit changes → **`tfmux send mediator` a one-line status when done or blocked** |
| **you** (idle in your pane) | stay put → **catch Claude's ping** → **then always finish the run yourself:** review `git diff main` → `git -C <REPO> merge feat/<slug>` into local main → `git -C <REPO> pull --rebase origin main` → remove the worktree + kill the session. Never hand the finish to a human. (⚠️ blocked → don't merge; surface the blocker.) |

> Claude runs through `claudex` (max effort) by default. **State the task clearly** — Claude asks for clarification if needed, and still pings `⚠️ <slug> blocked: …` if it gets stuck.

## How the ping-back works

`tfmux` has **no global current session** — routing is by an explicit named session. The script derives one per run (`tfmux-<slug>`), binds **your** pane as `mediator` in it, and exports `TFMUX_SESSION` into Claude's shell. Claude reaches you with its **final** action:

```bash
tfmux send mediator --session tfmux-<slug> --text '✅ <slug> done: <one-line summary>'   # or: ⚠️ <slug> blocked: <one-line reason>
```

That line lands in **your** pane. **Stay idle to catch it.** Ping-back needs you **in tmux** (the script binds your pane via `--here`); if you're not, the script warns and degrades to fire-and-forget — then you check the worktree yourself.

## Prerequisites (check once)
- **`tfmux` — REQUIRED** (delivers the task *and* carries the ping-back). `tmux` on PATH too. In git mode, `wt`, `dotllm`, `git`, and `python3` are preflighted; outside git, dotllm is tried only best-effort.
- `claudex` alias resolvable in your agent shell for default `max` launches (`zsh -ic 'type claudex'` is the check); `claude` on PATH for non-`max` efforts; or set `SF_CLAUDEGO_CMD`.
- The cleanup step needs `gh` authenticated so the shared helper can verify the merged PR before deleting the worktree.
- **Ping-back needs you in tmux.** Not in tmux → no mediator pane → no ping; the script warns and still delivers the task.

## Checking on it (you'll usually be pinged first)

Normally you just **wait for the ping**. To look before then:

```bash
wts feat/<slug>                       # switch into the worktree
git diff main                         # see local changes
tmux attach -t sf_<slug>_claude       # watch Claude live
tmux capture-pane -p -t sf_<slug>_claude | tail -80
```

- **Got `✅ <slug> done: …`** → **finish it yourself, always — never leave it for a human.** `wts feat/<slug>` → review `git diff main` → merge into local main (`git -C <REPO> merge feat/<slug>`) → `git -C <REPO> pull --rebase origin main` (main must end up current) → tear the run down (no PR here, so manual): `git -C <REPO> worktree remove <WT>` · `git -C <REPO> branch -d feat/<slug>` · `tmux kill-session -t sf_<slug>_claude`.
- **If Claude opened a PR instead** → run the printed `factory-cleanup.sh ... --pr <PR_URL_OR_NUMBER>` command; it ff-only syncs main, proves the merge commit is on main, removes the worktree, then kills the session — steps 2–3 in one safe pass.
- **Got `⚠️ <slug> blocked: …`** → attach and unblock, or re-fire with a clearer task.
- **"prompt delivery was not confirmed"** → the script killed the unstarted session and left the worktree; inspect, then clean up (`wtr feat/<slug>`) and retry with a new `--slug`.
- **Still quiet** → leave it; the ping will come, or watch live with `tmux attach -t sf_<slug>_claude`.

## Notes
- **Worktree-first (load-bearing):** the launcher always finishes the worktree — create `feat/<slug>` → quarantine `.llm` → `dotllm trust` → `dotllm init` — and **only then** spawns the tmux session with cwd already set to the worktree (`tmux new-session -c "$WT"`). Claude comes up already inside `$WT`. `assert_worktree_ready "$WT"` aborts loudly on any reordering; `scripts/test-launch-order.sh` pins it.
- **Per-run session:** each launch gets its own `tfmux-<slug>` session, so runs never collide — fire as many as you like, each pings back on its own. Override with `SF_TFMUX_SESSION`.
- **Launch config:** the Claude command lives near the top of `scripts/claude-go.sh` (`CONFIG POINT`). Default is `claudex` (max). Override per-run with `--effort`, or wholesale with `SF_CLAUDEGO_CMD` / `SF_CLAUDEGO_EFFORT`.
- **Verified delivery:** after `tfmux send` the script confirms Claude actually **started** and retries up to 5× — guards a readiness race that could leave the task unstarted.
- **Cleanup split:** the launcher prints the exact `factory-cleanup.sh --session <NAME> --worktree <WT> --branch <BRANCH> --repo <REPO> --pr <PR_URL_OR_NUMBER>` command because it knows the run handles. The bundled `factory-cleanup.sh` helper (shared from the `tmux-factory-codex-go` skill, fake-binary tested) owns the safety checks and deletion sequence — verify merged PR, ff-only update main, prove the merge commit is on main, remove the worktree, then kill tmux. Do not duplicate that logic in skill prose.
- **`.llm` is for Claude, not routing:** `dotllm init` gives the worktree a project `.llm` for Claude's scratch; the ping is routed by the named tfmux session, not the `.llm`.
- **Preview:** `"$eng" --dry-run "<task>"` prints the exact steps (including whether ping-back is on) without spawning.
- **Slug + sessions:** slug derived from the task (kebab, ~6 words) unless `--slug`; tmux session = `sf_<slug_with_underscores>_claude`, tfmux session = `tfmux-<slug>`.

## Crib sheet
- `"$eng" "<task>"` — fire it. `--task-file` · `--slug` · `--effort low|medium|high|max` · `--dry-run`.
- **Ping-back:** Claude `tfmux send mediator`'s a `✅ <slug> done: …` (or `⚠️ blocked`) line to **your** pane — stay idle (needs you in tmux).
- **After the ✅ ping (always finish it):** review `git diff main` → `git -C <REPO> merge feat/<slug>` → `git -C <REPO> pull --rebase origin main` → worktree remove + `tmux kill-session`. ⚠️ blocked → surface it, don't merge.
- `wts feat/<slug>` — switch into the worktree · `git diff main` — see changes · `tmux list-sessions` — active sessions.
- `factory-cleanup.sh --session sf_<slug>_claude --worktree <WT> --branch feat/<slug> --repo <REPO> --pr <PR_URL_OR_NUMBER>` — safe cleanup after merged PR; use the printed command.
- `tmux attach -t sf_<slug>_claude` — watch live if needed.
- `bash -n scripts/claude-go.sh` · `scripts/test-dotllm-quarantine.sh` · `scripts/test-launch-order.sh` · `scripts/test-claude-send-state.sh` · `"$eng" --dry-run --slug smoke "smoke task"` — local checks after edits.
