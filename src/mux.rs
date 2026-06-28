//! tmux interaction: resolving a target to a canonical pane.
//!
//! Bind only needs `resolve_pane`. The `Mux` trait is the seam tests inject a
//! fake through; the `send` feature will grow it with buffer/paste/capture
//! methods later.

use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

/// Canonical pane information resolved from a tmux target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRef {
    /// The tmux target string the caller passed.
    pub input: String,
    /// Canonical `%N` pane id.
    pub pane_id: String,
    pub session: String,
    pub window: String,
    pub pane_index: String,
}

/// Seam over the tmux multiplexer.
pub trait Mux {
    /// Resolve a tmux target string to canonical pane information.
    fn resolve_pane(&self, target: &str) -> Result<PaneRef>;
}

/// Real tmux backend that shells out to the `tmux` binary.
pub struct Tmux {
    bin: PathBuf,
}

impl Tmux {
    /// Construct from an explicit binary path (used by tests and `from_env`).
    pub fn new(bin: PathBuf) -> Self {
        Self { bin }
    }

    /// Resolve the tmux binary from `TFMUX_TMUX_BIN` (else `tmux`) on `PATH`.
    pub fn from_env() -> Result<Self> {
        let name = std::env::var("TFMUX_TMUX_BIN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "tmux".to_string());
        let bin = which::which(&name).map_err(|e| {
            anyhow!("tmux binary \"{name}\" not found; install tmux or set TFMUX_TMUX_BIN: {e}")
        })?;
        Ok(Self::new(bin))
    }

    /// Run the tmux binary with `args`, returning stdout on success.
    /// On failure the error carries the trimmed combined stdout+stderr.
    fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.bin)
            .args(args)
            .output()
            .with_context(|| format!("running {}", self.bin.display()))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let mut detail = String::from_utf8_lossy(&output.stdout).into_owned();
        detail.push_str(&String::from_utf8_lossy(&output.stderr));
        let detail = detail.trim();
        let sub = args.first().copied().unwrap_or("tmux");
        if detail.is_empty() {
            bail!("tmux {sub} failed");
        }
        bail!("tmux {sub} failed: {detail}");
    }
}

impl Mux for Tmux {
    fn resolve_pane(&self, target: &str) -> Result<PaneRef> {
        let target = target.trim();
        if target.is_empty() {
            bail!("tmux target is required");
        }
        let out = self.run(&[
            "display-message",
            "-p",
            "-t",
            target,
            "#{pane_id}\t#{session_name}\t#{window_index}\t#{pane_index}",
        ])?;
        parse_pane_ref(target, &out)
    }
}

/// Parse the tab-separated `display-message` output into a `PaneRef`.
fn parse_pane_ref(target: &str, raw: &str) -> Result<PaneRef> {
    let trimmed = raw.trim();
    let fields: Vec<&str> = trimmed.split('\t').collect();
    if fields.len() < 4 {
        bail!("tmux target \"{target}\" resolved to malformed pane metadata \"{trimmed}\"");
    }
    let pane_id = fields[0].trim().to_string();
    if !pane_id.starts_with('%') {
        bail!("tmux target \"{target}\" resolved to \"{pane_id}\", want a %N pane id");
    }
    Ok(PaneRef {
        input: target.to_string(),
        pane_id,
        session: fields[1].trim().to_string(),
        window: fields[2].trim().to_string(),
        pane_index: fields[3].trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    // ---- parse_pane_ref (pure) ----

    #[test]
    fn parse_valid_four_fields() {
        let r = parse_pane_ref("sess:1.0", "%5\tsess\t1\t0\n").unwrap();
        assert_eq!(
            r,
            PaneRef {
                input: "sess:1.0".into(),
                pane_id: "%5".into(),
                session: "sess".into(),
                window: "1".into(),
                pane_index: "0".into(),
            }
        );
    }

    #[test]
    fn parse_requires_percent_pane_id() {
        let err = parse_pane_ref("sess:1.0", "5\tsess\t1\t0")
            .unwrap_err()
            .to_string();
        assert!(err.contains("%N pane id"), "got: {err}");
        assert!(err.contains("\"5\""), "got: {err}");
    }

    #[test]
    fn parse_rejects_malformed_metadata() {
        let err = parse_pane_ref("sess:1.0", "%5\tsess")
            .unwrap_err()
            .to_string();
        assert!(err.contains("malformed"), "got: {err}");
    }

    // ---- Tmux::resolve_pane (subprocess via a fake tmux binary) ----

    #[cfg(unix)]
    #[test]
    fn resolve_pane_issues_exact_display_message_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_fake_tmux(dir.path(), b"%5\tsess\t1\t0\n");
        let tmux = Tmux::new(bin);

        let r = tmux.resolve_pane("sess:1.0").unwrap();
        assert_eq!(r.pane_id, "%5");
        assert_eq!(r.session, "sess");
        assert_eq!(r.window, "1");
        assert_eq!(r.pane_index, "0");
        assert_eq!(r.input, "sess:1.0");

        let recorded = fs::read_to_string(dir.path().join("argv.txt")).unwrap();
        let args: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            args,
            vec![
                "display-message",
                "-p",
                "-t",
                "sess:1.0",
                "#{pane_id}\t#{session_name}\t#{window_index}\t#{pane_index}",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_pane_surfaces_command_failure_detail() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_failing_tmux(dir.path(), "unknown pane: bogus");
        let tmux = Tmux::new(bin);

        let err = tmux.resolve_pane("bogus").unwrap_err().to_string();
        assert!(err.contains("unknown pane: bogus"), "got: {err}");
    }

    #[test]
    fn resolve_pane_rejects_empty_target() {
        // Empty target must fail before any subprocess runs.
        let tmux = Tmux::new(PathBuf::from("/nonexistent/tmux"));
        let err = tmux.resolve_pane("   ").unwrap_err().to_string();
        assert!(err.contains("tmux target is required"), "got: {err}");
    }

    // ---- fake-binary helpers ----

    #[cfg(unix)]
    fn write_fake_tmux(dir: &Path, response: &[u8]) -> PathBuf {
        let argv = dir.join("argv.txt");
        let resp = dir.join("response.bin");
        fs::write(&resp, response).unwrap();
        let script = format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv:?}\ncat {resp:?}\n");
        write_script(dir.join("faketmux"), &script)
    }

    #[cfg(unix)]
    fn write_failing_tmux(dir: &Path, stderr: &str) -> PathBuf {
        let script = format!("#!/bin/sh\necho {stderr:?} >&2\nexit 1\n");
        write_script(dir.join("faketmux-fail"), &script)
    }

    #[cfg(unix)]
    fn write_script(path: PathBuf, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        fs::write(&path, body).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }
}
