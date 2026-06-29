use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use clap::Parser;

use tfmux::app::{base_dir_from_env, App};
use tfmux::cli::{Cli, Command};
use tfmux::git::{CliGit, CliGitHub, Git, GitHub};
use tfmux::mux::{Mux, Tmux};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if matches!(&cli.command, Command::Attach(_) | Command::Detach(_)) {
        return run_without_store(cli);
    }

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
    let new_git = || -> anyhow::Result<Box<dyn Git>> { Ok(Box::new(CliGit::from_env()?)) };
    let new_github = || -> anyhow::Result<Box<dyn GitHub>> { Ok(Box::new(CliGitHub::from_env()?)) };
    let read_stdin = || -> anyhow::Result<String> {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    };
    let new_buffer_name = || -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        format!("tfmux-send-{nanos}")
    };
    let sleep = |duration| std::thread::sleep(duration);
    let mut out = io::stdout();

    let mut app = App {
        base_dir,
        env: &env_fn,
        cwd,
        now: &now_fn,
        new_mux: &new_mux,
        new_git: &new_git,
        new_github: &new_github,
        read_stdin: &read_stdin,
        new_buffer_name: &new_buffer_name,
        sleep: &sleep,
        out: &mut out,
    };

    match tfmux::run(&mut app, cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

fn run_without_store(cli: Cli) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => return fail(e.into()),
    };
    let env_fn = |k: &str| std::env::var(k).ok();
    let now_fn = || Utc::now();
    let new_mux = || -> anyhow::Result<Box<dyn Mux>> { Ok(Box::new(Tmux::from_env()?)) };
    let new_git = || -> anyhow::Result<Box<dyn Git>> { Ok(Box::new(CliGit::from_env()?)) };
    let new_github = || -> anyhow::Result<Box<dyn GitHub>> { Ok(Box::new(CliGitHub::from_env()?)) };
    let read_stdin = || -> anyhow::Result<String> { Ok(String::new()) };
    let new_buffer_name = || -> String { String::new() };
    let sleep = |_duration| {};
    let mut out = io::stdout();

    let mut app = App {
        base_dir: PathBuf::new(),
        env: &env_fn,
        cwd,
        now: &now_fn,
        new_mux: &new_mux,
        new_git: &new_git,
        new_github: &new_github,
        read_stdin: &read_stdin,
        new_buffer_name: &new_buffer_name,
        sleep: &sleep,
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
