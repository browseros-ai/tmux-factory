pub mod app;
pub mod cli;
pub mod mux;
pub mod store;
pub mod target;

use anyhow::Result;

use app::App;
use cli::{Cli, Command};

/// Dispatch a parsed CLI invocation against the given app context.
pub fn run(app: &mut App, cli: Cli) -> Result<()> {
    match cli.command {
        Command::Bind(args) => cli::bind(app, &args),
        Command::Send(args) => cli::send(app, &args),
        Command::Unbind(args) => cli::unbind(app, &args),
        Command::Targets(args) => cli::targets(app, &args),
    }
}
