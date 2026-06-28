//! End-to-end `tfmux bind` tests driven through `tfmux::run` with a fake tmux
//! backend, a temp `TFMUX_HOME`, an injected env/cwd, and a fixed clock.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use chrono::{DateTime, TimeZone, Utc};
use clap::Parser;
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
}

/// Fake tmux: records each resolve call and returns a scripted pane (or error).
struct FakeMux {
    calls: Rc<RefCell<MuxCalls>>,
    pane: PaneRef,
    err: Option<String>,
}

impl Mux for FakeMux {
    fn resolve_pane(&self, target: &str) -> anyhow::Result<PaneRef> {
        self.calls.borrow_mut().resolved.push(target.to_string());
        if let Some(e) = &self.err {
            anyhow::bail!("{e}");
        }
        Ok(PaneRef {
            input: target.to_string(),
            ..self.pane.clone()
        })
    }

    fn load_buffer(&self, _name: &str, _text: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn paste_buffer(&self, _name: &str, _pane_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn send_enter(&self, _pane_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn capture_pane(&self, _pane_id: &str, _scrollback: i32) -> anyhow::Result<String> {
        Ok(String::new())
    }
}

/// Builder + driver for a single bind invocation.
struct Scenario {
    home: TempDir,
    cwd: TempDir,
    env: HashMap<String, String>,
    pane: PaneRef,
    resolve_err: Option<String>,
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
        let new_mux = move || -> anyhow::Result<Box<dyn Mux>> {
            calls.borrow_mut().built += 1;
            Ok(Box::new(FakeMux {
                calls: calls.clone(),
                pane: pane.clone(),
                err: err.clone(),
            }))
        };
        let read_stdin = || -> anyhow::Result<String> { Ok(String::new()) };

        let mut out: Vec<u8> = Vec::new();
        let mut app = App {
            base_dir: self.home().to_path_buf(),
            env: &env_fn,
            cwd: self.cwd.path().to_path_buf(),
            now: &now_fn,
            new_mux: &new_mux,
            read_stdin: &read_stdin,
            out: &mut out,
        };
        let result = tfmux::run(&mut app, cli);
        (result, String::from_utf8(out).unwrap())
    }
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
