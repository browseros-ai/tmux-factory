//! End-to-end `tfmux bind` tests driven through `tfmux::run` with a fake tmux
//! backend, a temp `TFMUX_HOME`, an injected env/cwd, and a fixed clock.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use clap::Parser;
use serde_json::json;
use tempfile::TempDir;

use tfmux::app::App;
use tfmux::cli::Cli;
use tfmux::mux::{Mux, PaneRef};
use tfmux::store::Store;
use tfmux::target::Target;

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap()
}

#[derive(Default)]
struct MuxCalls {
    built: u32,
    resolved: Vec<String>,
    loaded: Vec<(String, String)>,
    pasted: Vec<(String, String)>,
    deleted: Vec<String>,
    enters: Vec<String>,
    captured: Vec<(String, i32)>,
    has_sessions: Vec<String>,
    attached_windows: Vec<(String, String)>,
}

/// Fake tmux: records each resolve call and returns a scripted pane (or error).
struct FakeMux {
    calls: Rc<RefCell<MuxCalls>>,
    pane: PaneRef,
    err: Option<String>,
    has_session_err: Option<String>,
    resolve_overrides: HashMap<String, std::result::Result<PaneRef, String>>,
    captures: Rc<RefCell<VecDeque<String>>>,
}

impl Mux for FakeMux {
    fn resolve_pane(&self, target: &str) -> anyhow::Result<PaneRef> {
        self.calls.borrow_mut().resolved.push(target.to_string());
        if let Some(result) = self.resolve_overrides.get(target) {
            return match result {
                Ok(pane) => Ok(PaneRef {
                    input: target.to_string(),
                    ..pane.clone()
                }),
                Err(err) => anyhow::bail!("{err}"),
            };
        }
        if let Some(e) = &self.err {
            anyhow::bail!("{e}");
        }
        Ok(PaneRef {
            input: target.to_string(),
            ..self.pane.clone()
        })
    }

    fn load_buffer(&self, name: &str, text: &str) -> anyhow::Result<()> {
        self.calls
            .borrow_mut()
            .loaded
            .push((name.to_string(), text.to_string()));
        Ok(())
    }

    fn paste_buffer(&self, name: &str, pane_id: &str) -> anyhow::Result<()> {
        self.calls
            .borrow_mut()
            .pasted
            .push((name.to_string(), pane_id.to_string()));
        Ok(())
    }

    fn delete_buffer(&self, name: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().deleted.push(name.to_string());
        Ok(())
    }

    fn send_enter(&self, pane_id: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().enters.push(pane_id.to_string());
        Ok(())
    }

    fn capture_pane(&self, pane_id: &str, scrollback: i32) -> anyhow::Result<String> {
        self.calls
            .borrow_mut()
            .captured
            .push((pane_id.to_string(), scrollback));
        Ok(self.captures.borrow_mut().pop_front().unwrap_or_default())
    }

    fn has_session(&self, session: &str) -> anyhow::Result<()> {
        self.calls
            .borrow_mut()
            .has_sessions
            .push(session.to_string());
        if let Some(err) = &self.has_session_err {
            anyhow::bail!("{err}");
        }
        Ok(())
    }

    fn attach_session_in_new_window(&self, session: &str, window_name: &str) -> anyhow::Result<()> {
        self.calls
            .borrow_mut()
            .attached_windows
            .push((session.to_string(), window_name.to_string()));
        Ok(())
    }
}

/// Builder + driver for a single bind invocation.
struct Scenario {
    home: TempDir,
    cwd: TempDir,
    env: HashMap<String, String>,
    pane: PaneRef,
    resolve_err: Option<String>,
    has_session_err: Option<String>,
    resolve_overrides: HashMap<String, std::result::Result<PaneRef, String>>,
    stdin: String,
    buffer_name: String,
    captures: Rc<RefCell<VecDeque<String>>>,
    sleeps: Rc<Cell<u32>>,
    calls: Rc<RefCell<MuxCalls>>,
}

impl Scenario {
    fn new() -> Self {
        Scenario {
            home: tempfile::tempdir().unwrap(),
            cwd: tempfile::tempdir().unwrap(),
            env: HashMap::new(),
            pane: PaneRef {
                input: String::new(),
                pane_id: "%5".into(),
                session: "sess".into(),
                window: "1".into(),
                pane_index: "0".into(),
            },
            resolve_err: None,
            has_session_err: None,
            resolve_overrides: HashMap::new(),
            stdin: String::new(),
            buffer_name: "buf-test".to_string(),
            captures: Rc::new(RefCell::new(VecDeque::from(["submitted\n".to_string()]))),
            sleeps: Rc::new(Cell::new(0)),
            calls: Rc::new(RefCell::new(MuxCalls::default())),
        }
    }

    fn env(mut self, k: &str, v: &str) -> Self {
        self.env.insert(k.to_string(), v.to_string());
        self
    }

    fn pane(mut self, pane_id: &str, session: &str, window: &str, pane_index: &str) -> Self {
        self.pane = PaneRef {
            input: String::new(),
            pane_id: pane_id.into(),
            session: session.into(),
            window: window.into(),
            pane_index: pane_index.into(),
        };
        self
    }

    fn resolve_err(mut self, msg: &str) -> Self {
        self.resolve_err = Some(msg.to_string());
        self
    }

    fn has_session_err(mut self, msg: &str) -> Self {
        self.has_session_err = Some(msg.to_string());
        self
    }

    fn resolve_override(
        mut self,
        target: &str,
        result: std::result::Result<PaneRef, String>,
    ) -> Self {
        self.resolve_overrides.insert(target.to_string(), result);
        self
    }

    fn stdin(mut self, value: &str) -> Self {
        self.stdin = value.to_string();
        self
    }

    fn marker(self, name: &str) -> Self {
        std::fs::create_dir_all(self.cwd.path().join(".llm")).unwrap();
        std::fs::write(self.cwd.path().join(".llm/tfmux-session"), name).unwrap();
        self
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn built(&self) -> u32 {
        self.calls.borrow().built
    }

    fn resolved(&self) -> Vec<String> {
        self.calls.borrow().resolved.clone()
    }

    fn loaded(&self) -> Vec<(String, String)> {
        self.calls.borrow().loaded.clone()
    }

    fn pasted(&self) -> Vec<(String, String)> {
        self.calls.borrow().pasted.clone()
    }

    fn enters(&self) -> Vec<String> {
        self.calls.borrow().enters.clone()
    }

    fn captured(&self) -> Vec<(String, i32)> {
        self.calls.borrow().captured.clone()
    }

    fn has_sessions(&self) -> Vec<String> {
        self.calls.borrow().has_sessions.clone()
    }

    fn attached_windows(&self) -> Vec<(String, String)> {
        self.calls.borrow().attached_windows.clone()
    }

    fn sleeps(&self) -> u32 {
        self.sleeps.get()
    }

    /// Number of top-level entries written under TFMUX_HOME.
    fn home_entry_count(&self) -> usize {
        std::fs::read_dir(self.home()).unwrap().count()
    }

    fn session_dir(&self, session: &str) -> Option<PathBuf> {
        Store::new(self.home().to_path_buf())
            .find_session_dir(session)
            .unwrap()
    }

    fn read_target(&self, session: &str, name: &str) -> Target {
        let dir = self.session_dir(session).expect("session dir should exist");
        let raw =
            std::fs::read_to_string(dir.join("targets").join(format!("{name}.json"))).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn target_path(&self, session: &str, name: &str) -> PathBuf {
        self.session_dir(session)
            .expect("session dir should exist")
            .join("targets")
            .join(format!("{name}.json"))
    }

    fn save_target(&self, session: &str, name: &str) {
        self.save_target_record(
            session,
            Target {
                name: name.to_string(),
                role: "agent".to_string(),
                kind: "generic".to_string(),
                input: "sess:1.0".to_string(),
                pane_id: "%5".to_string(),
                session: "sess".to_string(),
                window: "1".to_string(),
                pane_index: "0".to_string(),
                socket: String::new(),
                bound_at: "2026-06-28T12:00:00Z".to_string(),
            },
        );
    }

    fn save_target_record(&self, session: &str, target: Target) {
        let store = Store::new(self.home().to_path_buf());
        let dir = store.create_session(session, fixed_now()).unwrap();
        store.save_target(&dir, &target).unwrap();
    }

    fn run(&self, argv: &[&str]) -> (anyhow::Result<()>, String) {
        let cli = match Cli::try_parse_from(argv.iter().copied()) {
            Ok(cli) => cli,
            Err(e) => return (Err(anyhow::anyhow!("{e}")), String::new()),
        };

        let env = self.env.clone();
        let env_fn = move |k: &str| env.get(k).cloned();
        let now_fn = fixed_now;

        let calls = self.calls.clone();
        let pane = self.pane.clone();
        let err = self.resolve_err.clone();
        let has_session_err = self.has_session_err.clone();
        let resolve_overrides = self.resolve_overrides.clone();
        let captures = self.captures.clone();
        let new_mux = move || -> anyhow::Result<Box<dyn Mux>> {
            calls.borrow_mut().built += 1;
            Ok(Box::new(FakeMux {
                calls: calls.clone(),
                pane: pane.clone(),
                err: err.clone(),
                has_session_err: has_session_err.clone(),
                resolve_overrides: resolve_overrides.clone(),
                captures: captures.clone(),
            }))
        };
        let stdin = self.stdin.clone();
        let read_stdin = move || -> anyhow::Result<String> { Ok(stdin.clone()) };
        let buffer_name = self.buffer_name.clone();
        let new_buffer_name = move || -> String { buffer_name.clone() };
        let sleeps = self.sleeps.clone();
        let sleep = move |duration: Duration| {
            assert_eq!(duration, Duration::from_millis(150));
            sleeps.set(sleeps.get() + 1);
        };

        let mut out: Vec<u8> = Vec::new();
        let mut app = App {
            base_dir: self.home().to_path_buf(),
            env: &env_fn,
            cwd: self.cwd.path().to_path_buf(),
            now: &now_fn,
            new_mux: &new_mux,
            read_stdin: &read_stdin,
            new_buffer_name: &new_buffer_name,
            sleep: &sleep,
            out: &mut out,
        };
        let result = tfmux::run(&mut app, cli);
        (result, String::from_utf8(out).unwrap())
    }
}

fn stored_target(
    name: &str,
    role: &str,
    kind: &str,
    pane_id: &str,
    session: &str,
    window: &str,
    pane_index: &str,
) -> Target {
    Target {
        name: name.to_string(),
        role: role.to_string(),
        kind: kind.to_string(),
        input: format!("{session}:{window}.{pane_index}"),
        pane_id: pane_id.to_string(),
        session: session.to_string(),
        window: window.to_string(),
        pane_index: pane_index.to_string(),
        socket: String::new(),
        bound_at: "2026-06-28T12:00:00Z".to_string(),
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

#[test]
fn attach_inside_tmux_defaults_window_name_to_session_without_state() {
    let s = Scenario::new()
        .env("TMUX", "/tmp/tmux-501/default,1234,0")
        .env("TFMUX_SESSION", "ignored");
    std::fs::create_dir_all(s.cwd.path().join(".llm/tfmux-session")).unwrap();

    let (result, stdout) = s.run(&["tfmux", "attach", "worker"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(stdout, "attached \"worker\" in new window \"worker\"\n");
    assert_eq!(s.built(), 1);
    assert_eq!(s.has_sessions(), vec!["worker".to_string()]);
    assert_eq!(
        s.attached_windows(),
        vec![("worker".to_string(), "worker".to_string())]
    );
    assert!(s.resolved().is_empty());
    assert!(s.loaded().is_empty());
    assert!(!s.home().join("current").exists());
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn attach_custom_window_name() {
    let s = Scenario::new().env("TMUX", "/tmp/tmux-501/default,1234,0");

    let (result, stdout) = s.run(&["tfmux", "attach", "worker", "--window-name", "agent-worker"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(
        stdout,
        "attached \"worker\" in new window \"agent-worker\"\n"
    );
    assert_eq!(s.has_sessions(), vec!["worker".to_string()]);
    assert_eq!(
        s.attached_windows(),
        vec![("worker".to_string(), "agent-worker".to_string())]
    );
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn attach_preserves_raw_session_and_window_names_after_blank_validation() {
    let s = Scenario::new().env("TMUX", "/tmp/tmux-501/default,1234,0");

    let (result, stdout) = s.run(&[
        "tfmux",
        "attach",
        " worker ",
        "--window-name",
        " agent worker ",
    ]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(
        stdout,
        "attached \" worker \" in new window \" agent worker \"\n"
    );
    assert_eq!(s.has_sessions(), vec![" worker ".to_string()]);
    assert_eq!(
        s.attached_windows(),
        vec![(" worker ".to_string(), " agent worker ".to_string())]
    );
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn attach_outside_tmux_errors_before_building_mux_or_writing_state() {
    let s = Scenario::new();

    let (result, _) = s.run(&["tfmux", "attach", "worker"]);

    let err = result.unwrap_err().to_string();
    assert_eq!(err, "attach requires TMUX; run from inside tmux");
    assert_eq!(s.built(), 0);
    assert!(s.has_sessions().is_empty());
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn attach_real_binary_outside_tmux_does_not_require_home() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tfmux"))
        .args(["attach", "worker"])
        .env_remove("HOME")
        .env_remove("TFMUX_HOME")
        .env_remove("TMUX")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: attach requires TMUX; run from inside tmux\n"
    );
}

#[test]
fn attach_blank_session_errors_before_building_mux_or_writing_state() {
    let s = Scenario::new().env("TMUX", "/tmp/tmux-501/default,1234,0");

    let (result, _) = s.run(&["tfmux", "attach", "", "--window-name", "worker"]);

    let err = result.unwrap_err().to_string();
    assert_eq!(err, "tmux session is required");
    assert_eq!(s.built(), 0);
    assert!(s.has_sessions().is_empty());
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn attach_blank_window_name_errors_before_building_mux_or_writing_state() {
    let s = Scenario::new().env("TMUX", "/tmp/tmux-501/default,1234,0");

    let (result, _) = s.run(&["tfmux", "attach", "worker", "--window-name", "   "]);

    let err = result.unwrap_err().to_string();
    assert_eq!(err, "--window-name requires a non-empty value");
    assert_eq!(s.built(), 0);
    assert!(s.has_sessions().is_empty());
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn attach_missing_tmux_session_propagates_has_session_failure() {
    let s = Scenario::new()
        .env("TMUX", "/tmp/tmux-501/default,1234,0")
        .has_session_err("tmux has-session failed: no such session: worker");

    let (result, _) = s.run(&["tfmux", "attach", "worker"]);

    let err = result.unwrap_err().to_string();
    assert_eq!(err, "tmux has-session failed: no such session: worker");
    assert_eq!(s.built(), 1);
    assert_eq!(s.has_sessions(), vec!["worker".to_string()]);
    assert!(s.attached_windows().is_empty());
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn bind_here_uses_tmux_pane_and_stores_canonical_pane() {
    let s = Scenario::new()
        .env("TMUX", "/tmp/tmux-501/default,1234,0")
        .env("TMUX_PANE", "%3")
        .pane("%3", "work", "2", "1");

    let (result, stdout) = s.run(&[
        "tfmux",
        "bind",
        "mediator",
        "--here",
        "--role",
        "mediator",
        "--session",
        "demo",
    ]);

    assert!(result.is_ok(), "{:?}", result.err());
    // Resolved via TMUX_PANE, never the TMUX socket value.
    assert_eq!(s.resolved(), vec!["%3".to_string()]);
    assert_eq!(stdout, "bound mediator -> %3 (work:2.1)\n");

    let t = s.read_target("demo", "mediator");
    assert_eq!(t.name, "mediator");
    assert_eq!(t.role, "mediator");
    assert_eq!(t.kind, "generic");
    assert_eq!(t.input, "%3");
    assert_eq!(t.pane_id, "%3");
    assert_eq!(t.session, "work");
    assert_eq!(t.window, "2");
    assert_eq!(t.pane_index, "1");
    assert_eq!(t.socket, "");
    assert_eq!(t.bound_at, "2026-06-28T12:00:00Z");
}

#[test]
fn bind_tmux_stores_canonical_pane_info() {
    let s = Scenario::new().pane("%5", "sess", "1", "0");

    let (result, stdout) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "sess:1.0",
        "--kind",
        "claude",
        "--session",
        "demo",
    ]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(s.resolved(), vec!["sess:1.0".to_string()]);
    assert_eq!(stdout, "bound agent1 -> %5 (sess:1.0)\n");

    let t = s.read_target("demo", "agent1");
    assert_eq!(t.role, "agent"); // default
    assert_eq!(t.kind, "claude");
    assert_eq!(t.input, "sess:1.0");
    assert_eq!(t.pane_id, "%5");
    assert_eq!(t.session, "sess");
}

#[test]
fn bind_json_prints_stored_target() {
    let s = Scenario::new().pane("%5", "sess", "1", "0");

    let (result, stdout) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "sess:1.0",
        "--session",
        "demo",
        "--json",
    ]);

    assert!(result.is_ok(), "{:?}", result.err());
    let printed: Target = serde_json::from_str(&stdout).unwrap();
    assert_eq!(printed, s.read_target("demo", "agent1"));
}

#[test]
fn bind_does_not_create_global_current_pointer() {
    let s = Scenario::new();
    let (result, _) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "sess:1.0",
        "--session",
        "demo",
    ]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert!(!s.home().join("current").exists());
    // Only the single dated dir should exist at the top level.
    assert_eq!(s.home_entry_count(), 1);
}

#[test]
fn bind_resolves_session_from_env_var() {
    let s = Scenario::new().env("TFMUX_SESSION", "envdemo");
    let (result, _) = s.run(&["tfmux", "bind", "agent1", "--tmux", "sess:1.0"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert!(s.session_dir("envdemo").is_some());
}

#[test]
fn bind_resolves_session_from_local_marker() {
    let s = Scenario::new().marker("markerdemo");
    let (result, _) = s.run(&["tfmux", "bind", "agent1", "--tmux", "sess:1.0"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert!(s.session_dir("markerdemo").is_some());
}

#[test]
fn bind_session_flag_wins_over_unreadable_marker() {
    let s = Scenario::new();
    std::fs::create_dir_all(s.cwd.path().join(".llm/tfmux-session")).unwrap();

    let (result, stdout) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "sess:1.0",
        "--session",
        "demo",
    ]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(stdout, "bound agent1 -> %5 (sess:1.0)\n");
    assert!(s.session_dir("demo").is_some());
}

#[test]
fn bind_without_any_session_name_errors() {
    let s = Scenario::new();
    let (result, _) = s.run(&["tfmux", "bind", "agent1", "--tmux", "sess:1.0"]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("no session name"), "got: {err}");
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn bind_with_neither_target_source_errors_without_writing() {
    let s = Scenario::new();
    let (result, _) = s.run(&["tfmux", "bind", "agent1", "--session", "demo"]);

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("choose exactly one target source"),
        "got: {err}"
    );
    assert_eq!(s.built(), 0, "mux must not be constructed");
    assert_eq!(s.home_entry_count(), 0, "no state should be written");
}

#[test]
fn bind_with_both_target_sources_errors_without_writing() {
    let s = Scenario::new().env("TMUX", "x").env("TMUX_PANE", "%3");
    let (result, _) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--here",
        "--tmux",
        "sess:1.0",
        "--session",
        "demo",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("choose exactly one target source"),
        "got: {err}"
    );
    assert_eq!(s.built(), 0);
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn bind_here_outside_tmux_errors_without_building_mux() {
    let s = Scenario::new(); // no TMUX in env
    let (result, _) = s.run(&["tfmux", "bind", "agent1", "--here", "--session", "demo"]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("--here requires TMUX"), "got: {err}");
    assert_eq!(
        s.built(),
        0,
        "mux must not be constructed on the --here guard"
    );
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn bind_here_with_empty_tmux_pane_errors() {
    let s = Scenario::new().env("TMUX", "/tmp/tmux-501/default,1,0"); // TMUX set, TMUX_PANE absent
    let (result, _) = s.run(&["tfmux", "bind", "agent1", "--here", "--session", "demo"]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("TMUX_PANE is empty"), "got: {err}");
    assert_eq!(s.built(), 0);
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn bind_invalid_role_errors_without_writing() {
    let s = Scenario::new();
    let (result, _) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "sess:1.0",
        "--role",
        "boss",
        "--session",
        "demo",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("invalid role"), "got: {err}");
    assert_eq!(s.built(), 0);
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn bind_resolve_failure_writes_no_state() {
    let s = Scenario::new().resolve_err("can't find pane bogus");
    let (result, _) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "bogus",
        "--session",
        "demo",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("can't find pane bogus"), "got: {err}");
    assert_eq!(s.built(), 1, "mux is built before resolve");
    assert_eq!(
        s.home_entry_count(),
        0,
        "a failed resolve must not create a session"
    );
}

#[test]
fn send_text_succeeds_against_bound_target() {
    let s = Scenario::new();
    let (bind_result, _) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "sess:1.0",
        "--session",
        "demo",
    ]);
    assert!(bind_result.is_ok(), "{:?}", bind_result.err());

    let (send_result, stdout) = s.run(&[
        "tfmux",
        "send",
        "agent1",
        "--text",
        "hello",
        "--session",
        "demo",
    ]);

    assert!(send_result.is_ok(), "{:?}", send_result.err());
    assert_eq!(stdout, "sent 5 bytes to \"agent1\" (%5)\n");
    assert_eq!(
        s.loaded(),
        vec![("buf-test".to_string(), "hello".to_string())]
    );
    assert_eq!(s.pasted(), vec![("buf-test".to_string(), "%5".to_string())]);
    assert_eq!(s.enters(), vec!["%5".to_string()]);
    assert_eq!(s.captured(), vec![("%5".to_string(), 80)]);
    assert_eq!(s.sleeps(), 1);
    assert_eq!(s.resolved(), vec!["sess:1.0".to_string(), "%5".to_string()]);
}

#[test]
fn send_file_succeeds_against_bound_target() {
    let s = Scenario::new();
    let file = s.cwd.path().join("message.txt");
    std::fs::write(&file, "from file").unwrap();
    let file_arg = file.to_str().unwrap();
    let (bind_result, _) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "sess:1.0",
        "--session",
        "demo",
    ]);
    assert!(bind_result.is_ok(), "{:?}", bind_result.err());

    let (send_result, stdout) = s.run(&[
        "tfmux",
        "send",
        "agent1",
        "--file",
        file_arg,
        "--session",
        "demo",
    ]);

    assert!(send_result.is_ok(), "{:?}", send_result.err());
    assert_eq!(stdout, "sent 9 bytes to \"agent1\" (%5)\n");
    assert_eq!(
        s.loaded(),
        vec![("buf-test".to_string(), "from file".to_string())]
    );
}

#[test]
fn send_stdin_succeeds_against_bound_target() {
    let s = Scenario::new().stdin("from stdin");
    let (bind_result, _) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "sess:1.0",
        "--session",
        "demo",
    ]);
    assert!(bind_result.is_ok(), "{:?}", bind_result.err());

    let (send_result, stdout) = s.run(&["tfmux", "send", "agent1", "--session", "demo", "-"]);

    assert!(send_result.is_ok(), "{:?}", send_result.err());
    assert_eq!(stdout, "sent 10 bytes to \"agent1\" (%5)\n");
    assert_eq!(
        s.loaded(),
        vec![("buf-test".to_string(), "from stdin".to_string())]
    );
}

#[test]
fn send_missing_session_errors_without_creating_state() {
    let s = Scenario::new();
    let (result, _) = s.run(&[
        "tfmux",
        "send",
        "agent1",
        "--text",
        "hello",
        "--session",
        "demo",
    ]);

    let err = result.unwrap_err().to_string();
    assert_eq!(
        err,
        "tfmux session \"demo\" not found; run `tfmux bind <name> ... --session demo` first"
    );
    assert_eq!(s.built(), 0);
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn send_missing_target_gives_bind_hint() {
    let s = Scenario::new();
    Store::new(s.home().to_path_buf())
        .create_session("demo", fixed_now())
        .unwrap();

    let (result, _) = s.run(&[
        "tfmux",
        "send",
        "agent1",
        "--text",
        "hello",
        "--session",
        "demo",
    ]);

    let err = result.unwrap_err().to_string();
    assert_eq!(
        err,
        "no target \"agent1\" in session demo; run `tfmux bind agent1 ...`"
    );
    assert_eq!(s.built(), 0);
}

#[test]
fn send_session_flag_wins_over_unreadable_marker() {
    let s = Scenario::new();
    s.save_target("demo", "agent1");
    std::fs::create_dir_all(s.cwd.path().join(".llm/tfmux-session")).unwrap();

    let (result, stdout) = s.run(&[
        "tfmux",
        "send",
        "agent1",
        "--text",
        "hello",
        "--session",
        "demo",
    ]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(stdout, "sent 5 bytes to \"agent1\" (%5)\n");
}

#[test]
fn send_empty_file_path_uses_custom_error() {
    let s = Scenario::new();
    s.save_target("demo", "agent1");

    let (result, _) = s.run(&["tfmux", "send", "agent1", "--file", "", "--session", "demo"]);

    let err = result.unwrap_err().to_string();
    assert_eq!(err, "--file requires a path");
    assert_eq!(s.built(), 0);
}

#[test]
fn send_resolves_session_from_env_var() {
    let s = Scenario::new().env("TFMUX_SESSION", "envdemo");
    let (bind_result, _) = s.run(&["tfmux", "bind", "agent1", "--tmux", "sess:1.0"]);
    assert!(bind_result.is_ok(), "{:?}", bind_result.err());

    let (send_result, stdout) = s.run(&["tfmux", "send", "agent1", "--text", "hello"]);

    assert!(send_result.is_ok(), "{:?}", send_result.err());
    assert_eq!(stdout, "sent 5 bytes to \"agent1\" (%5)\n");
}

#[test]
fn send_resolves_session_from_local_marker() {
    let s = Scenario::new().marker("markerdemo");
    let (bind_result, _) = s.run(&["tfmux", "bind", "agent1", "--tmux", "sess:1.0"]);
    assert!(bind_result.is_ok(), "{:?}", bind_result.err());

    let (send_result, stdout) = s.run(&["tfmux", "send", "agent1", "--text", "hello"]);

    assert!(send_result.is_ok(), "{:?}", send_result.err());
    assert_eq!(stdout, "sent 5 bytes to \"agent1\" (%5)\n");
}

#[test]
fn send_does_not_create_global_current_pointer() {
    let s = Scenario::new();
    let (bind_result, _) = s.run(&[
        "tfmux",
        "bind",
        "agent1",
        "--tmux",
        "sess:1.0",
        "--session",
        "demo",
    ]);
    assert!(bind_result.is_ok(), "{:?}", bind_result.err());

    let (send_result, _) = s.run(&[
        "tfmux",
        "send",
        "agent1",
        "--text",
        "hello",
        "--session",
        "demo",
    ]);

    assert!(send_result.is_ok(), "{:?}", send_result.err());
    assert!(!s.home().join("current").exists());
    assert_eq!(s.home_entry_count(), 1);
}

#[test]
fn unbind_explicit_session_removes_only_target_file() {
    let s = Scenario::new();
    s.save_target("demo", "agent1");
    s.save_target("demo", "agent2");
    let session_dir = s.session_dir("demo").unwrap();
    std::fs::write(session_dir.join("events.jsonl"), "keep\n").unwrap();

    let (result, stdout) = s.run(&["tfmux", "unbind", "agent1", "--session", "demo"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(stdout, "unbound \"agent1\" from session demo\n");
    assert!(!s.target_path("demo", "agent1").exists());
    assert!(s.target_path("demo", "agent2").exists());
    assert!(session_dir.join("session.json").exists());
    assert_eq!(
        std::fs::read_to_string(session_dir.join("events.jsonl")).unwrap(),
        "keep\n"
    );
    assert_eq!(s.built(), 0, "unbind must not construct tmux");
    assert!(!s.home().join("current").exists());
    assert_eq!(s.home_entry_count(), 1);
}

#[test]
fn unbind_json_prints_stable_payload() {
    let s = Scenario::new();
    s.save_target("demo", "agent1");

    let (result, stdout) = s.run(&["tfmux", "unbind", "agent1", "--session", "demo", "--json"]);

    assert!(result.is_ok(), "{:?}", result.err());
    let printed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        printed,
        json!({
            "session": "demo",
            "name": "agent1",
            "removed": true,
        })
    );
    assert!(!s.target_path("demo", "agent1").exists());
    assert_eq!(s.built(), 0);
}

#[test]
fn unbind_resolves_session_from_env_var() {
    let s = Scenario::new().env("TFMUX_SESSION", "envdemo");
    s.save_target("envdemo", "agent1");

    let (result, stdout) = s.run(&["tfmux", "unbind", "agent1"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(stdout, "unbound \"agent1\" from session envdemo\n");
    assert!(!s.target_path("envdemo", "agent1").exists());
    assert_eq!(s.built(), 0);
}

#[test]
fn unbind_resolves_session_from_local_marker() {
    let s = Scenario::new().marker("markerdemo");
    s.save_target("markerdemo", "agent1");

    let (result, stdout) = s.run(&["tfmux", "unbind", "agent1"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(stdout, "unbound \"agent1\" from session markerdemo\n");
    assert!(!s.target_path("markerdemo", "agent1").exists());
    assert_eq!(s.built(), 0);
}

#[test]
fn unbind_missing_session_errors_without_creating_state() {
    let s = Scenario::new();

    let (result, _) = s.run(&["tfmux", "unbind", "agent1", "--session", "demo"]);

    let err = result.unwrap_err().to_string();
    assert_eq!(err, "no tfmux session \"demo\"");
    assert_eq!(s.built(), 0);
    assert_eq!(s.home_entry_count(), 0);
    assert!(!s.home().join("current").exists());
}

#[test]
fn unbind_missing_target_errors_cleanly() {
    let s = Scenario::new();
    Store::new(s.home().to_path_buf())
        .create_session("demo", fixed_now())
        .unwrap();

    let (result, _) = s.run(&["tfmux", "unbind", "agent1", "--session", "demo"]);

    let err = result.unwrap_err().to_string();
    assert_eq!(err, "no target \"agent1\" in session demo");
    assert_eq!(s.built(), 0);
    assert!(!s.home().join("current").exists());
}

#[test]
fn unbind_invalid_name_errors_before_any_write() {
    let s = Scenario::new();

    let (result, _) = s.run(&["tfmux", "unbind", "bad/name", "--session", "demo"]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("invalid name \"bad/name\""), "got: {err}");
    assert_eq!(s.built(), 0);
    assert_eq!(s.home_entry_count(), 0);
    assert!(!s.home().join("current").exists());
}

#[test]
fn targets_session_prints_table_for_bound_targets() {
    let s = Scenario::new()
        .resolve_override("%5", Ok(pane_ref_at("%5", "work", "1", "0")))
        .resolve_override("%3", Err("can't find pane %3".to_string()));
    s.save_target_record(
        "demo",
        stored_target("mediator", "mediator", "codex", "%3", "work", "0", "0"),
    );
    s.save_target_record(
        "demo",
        stored_target("agent1", "agent", "claude", "%5", "work", "1", "0"),
    );

    let (result, stdout) = s.run(&["tfmux", "targets", "--session", "demo"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(
        stdout,
        concat!(
            "NAME       ROLE     KIND     PANE   LOCATION       STATUS\n",
            "agent1     agent    claude   %5     work:1.0       live\n",
            "mediator   mediator codex    %3     work:0.0       dead\n",
        )
    );
    assert_eq!(s.resolved(), vec!["%5".to_string(), "%3".to_string()]);
}

#[test]
fn targets_json_session_prints_stable_valid_json() {
    let s = Scenario::new()
        .resolve_override("%5", Ok(pane_ref_at("%5", "work", "1", "0")))
        .resolve_override("%6", Ok(pane_ref_at("%7", "other", "2", "1")))
        .resolve_override("%3", Err("can't find pane %3".to_string()));
    s.save_target_record(
        "demo",
        stored_target("mediator", "mediator", "codex", "%3", "work", "0", "0"),
    );
    s.save_target_record(
        "demo",
        stored_target("agent2", "agent", "generic", "%6", "work", "2", "1"),
    );
    s.save_target_record(
        "demo",
        stored_target("agent1", "agent", "claude", "%5", "work", "1", "0"),
    );

    let (result, stdout) = s.run(&["tfmux", "targets", "--json", "--session", "demo"]);

    assert!(result.is_ok(), "{:?}", result.err());
    let rows: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["name"], "agent1");
    assert_eq!(rows[0]["status"], "live");
    assert!(rows[0].get("actual_pane").is_none());
    assert!(rows[0].get("error").is_none());
    assert_eq!(rows[1]["name"], "agent2");
    assert_eq!(rows[1]["status"], "stale");
    assert_eq!(rows[1]["actual_pane"]["pane_id"], "%7");
    assert_eq!(rows[1]["actual_pane"]["session"], "other");
    assert_eq!(rows[2]["name"], "mediator");
    assert_eq!(rows[2]["status"], "dead");
    assert_eq!(rows[2]["error"], "can't find pane %3");
}

#[test]
fn targets_resolves_session_from_env_var() {
    let s = Scenario::new()
        .env("TFMUX_SESSION", "envdemo")
        .resolve_override("%5", Ok(pane_ref_at("%5", "sess", "1", "0")));
    s.save_target("envdemo", "agent1");

    let (result, stdout) = s.run(&["tfmux", "targets"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert!(stdout.contains("agent1"), "stdout: {stdout}");
}

#[test]
fn targets_resolves_session_from_local_marker() {
    let s = Scenario::new()
        .marker("markerdemo")
        .resolve_override("%5", Ok(pane_ref_at("%5", "sess", "1", "0")));
    s.save_target("markerdemo", "agent1");

    let (result, stdout) = s.run(&["tfmux", "targets"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert!(stdout.contains("agent1"), "stdout: {stdout}");
}

#[test]
fn targets_without_any_session_selection_errors_without_creating_state() {
    let s = Scenario::new();

    let (result, _) = s.run(&["tfmux", "targets"]);

    let err = result.unwrap_err().to_string();
    assert_eq!(
        err,
        "no tfmux session selected; pass --session NAME, set TFMUX_SESSION, or add .llm/tfmux-session"
    );
    assert_eq!(s.built(), 0);
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn targets_missing_session_errors_without_creating_state() {
    let s = Scenario::new();

    let (result, _) = s.run(&["tfmux", "targets", "--session", "demo"]);

    let err = result.unwrap_err().to_string();
    assert_eq!(err, "no tfmux session \"demo\"");
    assert_eq!(s.built(), 0);
    assert_eq!(s.home_entry_count(), 0);
}

#[test]
fn targets_does_not_create_global_current_pointer() {
    let s = Scenario::new().resolve_override("%5", Ok(pane_ref_at("%5", "sess", "1", "0")));
    s.save_target("demo", "agent1");

    let (result, _) = s.run(&["tfmux", "targets", "--session", "demo"]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert!(!s.home().join("current").exists());
    assert_eq!(s.home_entry_count(), 1);
}
