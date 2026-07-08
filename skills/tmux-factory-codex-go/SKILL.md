---
name: tmux-factory-codex-go
description: Fire-and-get-pinged software factory (tmux-factory edition). You stay on main while it spins up an isolated worktree plus a detached codex tmux session running the full $sf-auto loop (design, implement, review, fix, open PR, squash-merge to main), delivers the task through the tfmux CLI, and binds your pane as the tfmux mediator so codex PINGS YOU when it finishes. A one-line merged or blocked status lands right in your pane. No polling, no daemon, just return to idle, catch the ping, and git pull main. Use the /tmux-factory-codex-go slash command with your feature description.
---

# tmux-factory — Codex Go (fire-and-get-pinged)

> **Fire, then get pinged.** Kick codex off in its own worktree and **get straight back to the main thread** — no polling, no waiting. codex **pings your pane** with a one-line status the moment it squash-merges (or hits a blocker). Stay idle to catch it.

> **Then stop — don't poll, don't babysit.** You (the mediator) already got confirmation: the launcher verified the task landed **and** that codex actually started the loop — that's the only check you need. So the moment it returns:
> - **No background watcher, no poll loop** — don't fire `gh pr list` / `tmux capture-pane` every N seconds. Busy-waiting right after launch defeats the whole fire-and-get-pinged design.
> - **The nudge comes to you** → codex `tfmux send`s a one-line ✅ merged / ⚠️ blocked status straight into your pane. Just return to idle and catch it.
> - **Want a safety net?** Worst case only, **one** long poll — a single background command that sleeps ~30 min, then checks/reminds **once**. Never short repeated polls.

Spin up codex in its own worktree running `$sf-auto` (design → implement → review → open PR → **squash-merge to main**), deliver the task over **`tfmux`**, and bind **your** pane as the tfmux **mediator** so codex pings you when done. A bundled script does every mechanical step in one shot — worktree → quarantine hook-copied `.llm` → `dotllm trust` + `init` → bind mediator → spawn → **verified `tfmux send`** → arm the done-ping — so you're back on `main` the moment it returns.

**Run from inside the repo you want to build in, on `main`** for the normal isolated-worktree + PR flow.

## Outside a git repo

If the current directory is not inside a git repo, the launcher degrades to in-place mode: no branch, worktree, PR, or squash-merge is created, codex starts with cwd set to the directory where you fired the command, dotllm setup is best-effort, and a done/blocked `tfmux send mediator` ping is still armed. Broken git metadata or other git errors fail loudly instead of falling back. The launch notice says there is no worktree isolation because codex edits that directory directly.

## Do this

**1. (Rich feature only) capture it verbatim to a file.** Multi-line, `@paths`, or code snippets → `Write` the exact text to a temp file and pass `--task-file` (shell quoting would mangle it). One-liners go inline.

**2. Run the engine — one command does everything:**

```bash
eng=~/.claude/skills/tmux-factory-codex-go/scripts/codex-go.sh
[ -x "$eng" ] || eng=~/.config/skl/library/skills/tmux-factory/tmux-factory-codex-go/scripts/codex-go.sh  # fallback: source tree
"$eng" "<feature, verbatim>"
# rich request:   "$eng" --task-file /tmp/feature.md
# options:        --slug <label>   --effort low|medium|high|xhigh   --dry-run
```

The script: creates the `feat/<slug>` worktree → quarantines any hook-copied `.llm` → `dotllm trust` + `init` (codex's project root) → derives a per-run tfmux session `tfmux-<slug>` → (in tmux) **binds THIS pane as the tfmux `mediator`** in that session → spawns a detached codex tmux session (`codexy`, `TFMUX_SESSION` exported) → waits for codex's composer → binds codex's pane and delivers `$sf-auto <feature>` via **`tfmux send`** (verified: confirms codex started, retries up to 5×) → tells codex to **`tfmux send mediator`** a status line when done → best-effort opens the session in a new tmux window via `tfmux attach`. Prints `spawned / worktree / branch / tfmux / ping-back`.

**3. Surface the output, then return to idle.** Don't poll or babysit (**see the no-poll callout up top**) — the **ping comes to you**. More features? fire the next one; each uses its own `tfmux-<slug>` session and pings back independently.

## What you just fired

| Owner | Does |
|-------|------|
| **the script** (one shot) | worktree on `feat/<slug>` · quarantine hook-copied `.llm` · `dotllm trust` + `init` · derive `tfmux-<slug>` · **bind THIS pane as tfmux `mediator`** · spawn detached codex (`TFMUX_SESSION` exported) · **`tfmux send` the `$sf-auto` task (verified)** · arm the done-ping · auto-attach window · return |
| **codex** (`$sf-auto`, unattended in the worktree) | request → design → implement (test-first, commit per task) → review → fix → **open PR → squash-merge to main** → **`tfmux send mediator` a status line** |
| **you** (idle in your pane) | stay put → **catch codex's ping** → **then always finish the run yourself:** verify the squash-merge landed (`gh pr list --head feat/<slug> --state merged`) → run the printed `factory-cleanup.sh ... --pr <PR>` (it syncs local main, removes the worktree, kills the session). Never hand the finish to a human. (Didn't actually merge, or ⚠️ blocked → don't clean up; surface it.) |

> codex uses the `$sf-auto` trigger. **Don't over-constrain the feature** (*"change only X"*) — that can make codex do a literal edit and skip the loop. State *what to build*, verbatim.

## How the ping-back works

`tfmux` has **no global current session** — routing is by an explicit named session. The script derives one per run (`tfmux-<slug>`), binds **your** pane as `mediator` in it, and exports `TFMUX_SESSION` into codex's shell. codex reaches you with its **final** action:

```bash
tfmux send mediator --session tfmux-<slug> --text '✅ <slug> merged: <PR url>'   # or: ⚠️ <slug> blocked: <one-line reason>
```

That line lands in **your** pane. **Stay idle to catch it.** Ping-back needs you **in tmux** (the script binds your pane via `--here`); if you're not, the script warns and degrades to fire-and-forget — then you check the PR yourself.

## Prerequisites (check once)
- **`tfmux` — REQUIRED** (delivers the task *and* carries the ping-back). `tmux` on PATH too. In git mode, `wt`, `dotllm`, `git`, and `python3` are preflighted; outside git, dotllm is tried only best-effort.
- `gh` authenticated in git mode — codex opens + squash-merges the PR, and the cleanup helper verifies the merged PR; the script preflights that a git **remote** exists. Outside git, there is no PR or squash-merge.
- The **`shadowfax`** bundle loaded — codex needs `$sf-auto` in `~/.codex/skills/`.
- **Ping-back needs you in tmux.** Not in tmux → no mediator pane → no ping; the script warns.

## Checking on it (you'll usually be pinged first)

```bash
gh pr list --head feat/<slug> --state all       # opened a PR? merged?
git ls-remote origin -h refs/heads/main          # did remote main advance?
```

- **Got `✅ <slug> merged: …`** → **finish it yourself, always — never leave it for a human.** Verify the squash-merge actually landed (`gh pr list --head feat/<slug> --state merged`), then run the printed `factory-cleanup.sh ... --pr <PR_URL_OR_NUMBER>` command — it ff-only syncs local main, proves the merge commit is on main, removes the worktree, then kills the session (steps 2–3 in one safe pass). If the PR did **not** actually merge, surface that to the human instead of cleaning up blind.
- **Running / PR open** → leave it; watch with `tmux attach -t <NAME>`.
- **No commit, no PR** (over-constrained ask) → re-send: `tfmux send <NAME> --session tfmux-<slug> --text '$sf-auto run your full loop end-to-end to a squash-merged PR for: <feature>'`.

## Notes
- **Worktree-first (load-bearing):** the launcher always finishes the worktree — create `feat/<slug>` → quarantine `.llm` → `dotllm trust` → `dotllm init` — and **only then** spawns the tmux session with cwd already set to the worktree (`tmux new-session -c "$WT"`). codex comes up already inside `$WT`. `assert_worktree_ready "$WT"` aborts loudly on any reordering; `scripts/test-launch-order.sh` pins it.
- **Per-run session:** each launch gets its own `tfmux-<slug>` session, so runs never collide — fire as many as you like, each pings back on its own. Override with `SF_TFMUX_SESSION`.
- **Launch config:** the codex command lives near the top of `scripts/codex-go.sh` (`CONFIG POINT`). Default is `codexy` (xhigh). Override per-run with `--effort`, or wholesale with `SF_CODEXGO_CMD` / `SF_CODEXGO_EFFORT`.
- **Verified delivery:** after `tfmux send` the script confirms codex actually **started** and retries up to 5× — guards a readiness race that could leave the loop unstarted.
- **Cleanup split:** the launcher prints the exact `factory-cleanup.sh --session <NAME> --worktree <WT> --branch <BRANCH> --repo <REPO> --pr <PR_URL_OR_NUMBER>` command because it knows the run handles. The bundled `factory-cleanup.sh` helper (in the `tmux-factory-codex-go` skill, fake-binary tested) owns the safety checks and deletion sequence — verify merged PR, ff-only update main, prove the merge commit is on main, remove the worktree, then kill tmux. Do not duplicate that logic in skill prose.
- **`.llm` is for codex, not routing:** `dotllm init` gives the worktree a project `.llm` for `$sf-auto`'s scratch; the ping is routed by the named tfmux session, not the `.llm`.
- **Preview:** `"$eng" --dry-run "<feature>"` prints the exact steps (including whether ping-back is on) without spawning.
- **Slug + sessions:** slug derived from the feature (kebab, ~6 words) unless `--slug`; tmux session = `sf_<slug_with_underscores>_codex`, tfmux session = `tfmux-<slug>`.

## Crib sheet
- `"$eng" "<feature>"` — fire it. `--task-file` · `--slug` · `--effort low|medium|high|xhigh` · `--dry-run`.
- **Ping-back:** codex `tfmux send mediator`'s a `✅ <slug> merged: <PR url>` (or `⚠️ blocked`) line to **your** pane — stay idle (needs you in tmux).
- **After the ✅ ping (always finish it):** verify `gh pr list --head feat/<slug> --state merged` → run the printed `factory-cleanup.sh ... --pr <PR>` (syncs main + removes worktree + kills session). Not merged / ⚠️ blocked → surface it, don't clean up blind.
- `gh pr list --head feat/<slug> --state all` · `git ls-remote origin -h refs/heads/main` — check authoritatively.
- `factory-cleanup.sh --session sf_<slug>_codex --worktree <WT> --branch feat/<slug> --repo <REPO> --pr <PR_URL_OR_NUMBER>` — safe cleanup after merge; use the printed command.
- `tmux attach -t sf_<slug>_codex` — watch live if needed.
- `bash -n scripts/codex-go.sh && bash -n scripts/factory-cleanup.sh` · `scripts/test-factory-cleanup.sh` · `scripts/test-dotllm-quarantine.sh` · `scripts/test-launch-order.sh` · `"$eng" --dry-run --slug smoke "smoke feature"` — local checks after edits.
