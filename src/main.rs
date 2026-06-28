use std::io;
use std::process::ExitCode;

use chrono::Utc;
use clap::Parser;

use tfmux::app::{base_dir_from_env, App};
use tfmux::cli::Cli;
use tfmux::mux::{Mux, Tmux};

fn main() -> ExitCode {
    let cli = Cli::parse();

    let base_dir = match base_dir_from_env() {
        Ok(dir) => dir,
        Err(e) => return fail(e),
    };
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => return fail(e.into()),
    };

    let env_fn = |k: &str| std::env::var(k).ok();
    let now_fn = || Utc::now();
    let new_mux = || -> anyhow::Result<Box<dyn Mux>> { Ok(Box::new(Tmux::from_env()?)) };
    let mut out = io::stdout();

    let mut app = App {
        base_dir,
        env: &env_fn,
        cwd,
        now: &now_fn,
        new_mux: &new_mux,
        out: &mut out,
    };

    match tfmux::run(&mut app, cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

fn fail(e: anyhow::Error) -> ExitCode {
    eprintln!("error: {e:#}");
    ExitCode::FAILURE
}
