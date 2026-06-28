//! Wiring/DI context threaded into command handlers.
//!
//! Everything that touches the outside world (env, clock, tmux, stdout) is a
//! field here so tests can inject fakes instead of mutating process globals.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use crate::mux::Mux;

/// Dependency-injection context threaded into every command handler.
pub struct App<'a> {
    /// `$TFMUX_HOME` or `~/.tfmux`; a tempdir in tests.
    pub base_dir: PathBuf,
    /// Environment lookup (TMUX, TMUX_PANE, TFMUX_SESSION).
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Working directory used to locate `.llm/tfmux-session`.
    pub cwd: PathBuf,
    /// Clock for timestamps.
    pub now: &'a dyn Fn() -> DateTime<Utc>,
    /// Lazily-built tmux backend (constructed only when a command needs it, so
    /// pure-validation failure paths never shell out to tmux).
    pub new_mux: &'a dyn Fn() -> Result<Box<dyn Mux>>,
    /// Read all text from stdin when a command explicitly asks for `-`.
    pub read_stdin: &'a dyn Fn() -> Result<String>,
    /// Generate a tmux buffer name for one send attempt.
    pub new_buffer_name: &'a dyn Fn() -> String,
    /// Sleep after submitting text so tmux/TUI capture can settle.
    pub sleep: &'a dyn Fn(Duration),
    /// Success output sink.
    pub out: &'a mut dyn Write,
}

/// Resolve the storage base dir: `$TFMUX_HOME` if set and non-empty, else
/// `$HOME/.tfmux`.
pub fn base_dir_from_env() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("TFMUX_HOME") {
        let home = home.trim();
        if !home.is_empty() {
            return Ok(PathBuf::from(home));
        }
    }
    let home =
        std::env::var("HOME").map_err(|_| anyhow!("HOME is not set; set TFMUX_HOME or HOME"))?;
    Ok(PathBuf::from(home).join(".tfmux"))
}
