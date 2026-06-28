//! Clap surface and command handlers.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::fs;
use std::time::Duration;

use crate::app::App;
use crate::mux::{Mux, PaneRef};
use crate::store::{self, rfc3339, Store};
use crate::target::{validate_kind, validate_name, validate_role, Target};

const SEND_SETTLE: Duration = Duration::from_millis(150);
const SEND_CAPTURE_SCROLLBACK: i32 = 80;

/// Parsed top-level CLI invocation.
#[derive(Parser)]
#[command(name = "tfmux", version, about = "Drive a tmux agent fleet.")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Subcommand)]
pub enum Command {
    /// Bind a named target to a canonical tmux pane.
    Bind(BindArgs),
    /// Send text to a bound tmux pane.
    Send(SendArgs),
    /// Remove a bound target from a session.
    Unbind(UnbindArgs),
    /// List bound targets in an existing session.
    Targets(TargetsArgs),
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
    pub file: Option<String>,
    /// Read the send payload from stdin when this positional is `-`.
    #[arg(value_name = "-", value_parser = ["-"])]
    pub stdin_marker: Option<String>,
    /// Override session selection.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
}

/// Arguments for `tfmux unbind`.
#[derive(Args)]
pub struct UnbindArgs {
    /// Target name (a single path-safe token).
    pub name: String,
    /// Override session selection.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
    /// Print a stable JSON summary instead of the text summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct UnbindOutput<'a> {
    session: &'a str,
    name: &'a str,
    removed: bool,
}

/// Arguments for `tfmux targets`.
#[derive(Args)]
pub struct TargetsArgs {
    /// Override session selection.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
    /// Print targets and status as JSON instead of a text table.
    #[arg(long)]
    pub json: bool,
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
        if path.is_empty() {
            bail!("--file requires a path");
        }
        fs::read_to_string(path).with_context(|| format!("reading {path}"))?
    } else {
        read_stdin()?
    };

    if payload.is_empty() {
        bail!("empty payload; pass non-empty text");
    }
    Ok(payload)
}

/// Result metadata from a verified send delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendDelivery {
    pub bytes: usize,
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetStatusKind {
    Live,
    Stale,
    Dead,
}

impl TargetStatusKind {
    fn as_str(&self) -> &'static str {
        match self {
            TargetStatusKind::Live => "live",
            TargetStatusKind::Stale => "stale",
            TargetStatusKind::Dead => "dead",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PaneSnapshot {
    pane_id: String,
    session: String,
    window: String,
    pane_index: String,
}

impl From<PaneRef> for PaneSnapshot {
    fn from(pane: PaneRef) -> Self {
        Self {
            pane_id: pane.pane_id,
            session: pane.session,
            window: pane.window,
            pane_index: pane.pane_index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetStatusRow {
    target: Target,
    status: TargetStatusKind,
    actual_pane: Option<PaneSnapshot>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TargetStatusJson {
    name: String,
    role: String,
    kind: String,
    input: String,
    pane_id: String,
    session: String,
    window: String,
    pane_index: String,
    bound_at: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_pane: Option<PaneSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl From<&TargetStatusRow> for TargetStatusJson {
    fn from(row: &TargetStatusRow) -> Self {
        Self {
            name: row.target.name.clone(),
            role: row.target.role.clone(),
            kind: row.target.kind.clone(),
            input: row.target.input.clone(),
            pane_id: row.target.pane_id.clone(),
            session: row.target.session.clone(),
            window: row.target.window.clone(),
            pane_index: row.target.pane_index.clone(),
            bound_at: row.target.bound_at.clone(),
            status: row.status.as_str(),
            actual_pane: row.actual_pane.clone(),
            error: row.error.clone(),
        }
    }
}

fn inspect_target_status(mux: &dyn Mux, target: &Target) -> TargetStatusRow {
    match mux.resolve_pane(&target.pane_id) {
        Ok(pane) => {
            let status = if pane.pane_id == target.pane_id
                && pane.session == target.session
                && pane.window == target.window
                && pane.pane_index == target.pane_index
            {
                TargetStatusKind::Live
            } else {
                TargetStatusKind::Stale
            };
            let actual_pane = match status {
                TargetStatusKind::Live => None,
                TargetStatusKind::Stale | TargetStatusKind::Dead => Some(pane.into()),
            };
            TargetStatusRow {
                target: target.clone(),
                status,
                actual_pane,
                error: None,
            }
        }
        Err(error) => TargetStatusRow {
            target: target.clone(),
            status: TargetStatusKind::Dead,
            actual_pane: None,
            error: Some(format!("{error:#}")),
        },
    }
}

/// Deliver `payload` to `target` through tmux and verify it was submitted.
///
/// The stored pane id is re-resolved before any paste. After paste + Enter,
/// the pane is captured and checked for Claude/Codex pasted-content markers;
/// one extra Enter is sent if such a marker remains visible.
///
/// # Errors
/// Returns an error when the target pane is gone, the re-resolved id differs,
/// any tmux action fails, or the pasted-content marker remains after retry.
pub fn deliver_payload(
    mux: &dyn Mux,
    target: &Target,
    payload: &str,
    buffer_name: &str,
    sleep: &dyn Fn(Duration),
) -> Result<SendDelivery> {
    let pane = mux.resolve_pane(&target.pane_id).map_err(|e| {
        anyhow!(
            "target \"{}\" pane {} no longer exists; rebind it: {e:#}",
            target.name,
            target.pane_id
        )
    })?;
    if pane.pane_id != target.pane_id {
        bail!(
            "target \"{}\" pane re-check resolved \"{}\" to \"{}\"; rebind it",
            target.name,
            target.pane_id,
            pane.pane_id
        );
    }
    if pane.session != target.session
        || pane.window != target.window
        || pane.pane_index != target.pane_index
    {
        bail!(
            "target \"{}\" pane metadata changed for {}; rebind it",
            target.name,
            target.pane_id
        );
    }

    mux.load_buffer(buffer_name, payload)?;
    if let Err(error) = mux.paste_buffer(buffer_name, &target.pane_id) {
        let _ = mux.delete_buffer(buffer_name);
        return Err(error);
    }
    mux.send_enter(&target.pane_id)?;
    sleep(SEND_SETTLE);

    let mut stripped = strip_ansi(&mux.capture_pane(&target.pane_id, SEND_CAPTURE_SCROLLBACK)?);
    if has_pasted_marker(&stripped) {
        mux.send_enter(&target.pane_id)?;
        sleep(SEND_SETTLE);
        stripped = strip_ansi(&mux.capture_pane(&target.pane_id, SEND_CAPTURE_SCROLLBACK)?);
    }

    if has_pasted_marker(&stripped) {
        bail!(
            "send to \"{}\" appears unsent; pasted-content marker is still visible:\n{}",
            target.name,
            last_lines(&stripped, 20)
        );
    }

    Ok(SendDelivery {
        bytes: payload.len(),
        pane_id: target.pane_id.clone(),
    })
}

fn has_pasted_marker(text: &str) -> bool {
    text.contains("[Pasted Content") || text.contains("[Pasted text")
}

fn last_lines(text: &str, count: usize) -> String {
    let mut lines = text.lines().rev().take(count).collect::<Vec<_>>();
    lines.reverse();
    lines.join("\n")
}

fn strip_ansi(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for escaped in chars.by_ref() {
                if ('@'..='~').contains(&escaped) {
                    break;
                }
            }
        } else {
            stripped.push(ch);
        }
    }
    stripped
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::fs;
    use std::rc::Rc;
    use std::time::Duration;

    use crate::mux::{Mux, PaneRef};

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

    fn sample_send_target() -> Target {
        Target {
            name: "agent1".to_string(),
            role: "agent".to_string(),
            kind: "claude".to_string(),
            input: "sess:1.0".to_string(),
            pane_id: "%5".to_string(),
            session: "sess".to_string(),
            window: "1".to_string(),
            pane_index: "0".to_string(),
            bound_at: "2026-06-28T12:00:00Z".to_string(),
        }
    }

    fn pane_ref(pane_id: &str) -> PaneRef {
        PaneRef {
            input: pane_id.to_string(),
            pane_id: pane_id.to_string(),
            session: "sess".to_string(),
            window: "1".to_string(),
            pane_index: "0".to_string(),
        }
    }

    fn pane_ref_at(pane_id: &str, session: &str, window: &str, pane_index: &str) -> PaneRef {
        PaneRef {
            input: pane_id.to_string(),
            pane_id: pane_id.to_string(),
            session: session.to_string(),
            window: window.to_string(),
            pane_index: pane_index.to_string(),
        }
    }

    #[derive(Default)]
    struct DeliveryCalls {
        resolved: Vec<String>,
        loaded: Vec<(String, String)>,
        pasted: Vec<(String, String)>,
        deleted: Vec<String>,
        enters: Vec<String>,
        captured: Vec<(String, i32)>,
    }

    struct DeliveryMux {
        calls: Rc<RefCell<DeliveryCalls>>,
        resolves: RefCell<VecDeque<std::result::Result<PaneRef, String>>>,
        captures: RefCell<VecDeque<String>>,
        paste_error: Option<String>,
    }

    impl DeliveryMux {
        fn new(resolves: Vec<std::result::Result<PaneRef, String>>, captures: Vec<&str>) -> Self {
            Self {
                calls: Rc::new(RefCell::new(DeliveryCalls::default())),
                resolves: RefCell::new(resolves.into_iter().collect()),
                captures: RefCell::new(captures.into_iter().map(str::to_string).collect()),
                paste_error: None,
            }
        }

        fn with_paste_error(mut self, message: &str) -> Self {
            self.paste_error = Some(message.to_string());
            self
        }
    }

    impl Mux for DeliveryMux {
        fn resolve_pane(&self, target: &str) -> Result<PaneRef> {
            self.calls.borrow_mut().resolved.push(target.to_string());
            match self.resolves.borrow_mut().pop_front() {
                Some(Ok(pane)) => Ok(pane),
                Some(Err(err)) => anyhow::bail!("{err}"),
                None => Ok(pane_ref(target)),
            }
        }

        fn load_buffer(&self, name: &str, text: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .loaded
                .push((name.to_string(), text.to_string()));
            Ok(())
        }

        fn paste_buffer(&self, name: &str, pane_id: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .pasted
                .push((name.to_string(), pane_id.to_string()));
            if let Some(message) = &self.paste_error {
                anyhow::bail!("{message}");
            }
            Ok(())
        }

        fn delete_buffer(&self, name: &str) -> Result<()> {
            self.calls.borrow_mut().deleted.push(name.to_string());
            Ok(())
        }

        fn send_enter(&self, pane_id: &str) -> Result<()> {
            self.calls.borrow_mut().enters.push(pane_id.to_string());
            Ok(())
        }

        fn capture_pane(&self, pane_id: &str, scrollback: i32) -> Result<String> {
            self.calls
                .borrow_mut()
                .captured
                .push((pane_id.to_string(), scrollback));
            Ok(self.captures.borrow_mut().pop_front().unwrap_or_default())
        }

        fn has_session(&self, _session: &str) -> Result<()> {
            anyhow::bail!("unexpected has_session call")
        }

        fn attach_session_in_new_window(&self, _session: &str, _window_name: &str) -> Result<()> {
            anyhow::bail!("unexpected attach_session_in_new_window call")
        }
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
            file: Some("message.txt".to_string()),
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
            file: Some(String::new()),
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
            file: Some(path.display().to_string()),
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

    #[test]
    fn send_cli_parser_preserves_empty_file_path() {
        let cli =
            Cli::try_parse_from(["tfmux", "send", "agent1", "--file", "", "--session", "demo"])
                .unwrap();

        match cli.command {
            Command::Send(args) => assert_eq!(args.file.as_deref(), Some("")),
            Command::Bind(_) | Command::Unbind(_) | Command::Targets(_) => {
                panic!("expected send command")
            }
        }
    }

    #[test]
    fn send_delivery_normal_success() {
        let mux = DeliveryMux::new(vec![Ok(pane_ref("%5"))], vec!["\u{1b}[32mok\u{1b}[0m"]);
        let target = sample_send_target();
        let slept = Cell::new(0);

        let sent = deliver_payload(&mux, &target, "hello", "buf1", &|duration| {
            assert_eq!(duration, Duration::from_millis(150));
            slept.set(slept.get() + 1);
        })
        .unwrap();

        assert_eq!(sent.bytes, 5);
        assert_eq!(sent.pane_id, "%5");
        let calls = mux.calls.borrow();
        assert_eq!(calls.resolved, vec!["%5"]);
        assert_eq!(
            calls.loaded,
            vec![("buf1".to_string(), "hello".to_string())]
        );
        assert_eq!(calls.pasted, vec![("buf1".to_string(), "%5".to_string())]);
        assert_eq!(calls.enters, vec!["%5"]);
        assert_eq!(calls.captured, vec![("%5".to_string(), 80)]);
        assert_eq!(slept.get(), 1);
    }

    #[test]
    fn send_delivery_dead_pane_asks_to_rebind() {
        let mux = DeliveryMux::new(vec![Err("can't find pane".to_string())], vec![]);
        let err = deliver_payload(&mux, &sample_send_target(), "hello", "buf1", &|_| {})
            .unwrap_err()
            .to_string();

        assert_eq!(
            err,
            "target \"agent1\" pane %5 no longer exists; rebind it: can't find pane"
        );
        assert!(mux.calls.borrow().loaded.is_empty());
    }

    #[test]
    fn send_delivery_pane_id_mismatch_asks_to_rebind() {
        let mux = DeliveryMux::new(vec![Ok(pane_ref("%6"))], vec![]);
        let err = deliver_payload(&mux, &sample_send_target(), "hello", "buf1", &|_| {})
            .unwrap_err()
            .to_string();

        assert_eq!(
            err,
            "target \"agent1\" pane re-check resolved \"%5\" to \"%6\"; rebind it"
        );
        assert!(mux.calls.borrow().loaded.is_empty());
    }

    #[test]
    fn send_delivery_pane_metadata_mismatch_asks_to_rebind() {
        let mux = DeliveryMux::new(vec![Ok(pane_ref_at("%5", "other", "1", "0"))], vec![]);
        let err = deliver_payload(&mux, &sample_send_target(), "hello", "buf1", &|_| {})
            .unwrap_err()
            .to_string();

        assert_eq!(
            err,
            "target \"agent1\" pane metadata changed for %5; rebind it"
        );
        assert!(mux.calls.borrow().loaded.is_empty());
    }

    #[test]
    fn send_delivery_deletes_loaded_buffer_when_paste_fails() {
        let mux =
            DeliveryMux::new(vec![Ok(pane_ref("%5"))], vec![]).with_paste_error("paste failed");
        let err = deliver_payload(&mux, &sample_send_target(), "hello", "buf1", &|_| {})
            .unwrap_err()
            .to_string();

        assert_eq!(err, "paste failed");
        let calls = mux.calls.borrow();
        assert_eq!(
            calls.loaded,
            vec![("buf1".to_string(), "hello".to_string())]
        );
        assert_eq!(calls.deleted, vec!["buf1".to_string()]);
        assert!(calls.enters.is_empty());
        assert!(calls.captured.is_empty());
    }

    #[test]
    fn send_delivery_pasted_content_marker_causes_one_extra_enter() {
        let mux = DeliveryMux::new(
            vec![Ok(pane_ref("%5"))],
            vec![
                "line\n[Pasted \u{1b}[31mContent 123]\n",
                "line\nsubmitted\n",
            ],
        );
        let slept = Cell::new(0);

        deliver_payload(&mux, &sample_send_target(), "hello", "buf1", &|_| {
            slept.set(slept.get() + 1);
        })
        .unwrap();

        let calls = mux.calls.borrow();
        assert_eq!(calls.enters, vec!["%5", "%5"]);
        assert_eq!(calls.captured.len(), 2);
        assert_eq!(slept.get(), 2);
    }

    #[test]
    fn send_delivery_marker_still_visible_returns_appears_unsent_error() {
        let lines = (1..=25)
            .map(|n| {
                if n == 25 {
                    "line25 [Pasted text 123]".to_string()
                } else {
                    format!("line{n}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mux = DeliveryMux::new(vec![Ok(pane_ref("%5"))], vec![&lines, &lines]);

        let err = deliver_payload(&mux, &sample_send_target(), "hello", "buf1", &|_| {})
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("send to \"agent1\" appears unsent"),
            "got: {err}"
        );
        assert!(err.contains("line6"), "got: {err}");
        assert!(err.contains("line25 [Pasted text 123]"), "got: {err}");
        assert!(!err.contains("line5\n"), "got: {err}");
        assert_eq!(mux.calls.borrow().enters, vec!["%5", "%5"]);
    }

    #[test]
    fn target_status_live_when_resolved_metadata_matches() {
        let mux = DeliveryMux::new(vec![Ok(pane_ref("%5"))], vec![]);
        let row = inspect_target_status(&mux, &sample_send_target());

        assert_eq!(row.status, TargetStatusKind::Live);
        assert_eq!(row.actual_pane, None);
        assert_eq!(row.error, None);
        assert_eq!(mux.calls.borrow().resolved, vec!["%5"]);
    }

    #[test]
    fn target_status_dead_when_resolve_fails() {
        let mux = DeliveryMux::new(vec![Err("can't find pane %5".to_string())], vec![]);
        let row = inspect_target_status(&mux, &sample_send_target());

        assert_eq!(row.status, TargetStatusKind::Dead);
        assert_eq!(row.actual_pane, None);
        assert_eq!(row.error.as_deref(), Some("can't find pane %5"));
    }

    #[test]
    fn target_status_stale_when_resolved_metadata_differs() {
        let mux = DeliveryMux::new(vec![Ok(pane_ref_at("%5", "other", "1", "0"))], vec![]);
        let row = inspect_target_status(&mux, &sample_send_target());

        assert_eq!(row.status, TargetStatusKind::Stale);
        assert_eq!(
            row.actual_pane,
            Some(PaneSnapshot {
                pane_id: "%5".to_string(),
                session: "other".to_string(),
                window: "1".to_string(),
                pane_index: "0".to_string(),
            })
        );
        assert_eq!(row.error, None);
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

    let env_session = (app.env)("TFMUX_SESSION");
    // Resolve the session name (read-only; precedence + path-safe validation).
    let marker = if has_flag_or_env_session(args.session.as_deref(), env_session.as_deref()) {
        None
    } else {
        store::read_session_marker(&app.cwd)?
    };
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

/// Handle `tfmux send`: resolve a payload and existing target, deliver it, and
/// print the verified byte count.
///
/// # Errors
/// Returns an error on invalid names/input, missing sessions or targets, tmux
/// failures, or verification failures.
pub fn send(app: &mut App, args: &SendArgs) -> Result<()> {
    validate_name(&args.name)?;
    let payload = resolve_send_payload(args, app.read_stdin)?;

    let env_session = (app.env)("TFMUX_SESSION");
    let marker = if has_flag_or_env_session(args.session.as_deref(), env_session.as_deref()) {
        None
    } else {
        store::read_session_marker(&app.cwd)?
    };
    let session_name = resolve_send_session_name(
        args.session.as_deref(),
        env_session.as_deref(),
        marker.as_deref(),
    )?;

    let store = Store::new(app.base_dir.clone());
    let session_dir = store
        .find_session_dir(&session_name)?
        .ok_or_else(|| missing_send_session(&session_name))?;
    let target = store
        .load_target(&session_dir, &args.name)?
        .ok_or_else(|| {
            anyhow!(
                "no target \"{}\" in session {}; run `tfmux bind {} ...`",
                args.name,
                session_name,
                args.name
            )
        })?;

    let mux = (app.new_mux)()?;
    let buffer_name = (app.new_buffer_name)();
    let sent = deliver_payload(mux.as_ref(), &target, &payload, &buffer_name, app.sleep)?;
    writeln!(
        app.out,
        "sent {} bytes to \"{}\" ({})",
        sent.bytes, target.name, sent.pane_id
    )?;
    Ok(())
}

/// Handle `tfmux unbind`: remove one target file from an existing session.
///
/// # Errors
/// Returns an error on invalid names, missing session selection, missing
/// sessions, missing targets, or filesystem deletion failures.
pub fn unbind(app: &mut App, args: &UnbindArgs) -> Result<()> {
    validate_name(&args.name)?;

    let env_session = (app.env)("TFMUX_SESSION");
    let marker = if has_flag_or_env_session(args.session.as_deref(), env_session.as_deref()) {
        None
    } else {
        store::read_session_marker(&app.cwd)?
    };
    let session_name = resolve_send_session_name(
        args.session.as_deref(),
        env_session.as_deref(),
        marker.as_deref(),
    )?;

    let store = Store::new(app.base_dir.clone());
    let session_dir = store
        .find_session_dir(&session_name)?
        .ok_or_else(|| anyhow!("no tfmux session \"{session_name}\""))?;
    if !store.delete_target(&session_dir, &args.name)? {
        bail!("no target \"{}\" in session {}", args.name, session_name);
    }

    if args.json {
        let output = UnbindOutput {
            session: &session_name,
            name: &args.name,
            removed: true,
        };
        writeln!(app.out, "{}", serde_json::to_string_pretty(&output)?)?;
    } else {
        writeln!(
            app.out,
            "unbound \"{}\" from session {}",
            args.name, session_name
        )?;
    }
    Ok(())
}

/// Handle `tfmux targets`: list bound targets in an existing session and show
/// whether each stored pane still resolves to the same tmux metadata.
///
/// # Errors
/// Returns an error when no session can be selected, the selected session does
/// not exist, target files cannot be read, or tmux construction fails.
pub fn targets(app: &mut App, args: &TargetsArgs) -> Result<()> {
    let env_session = (app.env)("TFMUX_SESSION");
    let marker = if has_flag_or_env_session(args.session.as_deref(), env_session.as_deref()) {
        None
    } else {
        store::read_session_marker(&app.cwd)?
    };
    let session_name = resolve_send_session_name(
        args.session.as_deref(),
        env_session.as_deref(),
        marker.as_deref(),
    )?;

    let store = Store::new(app.base_dir.clone());
    let session_dir = store
        .find_session_dir(&session_name)?
        .ok_or_else(|| anyhow!("no tfmux session \"{session_name}\""))?;
    let stored_targets = store.list_targets(&session_dir)?;
    if stored_targets.is_empty() {
        if args.json {
            writeln!(app.out, "[]")?;
        }
        return Ok(());
    }

    let mux = (app.new_mux)()?;
    let rows = stored_targets
        .iter()
        .map(|target| inspect_target_status(mux.as_ref(), target))
        .collect::<Vec<_>>();
    if args.json {
        let json_rows = rows.iter().map(TargetStatusJson::from).collect::<Vec<_>>();
        writeln!(app.out, "{}", serde_json::to_string_pretty(&json_rows)?)?;
    } else {
        write_targets_table(app, &rows)?;
    }
    Ok(())
}

fn write_targets_table(app: &mut App, rows: &[TargetStatusRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(
        app.out,
        "{:<10} {:<8} {:<8} {:<6} {:<14} STATUS",
        "NAME", "ROLE", "KIND", "PANE", "LOCATION"
    )?;
    for row in rows {
        let location = format!(
            "{}:{}.{}",
            row.target.session, row.target.window, row.target.pane_index
        );
        writeln!(
            app.out,
            "{:<10} {:<8} {:<8} {:<6} {:<14} {}",
            row.target.name,
            row.target.role,
            row.target.kind,
            row.target.pane_id,
            location,
            row.status.as_str()
        )?;
    }
    Ok(())
}

fn resolve_send_session_name(
    flag: Option<&str>,
    env: Option<&str>,
    marker: Option<&str>,
) -> Result<String> {
    for candidate in [flag, env, marker] {
        if let Some(name) = candidate.map(str::trim).filter(|s| !s.is_empty()) {
            validate_name(name)?;
            return Ok(name.to_string());
        }
    }
    Err(no_send_session_selected())
}

fn has_flag_or_env_session(flag: Option<&str>, env: Option<&str>) -> bool {
    [flag, env]
        .into_iter()
        .flatten()
        .any(|candidate| !candidate.trim().is_empty())
}

fn no_send_session_selected() -> anyhow::Error {
    anyhow!("no tfmux session selected; pass --session NAME, set TFMUX_SESSION, or add .llm/tfmux-session")
}

fn missing_send_session(session_name: &str) -> anyhow::Error {
    anyhow!(
        "tfmux session \"{session_name}\" not found; run `tfmux bind <name> ... --session {session_name}` first"
    )
}
