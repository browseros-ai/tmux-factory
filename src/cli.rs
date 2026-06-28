//! Clap surface and command handlers.

use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};

use crate::app::App;
use crate::store::{self, rfc3339, Store};
use crate::target::{validate_kind, validate_name, validate_role, Target};

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

#[derive(Subcommand)]
pub enum Command {
    /// Bind a named target to a canonical tmux pane.
    Bind(BindArgs),
}

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
