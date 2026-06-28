//! Clap surface and command handlers.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::fs;
use std::path::PathBuf;

use crate::app::App;
use crate::store::{self, rfc3339, Store};
use crate::target::{validate_kind, validate_name, validate_role, Target};

/// Parsed top-level CLI invocation.
#[derive(Parser)]
#[command(
    name = "tfmux",
    version,
    about = "Drive a tmux agent fleet: bind named panes."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Subcommand)]
pub enum Command {
    /// Bind a named target to a canonical tmux pane.
    Bind(BindArgs),
}

/// Arguments for `tfmux bind`.
#[derive(Args)]
pub struct BindArgs {
    /// Target name (a single path-safe token).
    pub name: String,
    /// Bind the current tmux pane (reads TMUX_PANE).
    #[arg(long)]
    pub here: bool,
    /// tmux target to resolve and bind (e.g. %5 or sess:1.0).
    #[arg(long, value_name = "TARGET")]
    pub tmux: Option<String>,
    /// Target role.
    #[arg(long, default_value = "agent")]
    pub role: String,
    /// Target kind.
    #[arg(long, default_value = "generic")]
    pub kind: String,
    /// Override session selection.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
    /// Print the stored target as JSON instead of the text summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `tfmux send`.
#[derive(Args)]
pub struct SendArgs {
    /// Target name (a single path-safe token).
    pub name: String,
    /// Text payload to send.
    #[arg(long)]
    pub text: Option<String>,
    /// File to read as the send payload.
    #[arg(long, value_name = "FILE")]
    pub file: Option<PathBuf>,
    /// Read the send payload from stdin when this positional is `-`.
    #[arg(value_name = "-", value_parser = ["-"])]
    pub stdin_marker: Option<String>,
    /// Override session selection.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
}

/// Resolve and read the payload for `tfmux send`.
///
/// Exactly one source must be present: `--text`, `--file`, or `-` for stdin.
/// The returned string is guaranteed to be non-empty.
///
/// # Errors
/// Returns an error when the source selection is invalid, the selected source
/// is empty, or the file/stdin reader fails.
pub fn resolve_send_payload(
    args: &SendArgs,
    read_stdin: &dyn Fn() -> Result<String>,
) -> Result<String> {
    let source_count = usize::from(args.text.is_some())
        + usize::from(args.file.is_some())
        + usize::from(args.stdin_marker.is_some());
    match source_count {
        0 => bail!("no input given: pass --text, --file, or -"),
        1 => {}
        _ => bail!("choose exactly one input source: --text, --file, or -"),
    }

    let payload = if let Some(text) = &args.text {
        text.clone()
    } else if let Some(path) = &args.file {
        if path.as_os_str().is_empty() {
            bail!("--file requires a path");
        }
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    } else {
        read_stdin()?
    };

    if payload.is_empty() {
        bail!("empty payload; pass non-empty text");
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::fs;
    use std::path::PathBuf;

    fn args_with_text(text: Option<&str>) -> SendArgs {
        SendArgs {
            name: "agent1".to_string(),
            text: text.map(str::to_string),
            file: None,
            stdin_marker: None,
            session: None,
        }
    }

    fn stdin_reader(value: &'static str) -> impl Fn() -> Result<String> {
        move || Ok(value.to_string())
    }

    #[test]
    fn send_payload_rejects_no_input_source() {
        let args = SendArgs {
            name: "agent1".to_string(),
            text: None,
            file: None,
            stdin_marker: None,
            session: None,
        };
        let err = resolve_send_payload(&args, &stdin_reader("ignored"))
            .unwrap_err()
            .to_string();
        assert_eq!(err, "no input given: pass --text, --file, or -");
    }

    #[test]
    fn send_payload_accepts_text_source() {
        let payload =
            resolve_send_payload(&args_with_text(Some("hello")), &stdin_reader("ignored")).unwrap();
        assert_eq!(payload, "hello");
    }

    #[test]
    fn send_payload_rejects_multiple_sources() {
        let args = SendArgs {
            name: "agent1".to_string(),
            text: Some("hello".to_string()),
            file: Some(PathBuf::from("message.txt")),
            stdin_marker: Some("-".to_string()),
            session: None,
        };
        let err = resolve_send_payload(&args, &stdin_reader("ignored"))
            .unwrap_err()
            .to_string();
        assert_eq!(err, "choose exactly one input source: --text, --file, or -");
    }

    #[test]
    fn send_payload_rejects_empty_text() {
        let err = resolve_send_payload(&args_with_text(Some("")), &stdin_reader("ignored"))
            .unwrap_err()
            .to_string();
        assert_eq!(err, "empty payload; pass non-empty text");
    }

    #[test]
    fn send_payload_rejects_empty_file_path() {
        let args = SendArgs {
            name: "agent1".to_string(),
            text: None,
            file: Some(PathBuf::from("")),
            stdin_marker: None,
            session: None,
        };
        let err = resolve_send_payload(&args, &stdin_reader("ignored"))
            .unwrap_err()
            .to_string();
        assert_eq!(err, "--file requires a path");
    }

    #[test]
    fn send_payload_reads_file_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("message.txt");
        fs::write(&path, "hello from file").unwrap();
        let args = SendArgs {
            name: "agent1".to_string(),
            text: None,
            file: Some(path),
            stdin_marker: None,
            session: None,
        };

        let payload = resolve_send_payload(&args, &stdin_reader("ignored")).unwrap();
        assert_eq!(payload, "hello from file");
    }

    #[test]
    fn send_payload_reads_stdin_source() {
        let args = SendArgs {
            name: "agent1".to_string(),
            text: None,
            file: None,
            stdin_marker: Some("-".to_string()),
            session: None,
        };
        let payload = resolve_send_payload(&args, &stdin_reader("from stdin")).unwrap();
        assert_eq!(payload, "from stdin");
    }
}

/// Handle `tfmux bind`: resolve the session and tmux pane, then persist the
/// target. Validation and argument errors fail before any state is written.
///
/// # Errors
/// Returns an error on an invalid name/role/kind, when not exactly one of
/// `--here`/`--tmux` is given, when no session name resolves, or when tmux
/// cannot resolve the pane.
pub fn bind(app: &mut App, args: &BindArgs) -> Result<()> {
    validate_name(&args.name)?;
    validate_role(&args.role)?;
    validate_kind(&args.kind)?;

    // Exactly one target source. A blank `--tmux ""` counts as absent.
    let tmux_target = args
        .tmux
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if args.here == tmux_target.is_some() {
        bail!("choose exactly one target source: --here or --tmux TARGET");
    }

    // Resolve the session name (read-only; precedence + path-safe validation).
    let marker = store::read_session_marker(&app.cwd)?;
    let env_session = (app.env)("TFMUX_SESSION");
    let session_name = store::resolve_session_name(
        args.session.as_deref(),
        env_session.as_deref(),
        marker.as_deref(),
    )?;

    // Resolve the tmux input string. For `--here` this validates the tmux env
    // *before* the mux is built, so that failure path never shells out.
    let input = if args.here {
        let in_tmux = (app.env)("TMUX")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        if in_tmux.is_none() {
            bail!("--here requires TMUX and TMUX_PANE; run from inside tmux or use --tmux TARGET");
        }
        match (app.env)("TMUX_PANE")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            Some(pane) => pane,
            None => bail!("inside tmux but TMUX_PANE is empty; use --tmux TARGET"),
        }
    } else {
        tmux_target
            .expect("xor check guarantees a tmux target")
            .to_string()
    };

    // Look up an existing session before doing anything that writes.
    let store = Store::new(app.base_dir.clone());
    let existing = store.find_session_dir(&session_name)?;

    // Build the mux only now, then resolve the canonical pane.
    let mux = (app.new_mux)()?;
    let pane = mux.resolve_pane(&input)?;

    let now = (app.now)();
    let target = Target {
        name: args.name.clone(),
        role: args.role.clone(),
        kind: args.kind.clone(),
        input,
        pane_id: pane.pane_id,
        session: pane.session,
        window: pane.window,
        pane_index: pane.pane_index,
        bound_at: rfc3339(now),
    };

    // Writes: create the session on first bind, then save the target.
    let session_dir = match existing {
        Some(dir) => dir,
        None => store.create_session(&session_name, now)?,
    };
    store.save_target(&session_dir, &target)?;

    if args.json {
        writeln!(app.out, "{}", serde_json::to_string_pretty(&target)?)?;
    } else {
        writeln!(
            app.out,
            "bound {} -> {} ({}:{}.{})",
            target.name, target.pane_id, target.session, target.window, target.pane_index
        )?;
    }
    Ok(())
}
