//! Clap surface and command handlers.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use crate::app::App;
use crate::mux::Mux;
use crate::store::{self, rfc3339, Store};
use crate::target::{validate_kind, validate_name, validate_role, Target};

const SEND_SETTLE: Duration = Duration::from_millis(150);
const SEND_CAPTURE_SCROLLBACK: i32 = 80;

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
    /// Send text to a bound tmux pane.
    Send(SendArgs),
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

/// Result metadata from a verified send delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendDelivery {
    pub bytes: usize,
    pub pane_id: String,
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

    mux.load_buffer(buffer_name, payload)?;
    mux.paste_buffer(buffer_name, &target.pane_id)?;
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
mod tests {
    use super::*;
    use anyhow::Result;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
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

    #[derive(Default)]
    struct DeliveryCalls {
        resolved: Vec<String>,
        loaded: Vec<(String, String)>,
        pasted: Vec<(String, String)>,
        enters: Vec<String>,
        captured: Vec<(String, i32)>,
    }

    struct DeliveryMux {
        calls: Rc<RefCell<DeliveryCalls>>,
        resolves: RefCell<VecDeque<std::result::Result<PaneRef, String>>>,
        captures: RefCell<VecDeque<String>>,
    }

    impl DeliveryMux {
        fn new(resolves: Vec<std::result::Result<PaneRef, String>>, captures: Vec<&str>) -> Self {
            Self {
                calls: Rc::new(RefCell::new(DeliveryCalls::default())),
                resolves: RefCell::new(resolves.into_iter().collect()),
                captures: RefCell::new(captures.into_iter().map(str::to_string).collect()),
            }
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

/// Handle `tfmux send`: resolve a payload and existing target, deliver it, and
/// print the verified byte count.
///
/// # Errors
/// Returns an error on invalid names/input, missing sessions or targets, tmux
/// failures, or verification failures.
pub fn send(app: &mut App, args: &SendArgs) -> Result<()> {
    validate_name(&args.name)?;
    let payload = resolve_send_payload(args, app.read_stdin)?;

    let marker = store::read_session_marker(&app.cwd)?;
    let env_session = (app.env)("TFMUX_SESSION");
    let session_name = resolve_send_session_name(
        args.session.as_deref(),
        env_session.as_deref(),
        marker.as_deref(),
    )?;

    let store = Store::new(app.base_dir.clone());
    let session_dir = store
        .find_session_dir(&session_name)?
        .ok_or_else(no_send_session_selected)?;
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

fn no_send_session_selected() -> anyhow::Error {
    anyhow!("no tfmux session selected; pass --session NAME, set TFMUX_SESSION, or add .llm/tfmux-session")
}
