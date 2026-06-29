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
use tfmux::git::{Git, GitHub, PullRequest};
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
    killed_sessions: Vec<String>,
}

#[derive(Clone, Default)]
struct GitCalls {
    roots: Vec<PathBuf>,
    statuses: Vec<PathBuf>,
    branches: Vec<PathBuf>,
    fetches: Vec<(PathBuf, String, String)>,
    pulls: Vec<(PathBuf, String, String)>,
    ancestors: Vec<(PathBuf, String, String)>,
    removed_worktrees: Vec<(PathBuf, PathBuf)>,
}

#[derive(Clone, Default)]
struct GitHubCalls {
    prs: Vec<(PathBuf, String)>,
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

    fn kill_session(&self, session: &str) -> anyhow::Result<()> {
        self.calls
            .borrow_mut()
            .killed_sessions
            .push(session.to_string());
        Ok(())
    }
}

/// Fake Git backend for `tfmux detach` command sequencing tests.
struct FakeGit {
    calls: Rc<RefCell<GitCalls>>,
    repo_root: PathBuf,
    worktree_root: PathBuf,
    repo_status: String,
    worktree_status: String,
    repo_branch: String,
    worktree_branch: String,
    worktree_root_err: Option<String>,
    remove_worktree_err: Option<String>,
    ancestor_err: Option<String>,
}

impl Git for FakeGit {
    fn root(&self, cwd: &Path) -> anyhow::Result<PathBuf> {
        self.calls.borrow_mut().roots.push(cwd.to_path_buf());
        if cwd == self.repo_root {
            return Ok(self.repo_root.clone());
        }
        if cwd == self.worktree_root {
            if let Some(err) = &self.worktree_root_err {
                anyhow::bail!("{err}");
            }
            return Ok(self.worktree_root.clone());
        }
        anyhow::bail!("not a git repository: {}", cwd.display())
    }

    fn status_porcelain(&self, cwd: &Path) -> anyhow::Result<String> {
        self.calls.borrow_mut().statuses.push(cwd.to_path_buf());
        if cwd == self.repo_root {
            return Ok(self.repo_status.clone());
        }
        if cwd == self.worktree_root {
            return Ok(self.worktree_status.clone());
        }
        anyhow::bail!("not a git repository: {}", cwd.display())
    }

    fn current_branch(&self, cwd: &Path) -> anyhow::Result<String> {
        self.calls.borrow_mut().branches.push(cwd.to_path_buf());
        if cwd == self.repo_root {
            return Ok(self.repo_branch.clone());
        }
        if cwd == self.worktree_root {
            return Ok(self.worktree_branch.clone());
        }
        anyhow::bail!("not a git repository: {}", cwd.display())
    }

    fn fetch(&self, repo: &Path, remote: &str, main: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().fetches.push((
            repo.to_path_buf(),
            remote.to_string(),
            main.to_string(),
        ));
        Ok(())
    }

    fn pull_ff_only(&self, repo: &Path, remote: &str, main: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().pulls.push((
            repo.to_path_buf(),
            remote.to_string(),
            main.to_string(),
        ));
        Ok(())
    }

    fn merge_base_is_ancestor(
        &self,
        repo: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> anyhow::Result<()> {
        self.calls.borrow_mut().ancestors.push((
            repo.to_path_buf(),
            ancestor.to_string(),
            descendant.to_string(),
        ));
        if let Some(err) = &self.ancestor_err {
            anyhow::bail!("{err}");
        }
        Ok(())
    }

    fn remove_worktree(&self, repo: &Path, worktree: &Path) -> anyhow::Result<()> {
        self.calls
            .borrow_mut()
            .removed_worktrees
            .push((repo.to_path_buf(), worktree.to_path_buf()));
        if let Some(err) = &self.remove_worktree_err {
            anyhow::bail!("{err}");
        }
        Ok(())
    }
}

/// Fake GitHub backend for PR-state checks.
struct FakeGitHub {
    calls: Rc<RefCell<GitHubCalls>>,
    pr: PullRequest,
}

impl GitHub for FakeGitHub {
    fn pull_request(&self, repo: &Path, pr: &str) -> anyhow::Result<PullRequest> {
        self.calls
            .borrow_mut()
            .prs
            .push((repo.to_path_buf(), pr.to_string()));
        Ok(self.pr.clone())
    }
}

/// Builder + driver for a single bind invocation.
struct Scenario {
    home: TempDir,
    cwd: TempDir,
    worktree: TempDir,
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
    git_calls: Rc<RefCell<GitCalls>>,
    github_calls: Rc<RefCell<GitHubCalls>>,
    repo_status: String,
    worktree_status: String,
    repo_branch: String,
    worktree_branch: String,
    worktree_root_err: Option<String>,
    remove_worktree_err: Option<String>,
    ancestor_err: Option<String>,
    pr: PullRequest,
}

impl Scenario {
    fn new() -> Self {
        Scenario {
            home: tempfile::tempdir().unwrap(),
            cwd: tempfile::tempdir().unwrap(),
            worktree: tempfile::tempdir().unwrap(),
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
            git_calls: Rc::new(RefCell::new(GitCalls::default())),
            github_calls: Rc::new(RefCell::new(GitHubCalls::default())),
            repo_status: String::new(),
            worktree_status: String::new(),
            repo_branch: "main".to_string(),
            worktree_branch: "feat/demo".to_string(),
            worktree_root_err: None,
            remove_worktree_err: None,
            ancestor_err: None,
            pr: PullRequest {
                state: "MERGED".to_string(),
                merged_at: Some("2026-06-28T20:00:00Z".to_string()),
                merge_commit: Some("abc123".to_string()),
                head_ref_name: "feat/demo".to_string(),
                base_ref_name: "main".to_string(),
                url: "https://github.com/example/repo/pull/12".to_string(),
            },
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

    fn worktree_status(mut self, status: &str) -> Self {
        self.worktree_status = status.to_string();
        self
    }

    fn repo_status(mut self, status: &str) -> Self {
        self.repo_status = status.to_string();
        self
    }

    fn repo_branch(mut self, branch: &str) -> Self {
        self.repo_branch = branch.to_string();
        self
    }

    fn worktree_branch(mut self, branch: &str) -> Self {
        self.worktree_branch = branch.to_string();
        self.pr.head_ref_name = branch.to_string();
        self
    }

    fn worktree_root_err(mut self, msg: &str) -> Self {
        self.worktree_root_err = Some(msg.to_string());
        self
    }

    fn remove_worktree_err(mut self, msg: &str) -> Self {
        self.remove_worktree_err = Some(msg.to_string());
        self
    }

    fn ancestor_err(mut self, msg: &str) -> Self {
        self.ancestor_err = Some(msg.to_string());
        self
    }

    fn pr_state(mut self, state: &str) -> Self {
        self.pr.state = state.to_string();
        self
    }

    fn pr_base(mut self, base: &str) -> Self {
        self.pr.base_ref_name = base.to_string();
        self
    }

    fn pr_head(mut self, head: &str) -> Self {
        self.pr.head_ref_name = head.to_string();
        self
    }

    fn pr_merge_commit(mut self, merge_commit: Option<&str>) -> Self {
        self.pr.merge_commit = merge_commit.map(str::to_string);
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

    fn repo(&self) -> PathBuf {
        self.cwd.path().canonicalize().unwrap()
    }

    fn worktree(&self) -> PathBuf {
        self.worktree.path().canonicalize().unwrap()
    }

    fn worktree_arg(&self) -> String {
        self.worktree().display().to_string()
    }

    fn repo_arg(&self) -> String {
        self.repo().display().to_string()
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

    fn killed_sessions(&self) -> Vec<String> {
        self.calls.borrow().killed_sessions.clone()
    }

    fn git_calls(&self) -> GitCalls {
        self.git_calls.borrow().clone()
    }

    fn github_calls(&self) -> GitHubCalls {
        self.github_calls.borrow().clone()
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
        let git_calls = self.git_calls.clone();
        let repo_root = self.repo();
        let worktree_root = self.worktree();
        let repo_status = self.repo_status.clone();
        let worktree_status = self.worktree_status.clone();
        let repo_branch = self.repo_branch.clone();
        let worktree_branch = self.worktree_branch.clone();
        let worktree_root_err = self.worktree_root_err.clone();
        let remove_worktree_err = self.remove_worktree_err.clone();
        let ancestor_err = self.ancestor_err.clone();
        let new_git = move || -> anyhow::Result<Box<dyn Git>> {
            Ok(Box::new(FakeGit {
                calls: git_calls.clone(),
                repo_root: repo_root.clone(),
                worktree_root: worktree_root.clone(),
                repo_status: repo_status.clone(),
                worktree_status: worktree_status.clone(),
                repo_branch: repo_branch.clone(),
                worktree_branch: worktree_branch.clone(),
                worktree_root_err: worktree_root_err.clone(),
                remove_worktree_err: remove_worktree_err.clone(),
                ancestor_err: ancestor_err.clone(),
            }))
        };
        let github_calls = self.github_calls.clone();
        let pr = self.pr.clone();
        let new_github = move || -> anyhow::Result<Box<dyn GitHub>> {
            Ok(Box::new(FakeGitHub {
                calls: github_calls.clone(),
                pr: pr.clone(),
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
            new_git: &new_git,
            new_github: &new_github,
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
fn detach_real_binary_validation_does_not_require_home() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tfmux"))
        .args([
            "detach",
            "worker",
            "--worktree",
            "/tmp/tfmux-detach-missing-worktree",
            "--pr",
            "12",
        ])
        .env_remove("HOME")
        .env_remove("TFMUX_HOME")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: --worktree /tmp/tfmux-detach-missing-worktree does not exist\n"
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
fn detach_success_updates_main_removes_worktree_then_kills_tmux() {
    let s = Scenario::new();
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, stdout) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(
        stdout,
        format!(
            "detached \"worker\"\nworktree removed: {worktree}\nmain updated: origin/main\npr verified: https://github.com/example/repo/pull/12\nbranch kept: feat/demo\n"
        )
    );
    assert_eq!(s.has_sessions(), vec!["worker".to_string()]);
    assert_eq!(s.killed_sessions(), vec!["worker".to_string()]);

    let git = s.git_calls();
    assert_eq!(
        git.fetches,
        vec![(s.repo(), "origin".to_string(), "main".to_string())]
    );
    assert_eq!(
        git.pulls,
        vec![(s.repo(), "origin".to_string(), "main".to_string())]
    );
    assert_eq!(
        git.ancestors,
        vec![(s.repo(), "abc123".to_string(), "main".to_string())]
    );
    assert_eq!(git.removed_worktrees, vec![(s.repo(), s.worktree())]);

    let github = s.github_calls();
    assert_eq!(github.prs, vec![(s.repo(), "12".to_string())]);
}

#[test]
fn detach_dry_run_prints_plan_without_destructive_actions() {
    let s = Scenario::new();
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, stdout) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
        "--dry-run",
    ]);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(
        stdout,
        format!(
            "dry run: would detach \"worker\"\nwould verify PR 12 is merged into main\nwould fetch origin main in {repo}\nwould pull --ff-only origin main in {repo}\nwould remove worktree {worktree}\nwould kill tmux session worker\n"
        )
    );
    assert_eq!(s.has_sessions(), vec!["worker".to_string()]);
    assert!(s.killed_sessions().is_empty());

    let git = s.git_calls();
    assert!(git.fetches.is_empty());
    assert!(git.pulls.is_empty());
    assert!(git.removed_worktrees.is_empty());
}

#[test]
fn detach_rejects_dirty_worktree_before_pr_or_destructive_actions() {
    let s = Scenario::new().worktree_status(" M src/lib.rs\n?? scratch.txt\n");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("has uncommitted or untracked changes"),
        "got: {err}"
    );
    assert!(
        err.contains("commit, stash, or remove them before detach"),
        "got: {err}"
    );
    assert!(s.github_calls().prs.is_empty());
    let git = s.git_calls();
    assert!(git.fetches.is_empty());
    assert!(git.pulls.is_empty());
    assert!(git.removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_unmerged_pr_before_fetch_or_cleanup() {
    let s = Scenario::new().pr_state("OPEN");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("PR 12 is OPEN, not MERGED"), "got: {err}");
    let git = s.git_calls();
    assert!(git.fetches.is_empty());
    assert!(git.pulls.is_empty());
    assert!(git.removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_repo_not_on_main_before_pr_or_cleanup() {
    let s = Scenario::new().repo_branch("feat/other");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("is on branch feat/other, not main"),
        "got: {err}"
    );
    assert!(s.github_calls().prs.is_empty());
    assert!(s.git_calls().removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_repo_and_worktree_resolving_to_same_checkout() {
    let s = Scenario::new();
    let worktree = s.worktree_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &worktree,
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("--repo and --worktree both resolve to"),
        "got: {err}"
    );
    assert!(
        err.contains("run from the main checkout or pass --repo PATH"),
        "got: {err}"
    );
    assert!(s.github_calls().prs.is_empty());
    assert!(s.git_calls().removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_dirty_main_checkout_before_pr_or_cleanup() {
    let s = Scenario::new().repo_status(" M README.md\n");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("repo main checkout") && err.contains("has uncommitted"),
        "got: {err}"
    );
    assert!(s.github_calls().prs.is_empty());
    assert!(s.git_calls().removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_non_git_worktree_before_pr_or_cleanup() {
    let s = Scenario::new().worktree_root_err("not a git repository");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("not a git repository"), "got: {err}");
    assert!(s.github_calls().prs.is_empty());
    assert!(s.git_calls().removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_explicit_branch_mismatch_before_pr_or_cleanup() {
    let s = Scenario::new().worktree_branch("feat/actual");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--branch",
        "feat/expected",
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("worktree is on branch feat/actual, not feat/expected"),
        "got: {err}"
    );
    assert!(s.github_calls().prs.is_empty());
    assert!(s.git_calls().removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_pr_base_mismatch_before_fetch_or_cleanup() {
    let s = Scenario::new().pr_base("develop");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("targets develop, not main"), "got: {err}");
    let git = s.git_calls();
    assert!(git.fetches.is_empty());
    assert!(git.pulls.is_empty());
    assert!(git.removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_pr_head_mismatch_before_fetch_or_cleanup() {
    let s = Scenario::new().pr_head("feat/other");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("head branch is feat/other, not feat/demo"),
        "got: {err}"
    );
    assert!(s.git_calls().removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_pr_without_merge_commit_before_fetch_or_cleanup() {
    let s = Scenario::new().pr_merge_commit(None);
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = result.unwrap_err().to_string();
    assert!(err.contains("has no merge commit"), "got: {err}");
    let git = s.git_calls();
    assert!(git.fetches.is_empty());
    assert!(git.pulls.is_empty());
    assert!(git.removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_rejects_merge_commit_not_on_main_after_update() {
    let s = Scenario::new().ancestor_err("not an ancestor");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = format!("{:#}", result.unwrap_err());
    assert!(
        err.contains("PR merge commit abc123 is not on main"),
        "got: {err}"
    );
    assert!(err.contains("not an ancestor"), "got: {err}");
    let git = s.git_calls();
    assert_eq!(
        git.fetches,
        vec![(s.repo(), "origin".to_string(), "main".to_string())]
    );
    assert_eq!(
        git.pulls,
        vec![(s.repo(), "origin".to_string(), "main".to_string())]
    );
    assert!(git.removed_worktrees.is_empty());
    assert!(s.killed_sessions().is_empty());
}

#[test]
fn detach_worktree_remove_failure_does_not_kill_tmux() {
    let s = Scenario::new().remove_worktree_err("worktree contains locked files");
    let worktree = s.worktree_arg();
    let repo = s.repo_arg();

    let (result, _) = s.run(&[
        "tfmux",
        "detach",
        "worker",
        "--worktree",
        &worktree,
        "--repo",
        &repo,
        "--pr",
        "12",
    ]);

    let err = format!("{:#}", result.unwrap_err());
    assert!(err.contains("removing worktree"), "got: {err}");
    assert!(err.contains("worktree contains locked files"), "got: {err}");
    let git = s.git_calls();
    assert_eq!(git.removed_worktrees, vec![(s.repo(), s.worktree())]);
    assert!(s.killed_sessions().is_empty());
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
