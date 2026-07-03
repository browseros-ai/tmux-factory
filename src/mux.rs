//! tmux interaction: resolving panes and delivering text through buffers.
//!
//! The `Mux` trait is the seam tests inject a fake through; the real `Tmux`
//! backend owns every subprocess call and exact tmux argv shape.

use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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

    /// Load `text` into a named tmux buffer.
    fn load_buffer(&self, name: &str, text: &str) -> Result<()>;

    /// Paste a named tmux buffer into `pane_id`.
    fn paste_buffer(&self, name: &str, pane_id: &str) -> Result<()>;

    /// Delete a named tmux buffer.
    fn delete_buffer(&self, name: &str) -> Result<()>;

    /// Send Enter to `pane_id`.
    fn send_enter(&self, pane_id: &str) -> Result<()>;

    /// Capture text from `pane_id`, optionally including scrollback lines.
    fn capture_pane(&self, pane_id: &str, scrollback: i32) -> Result<String>;

    /// Check that a tmux session exists.
    fn has_session(&self, session: &str) -> Result<()>;

    /// Open a new tmux window that attaches to `session` with `TMUX` unset.
    fn attach_session_in_new_window(
        &self,
        session: &str,
        window_name: &str,
        socket: &str,
    ) -> Result<()>;
}

/// Real tmux backend that shells out to the `tmux` binary.
pub struct Tmux {
    bin: PathBuf,
    socket: String,
}

impl Tmux {
    /// Construct from an explicit binary path and resolved socket settings.
    pub fn new(bin: PathBuf, socket: &str, main_socket: &str) -> Self {
        Self {
            bin,
            socket: resolve_socket(socket, main_socket),
        }
    }

    /// Resolve the tmux binary from `TFMUX_TMUX_BIN` (else `tmux`) on `PATH`.
    pub fn from_env(socket: &str) -> Result<Self> {
        let name = std::env::var("TFMUX_TMUX_BIN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "tmux".to_string());
        let main_socket = std::env::var("TFMUX_MAIN_SOCKET").unwrap_or_default();
        let bin = which::which(&name).map_err(|e| {
            anyhow!("tmux binary \"{name}\" not found; install tmux or set TFMUX_TMUX_BIN: {e}")
        })?;
        Ok(Self::new(bin, socket, &main_socket))
    }

    /// Run the tmux binary with `args`, returning stdout on success.
    /// On failure the error carries the trimmed combined stdout+stderr.
    fn run(&self, args: &[&str]) -> Result<String> {
        self.run_with_stdin(args, None)
    }

    /// Run the tmux binary with optional stdin, returning stdout on success.
    fn run_with_stdin(&self, args: &[&str], stdin: Option<&str>) -> Result<String> {
        let mut full_args = Vec::with_capacity(args.len() + 2);
        full_args.push("-L");
        full_args.push(self.socket.as_str());
        full_args.extend_from_slice(args);
        let sub = args.first().copied().unwrap_or("tmux");
        self.run_raw(&full_args, stdin, sub)
    }

    /// Run tmux without socket routing for command surfaces not in this slice.
    fn run_ambient(&self, args: &[&str]) -> Result<String> {
        let sub = args.first().copied().unwrap_or("tmux");
        self.run_raw(args, None, sub)
    }

    fn run_raw(&self, args: &[&str], stdin: Option<&str>, sub: &str) -> Result<String> {
        let mut child = Command::new(&self.bin)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("running {}", self.bin.display()))?;
        if let Some(input) = stdin {
            let mut pipe = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("opening stdin for {}", self.bin.display()))?;
            pipe.write_all(input.as_bytes())
                .with_context(|| format!("writing stdin for {}", self.bin.display()))?;
        }
        let output = child
            .wait_with_output()
            .with_context(|| format!("waiting for {}", self.bin.display()))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let mut detail = String::from_utf8_lossy(&output.stdout).into_owned();
        detail.push_str(&String::from_utf8_lossy(&output.stderr));
        let detail = detail.trim();
        if detail.is_empty() {
            bail!("tmux {sub} failed");
        }
        bail!("tmux {sub} failed: {detail}");
    }
}

fn resolve_socket(socket: &str, main_socket: &str) -> String {
    let socket = socket.trim();
    if !socket.is_empty() {
        return socket.to_string();
    }

    let main_socket = main_socket.trim();
    if main_socket.is_empty() {
        "default".to_string()
    } else {
        main_socket.to_string()
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

    fn load_buffer(&self, name: &str, text: &str) -> Result<()> {
        self.run_with_stdin(&["load-buffer", "-b", name, "-"], Some(text))?;
        Ok(())
    }

    fn paste_buffer(&self, name: &str, pane_id: &str) -> Result<()> {
        self.run(&["paste-buffer", "-d", "-p", "-b", name, "-t", pane_id])?;
        Ok(())
    }

    fn delete_buffer(&self, name: &str) -> Result<()> {
        self.run(&["delete-buffer", "-b", name])?;
        Ok(())
    }

    fn send_enter(&self, pane_id: &str) -> Result<()> {
        self.run(&["send-keys", "-t", pane_id, "Enter"])?;
        Ok(())
    }

    fn capture_pane(&self, pane_id: &str, scrollback: i32) -> Result<String> {
        if scrollback > 0 {
            let scrollback = format!("-{scrollback}");
            self.run(&["capture-pane", "-p", "-t", pane_id, "-S", &scrollback])
        } else {
            self.run(&["capture-pane", "-p", "-t", pane_id])
        }
    }

    fn has_session(&self, session: &str) -> Result<()> {
        self.run(&["has-session", "-t", session])?;
        Ok(())
    }

    fn attach_session_in_new_window(
        &self,
        session: &str,
        window_name: &str,
        socket: &str,
    ) -> Result<()> {
        let session = shell_quote(session);
        let requested_socket = socket.trim();
        let nested_socket = if requested_socket.is_empty() && self.socket != "default" {
            self.socket.as_str()
        } else {
            requested_socket
        };
        let command = if nested_socket.is_empty() {
            format!("env -u TMUX tmux attach-session -t {session}")
        } else {
            format!(
                "env -u TMUX tmux -L {} attach-session -t {session}",
                shell_quote(nested_socket)
            )
        };
        self.run_ambient(&["new-window", "-n", window_name, &command])?;
        Ok(())
    }
}

fn shell_quote(value: &str) -> String {
    if value.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
            )
    }) {
        return value.to_string();
    }

    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
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
        let tmux = Tmux::new(bin, "", "default");

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
                "-L",
                "default",
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
        let tmux = Tmux::new(bin, "", "default");

        let err = tmux.resolve_pane("bogus").unwrap_err().to_string();
        assert!(err.contains("unknown pane: bogus"), "got: {err}");
    }

    #[test]
    fn resolve_pane_rejects_empty_target() {
        // Empty target must fail before any subprocess runs.
        let tmux = Tmux::new(PathBuf::from("/nonexistent/tmux"), "", "default");
        let err = tmux.resolve_pane("   ").unwrap_err().to_string();
        assert!(err.contains("tmux target is required"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn load_buffer_issues_exact_argv_and_writes_payload_to_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "", "default");

        tmux.load_buffer("tfmux-agent1", "hello\nworld").unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec!["-L", "default", "load-buffer", "-b", "tfmux-agent1", "-"]
        );
        let stdin = fs::read_to_string(dir.path().join("stdin.txt")).unwrap();
        assert_eq!(stdin, "hello\nworld");
    }

    #[cfg(unix)]
    #[test]
    fn paste_buffer_issues_exact_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "", "default");

        tmux.paste_buffer("tfmux-agent1", "%5").unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec![
                "-L",
                "default",
                "paste-buffer",
                "-d",
                "-p",
                "-b",
                "tfmux-agent1",
                "-t",
                "%5",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn send_enter_issues_exact_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "", "default");

        tmux.send_enter("%5").unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec!["-L", "default", "send-keys", "-t", "%5", "Enter"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_pane_issues_exact_argv_and_returns_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "captured\n");
        let tmux = Tmux::new(bin, "", "default");

        let output = tmux.capture_pane("%5", 80).unwrap();

        assert_eq!(output, "captured\n");
        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec![
                "-L",
                "default",
                "capture-pane",
                "-p",
                "-t",
                "%5",
                "-S",
                "-80"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn has_session_issues_exact_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "", "default");

        tmux.has_session("worker").unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(args, vec!["-L", "default", "has-session", "-t", "worker"]);
    }

    #[cfg(unix)]
    #[test]
    fn has_session_surfaces_command_failure_detail() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_failing_tmux(dir.path(), "no such session: worker");
        let tmux = Tmux::new(bin, "", "default");

        let err = tmux.has_session("worker").unwrap_err().to_string();

        assert!(err.contains("tmux has-session failed"), "got: {err}");
        assert!(err.contains("no such session: worker"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn attach_session_in_new_window_empty_socket_keeps_legacy_inner_command() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "", "default");

        tmux.attach_session_in_new_window("worker", "agent-worker", "")
            .unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec![
                "new-window",
                "-n",
                "agent-worker",
                "env -u TMUX tmux attach-session -t worker",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn attach_session_in_new_window_socket_qualifies_inner_command_only() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "factory", "default");

        tmux.attach_session_in_new_window("worker", "agent-worker", "factory")
            .unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec![
                "new-window",
                "-n",
                "agent-worker",
                "env -u TMUX tmux -L factory attach-session -t worker",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn attach_session_empty_socket_uses_custom_main_socket_for_inner_command() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "", "main");

        tmux.attach_session_in_new_window("worker", "agent-worker", "")
            .unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec![
                "new-window",
                "-n",
                "agent-worker",
                "env -u TMUX tmux -L main attach-session -t worker",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn attach_session_quotes_nested_session_argument() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "", "default");

        tmux.attach_session_in_new_window("work session's $main", "agent", "")
            .unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec![
                "new-window",
                "-n",
                "agent",
                "env -u TMUX tmux attach-session -t 'work session'\\''s $main'",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn delete_buffer_issues_exact_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "", "default");

        tmux.delete_buffer("tfmux-agent1").unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec!["-L", "default", "delete-buffer", "-b", "tfmux-agent1"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_empty_socket_prefixes_that_socket() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "factory", "default");

        tmux.send_enter("%5").unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(
            args,
            vec!["-L", "factory", "send-keys", "-t", "%5", "Enter"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn empty_socket_uses_main_socket_override() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_tmux(dir.path(), "");
        let tmux = Tmux::new(bin, "", "main");

        tmux.send_enter("%5").unwrap();

        let args = recorded_args(dir.path());
        assert_eq!(args, vec!["-L", "main", "send-keys", "-t", "%5", "Enter"]);
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
    fn write_recording_tmux(dir: &Path, response: &str) -> PathBuf {
        let argv = dir.join("argv.txt");
        let stdin = dir.join("stdin.txt");
        let resp = dir.join("response.txt");
        fs::write(&resp, response).unwrap();
        let script =
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv:?}\ncat > {stdin:?}\ncat {resp:?}\n");
        write_script(dir.join("faketmux-record"), &script)
    }

    #[cfg(unix)]
    fn recorded_args(dir: &Path) -> Vec<String> {
        fs::read_to_string(dir.join("argv.txt"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
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
