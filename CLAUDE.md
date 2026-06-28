# tfmux — agent & contributor guidelines

`tfmux` is a small, **synchronous** Rust CLI that lets a *mediator* drive a fleet
of *agents*, each in its own tmux pane: it **bind**s panes to names and (later)
**send**s them prompts. Inspired by `riff3`; shares no state, paths, or commands
with it.

**Scope: `bind` only.** `send` is the planned next feature. Add no stubs,
placeholder modules, TODO bodies, or dead scaffolding — let a failing test pull
each new piece of code into existence.

These rules MUST be followed by all AI coding agents and contributors.

## Commands

```bash
cargo test                                  # in-module unit tests + e2e tests/cli.rs
cargo fmt                                    # required before every commit
cargo clippy --all-targets -- -D warnings   # must pass clean
cargo run -- bind NAME --tmux sess:1.0 --session demo
```

CLI surface:

```
tfmux bind NAME (--here | --tmux TARGET) [--role mediator|agent]
               [--kind claude|codex|generic] [--session NAME] [--json]
```

## Architecture (module ownership — keep these boundaries)

| File | Owns | Rule |
|---|---|---|
| `target.rs` | `Target` data + `validate_name`/`validate_role`/`validate_kind` | pure; **no I/O** |
| `store.rs` | `~/.tfmux` layout, `Session`, session resolution, atomic writes | all filesystem state |
| `mux.rs` | `Mux` trait + `Tmux` backend + pane parsing | the only code that shells out to tmux |
| `cli.rs` | clap `Cli`/`Command`/`*Args` + handlers + output formatting | wires flags → store/mux |
| `app.rs` | `App` DI context + `base_dir_from_env` | the only door for env/cwd/clock/tmux/stdout |
| `main.rs` | composition root | builds the real `App`; prints `error: {:#}`, exits 1 |
| `lib.rs` | module decls + `run(app, cli)` | dispatch only |

Adding a command later = one `Command` arm + one handler (+ grow the `Mux` trait /
extend `App` only if that feature needs it). Don't cross the ownership above.

## Core principles

- Write only what the problem needs — no speculative abstraction, no technical
  debt, DRY. Prioritize clarity and maintainability over cleverness.
- "Optimized" here means clean data flow and no needless allocations/clones.
  `tfmux` is I/O-bound (file writes + a tmux subprocess), so **SIMD and
  threading do not apply** — reach for `rayon`/SIMD only if profiling ever shows
  a real hotspot.
- Do a second review pass before handing off: leave the tree `fmt`-clean,
  `clippy`-clean, and green.
- The reader is a Python expert but a Rust novice — add brief comments on
  Rust-specific nuances (ownership/borrowing, lifetimes, trait objects,
  interior mutability) that a Python developer wouldn't expect.

## Code style & formatting

`cargo fmt` (rustfmt, default style) is the **source of truth** — run it before
committing; never hand-format around it. `cargo clippy --all-targets -- -D
warnings` MUST pass (enforce `-D warnings` in CI, never `#![deny(warnings)]` in
source).

- 4-space indentation, never tabs; max line width 100 characters.
- `snake_case` for functions/variables/modules, `PascalCase` for types/traits,
  `SCREAMING_SNAKE_CASE` for constants. Meaningful, descriptive names.
- Block indent (not visual indent); trailing comma on the last item of a
  multi-line list; at most one blank line between items; no trailing whitespace.
- Imports are sorted (version-sorted: `u8` before `u16`); order std → external →
  `crate`/`super`, with local imports in their own block. **NEVER** use wildcard
  imports except preludes and `use super::*;` inside `#[cfg(test)]` modules.
- One `#[derive(...)]` per item (combine the traits); one attribute per line;
  doc comments go **before** attributes.
- **NEVER** use emoji or emoji-like unicode (e.g. ✓, ✗) in code or program
  output — the only exception is tests that specifically exercise multibyte text.

## Documentation

- **MUST** put a doc comment (`///`) on every public function, struct, enum, and
  method, documenting parameters, return values, and errors. Use `//!` only for
  module/crate docs at the top of a file. Keep comments current with the code.
- Use the sectioned form for non-trivial APIs; `# Examples` are compiled and run
  by `cargo test`, so keep them valid. Trivial constructors may be one line.
- Comments are complete sentences (capital start, `.` end; a short inline note
  may skip the period) and ≤ 80 chars on comment-only lines.

```rust
/// Resolve the session name from the precedence chain.
///
/// # Arguments
/// * `flag` - value of `--session`, if given
/// * `env` - value of `TFMUX_SESSION`, if set
/// * `marker` - first line of `.llm/tfmux-session`, if present
///
/// # Errors
/// Returns an error if none resolve, or the chosen name is not a path-safe token.
pub fn resolve_session_name(/* … */) -> Result<String> {
```

## Types & errors

- This is an **application**: use `anyhow` (`bail!`, `?`, `.context()`); `main`
  prints `error: {:#}` to stderr and exits 1. Introduce a `thiserror` enum only
  if a real typed/library error surface appears — don't add one preemptively.
- **NEVER** `.unwrap()` in production paths. Use `.expect("…")` only for true
  invariants, always with a message (e.g. the post-xor "guaranteed target" in
  `bind`). `.unwrap()` is fine in tests.
- Lean on the type system: prefer `Option<T>` over sentinel values; use newtypes
  to separate semantically different values of the same underlying type;
  pattern-match exhaustively — a catch-all `_` arm is acceptable only when the
  set is genuinely open (e.g. validating an arbitrary `&str`).

## Functions & structs

- One responsibility per function/type. Prefer borrowing (`&T`/`&mut T`) over
  owning; return early to cut nesting; ≤ 5 parameters (use a struct beyond that).
- Derive `Debug, Clone, PartialEq` where sensible; `Default` when a sensible
  default exists.
- Fields are private by default — **except** the serde data records (`Target`,
  `Session`, `PaneRef`) and the `App` DI context, whose public fields are
  intentional. Keep `Store.base_dir`, `Tmux.bin`, and the like private.

## Testing

- **TDD, strictly:** write the failing test first, watch it fail for the right
  reason, then write the minimal code to pass. Arrange-Act-Assert. Never commit
  commented-out tests.
- Unit tests in `#[cfg(test)] mod tests` beside the code; end-to-end CLI tests in
  `tests/cli.rs`.
- Fake the **tmux** boundary through the `Mux` trait (`FakeMux`, or a fake-binary
  shell script when asserting exact argv). Do **not** mock the filesystem —
  exercise it against real `tempfile` temp dirs. Tests never set process env
  vars and never touch real tmux.

## Dependencies

- Keep them minimal: tmux-only, synchronous, no async/HTTP; read `HOME` directly
  (no `dirs`). Pin versions in `Cargo.toml`. Add a crate only with the feature
  that first needs it (e.g. `regex` arrives with `send`, not before).

## Rust idioms & performance

- Call `.clone()` explicitly; avoid hidden clones in closures/iterators. Prefer
  `&str` over `String`, `Cow<'_, str>` when ownership is conditional, and
  `Vec::with_capacity` when the size is known.
- Prefer iterators/adapters and `if let`/`while let` over manual loops; use
  `enumerate()` rather than a hand-rolled counter; use `format!` for strings.

## Security

- **NEVER** hardcode secrets, tokens, or passwords. If configuration is ever
  needed, read it from env vars (`std::env`/`dotenvy`) with `.env` git-ignored.
  Never log secrets or PII.

## Project-specific rules (the tfmux core)

- **DI everywhere:** handlers reach the outside world *only* through `App` (env
  lookup, cwd, clock, `new_mux`, `out`). `App::new_mux` is **lazy** — built only
  when a command needs tmux, so validation-failure paths never shell out (tests
  assert it was not built).
- **tmux:** go through `Mux`/`Tmux` only. Read `TMUX_PANE`; **never** parse
  `TMUX`. Canonicalize with
  `display-message -p -t <t> "#{pane_id}\t#{session_name}\t#{window_index}\t#{pane_index}"`
  and require a `%N` id.
- **Storage:** base dir is `$TFMUX_HOME` else `~/.tfmux`; layout
  `~/.tfmux/<YYYY-MM-DD>/<session>/{session.json, targets/<name>.json}`. Write
  atomically (same-dir temp + rename), pretty JSON + trailing newline. **NEVER**
  create a global `~/.tfmux/current`; session identity travels with the pane
  (`--session` > `TFMUX_SESSION` > `.llm/tfmux-session`).
- **Names** (target and session) are path-safe tokens — always via `validate_name`.
- Validation/argument errors MUST fail **before** any state is written.

## Not in scope here

`tfmux` is a sync CLI, so the source template's rules for web (`axum`), TUIs
(`ratatui`/`crossterm`), WASM/front-end (`dioxus`/Pico CSS), async (`tokio`),
data frames (`polars`), Python bindings (`PyO3`/`maturin`), and progress bars
(`indicatif`) do not apply. If scope ever grows into one of those, adopt that
section from the template at that point.

## Before committing

- [ ] `cargo test` passes
- [ ] `cargo build` has no warnings; `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo fmt --check` is clean
- [ ] every public item has a doc comment
- [ ] no commented-out code, `dbg!`, or stray debug `println!`
- [ ] no hardcoded credentials; `.llm/` is not committed (it is a gitignored symlink)
