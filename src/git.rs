//! Git and GitHub CLI subprocess boundaries for cleanup operations.
//!
//! The traits here are injected through `App` so command tests can assert the
//! cleanup sequence without touching a real repository or GitHub.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Minimal Git operations needed by `tfmux detach`.
pub trait Git {
    /// Resolve the repository root for `cwd`.
    fn root(&self, cwd: &Path) -> Result<PathBuf>;

    /// Return `git status --porcelain --untracked-files=all` output.
    fn status_porcelain(&self, cwd: &Path) -> Result<String>;

    /// Return the current branch name.
    fn current_branch(&self, cwd: &Path) -> Result<String>;

    /// Fetch `main` from `remote` into the local repository.
    fn fetch(&self, repo: &Path, remote: &str, main: &str) -> Result<()>;

    /// Fast-forward-only pull of `main` from `remote`.
    fn pull_ff_only(&self, repo: &Path, remote: &str, main: &str) -> Result<()>;

    /// Verify `ancestor` is an ancestor of `descendant`.
    fn merge_base_is_ancestor(&self, repo: &Path, ancestor: &str, descendant: &str) -> Result<()>;

    /// Remove a linked worktree from `repo`.
    fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<()>;
}

/// GitHub PR fields used to prove squash-merged cleanup is safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub state: String,
    pub merged_at: Option<String>,
    pub merge_commit: Option<String>,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub url: String,
}

/// Minimal GitHub CLI operations needed by `tfmux detach`.
pub trait GitHub {
    /// Load PR metadata by number or URL.
    fn pull_request(&self, repo: &Path, pr: &str) -> Result<PullRequest>;
}

/// Real Git implementation backed by the `git` executable.
pub struct CliGit {
    bin: PathBuf,
}

impl CliGit {
    /// Resolve `git` from `PATH`.
    pub fn from_env() -> Result<Self> {
        let bin = which::which("git").map_err(|e| anyhow!("git binary not found on PATH: {e}"))?;
        Ok(Self { bin })
    }

    fn run(&self, cwd: &Path, args: &[&OsStr]) -> Result<String> {
        let output = Command::new(&self.bin)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("running git in {}", cwd.display()))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let detail = command_detail(&output.stdout, &output.stderr);
        let sub = args.first().and_then(|arg| arg.to_str()).unwrap_or("git");
        if detail.is_empty() {
            bail!("git {sub} failed");
        }
        bail!("git {sub} failed: {detail}");
    }
}

impl Git for CliGit {
    fn root(&self, cwd: &Path) -> Result<PathBuf> {
        let out = self.run(
            cwd,
            &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        )?;
        let root = out.trim();
        if root.is_empty() {
            bail!("git root was empty for {}", cwd.display());
        }
        Ok(PathBuf::from(root))
    }

    fn status_porcelain(&self, cwd: &Path) -> Result<String> {
        self.run(
            cwd,
            &[
                OsStr::new("status"),
                OsStr::new("--porcelain"),
                OsStr::new("--untracked-files=all"),
            ],
        )
    }

    fn current_branch(&self, cwd: &Path) -> Result<String> {
        let out = self.run(cwd, &[OsStr::new("branch"), OsStr::new("--show-current")])?;
        let branch = out.trim();
        if branch.is_empty() {
            bail!("git checkout is detached; check out a branch before detach");
        }
        Ok(branch.to_string())
    }

    fn fetch(&self, repo: &Path, remote: &str, main: &str) -> Result<()> {
        self.run(
            repo,
            &[OsStr::new("fetch"), OsStr::new(remote), OsStr::new(main)],
        )?;
        Ok(())
    }

    fn pull_ff_only(&self, repo: &Path, remote: &str, main: &str) -> Result<()> {
        self.run(
            repo,
            &[
                OsStr::new("pull"),
                OsStr::new("--ff-only"),
                OsStr::new(remote),
                OsStr::new(main),
            ],
        )?;
        Ok(())
    }

    fn merge_base_is_ancestor(&self, repo: &Path, ancestor: &str, descendant: &str) -> Result<()> {
        self.run(
            repo,
            &[
                OsStr::new("merge-base"),
                OsStr::new("--is-ancestor"),
                OsStr::new(ancestor),
                OsStr::new(descendant),
            ],
        )?;
        Ok(())
    }

    fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<()> {
        self.run(
            repo,
            &[
                OsStr::new("worktree"),
                OsStr::new("remove"),
                worktree.as_os_str(),
            ],
        )?;
        Ok(())
    }
}

/// Real GitHub implementation backed by the `gh` executable.
pub struct CliGitHub {
    bin: PathBuf,
}

impl CliGitHub {
    /// Resolve `gh` from `PATH`.
    pub fn from_env() -> Result<Self> {
        let bin = which::which("gh").map_err(|e| anyhow!("gh binary not found on PATH: {e}"))?;
        Ok(Self { bin })
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.bin)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("running gh in {}", cwd.display()))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let detail = command_detail(&output.stdout, &output.stderr);
        let sub = args.first().copied().unwrap_or("gh");
        if detail.is_empty() {
            bail!("gh {sub} failed");
        }
        bail!("gh {sub} failed: {detail}");
    }
}

impl GitHub for CliGitHub {
    fn pull_request(&self, repo: &Path, pr: &str) -> Result<PullRequest> {
        let raw = self.run(
            repo,
            &[
                "pr",
                "view",
                pr,
                "--json",
                "state,mergedAt,mergeCommit,headRefName,baseRefName,url",
            ],
        )?;
        let parsed: GhPullRequest = serde_json::from_str(&raw).context("decoding gh PR JSON")?;
        Ok(parsed.into())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPullRequest {
    state: String,
    merged_at: Option<String>,
    merge_commit: Option<GhCommit>,
    head_ref_name: String,
    base_ref_name: String,
    url: String,
}

#[derive(Deserialize)]
struct GhCommit {
    oid: String,
}

impl From<GhPullRequest> for PullRequest {
    fn from(value: GhPullRequest) -> Self {
        Self {
            state: value.state,
            merged_at: value.merged_at,
            merge_commit: value.merge_commit.map(|commit| commit.oid),
            head_ref_name: value.head_ref_name,
            base_ref_name: value.base_ref_name,
            url: value.url,
        }
    }
}

fn command_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut detail = String::from_utf8_lossy(stdout).into_owned();
    detail.push_str(&String::from_utf8_lossy(stderr));
    detail.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn cli_git_root_issues_exact_argv_and_trims_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_bin(dir.path(), "/tmp/repo\n");
        let git = CliGit { bin };

        let root = git.root(dir.path()).unwrap();

        assert_eq!(root, PathBuf::from("/tmp/repo"));
        assert_eq!(
            recorded_args(dir.path()),
            vec!["rev-parse", "--show-toplevel"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_git_status_issues_exact_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_bin(dir.path(), " M README.md\n");
        let git = CliGit { bin };

        let status = git.status_porcelain(dir.path()).unwrap();

        assert_eq!(status, " M README.md\n");
        assert_eq!(
            recorded_args(dir.path()),
            vec!["status", "--porcelain", "--untracked-files=all"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_git_current_branch_issues_exact_argv_and_trims_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_bin(dir.path(), "feature/demo\n");
        let git = CliGit { bin };

        let branch = git.current_branch(dir.path()).unwrap();

        assert_eq!(branch, "feature/demo");
        assert_eq!(recorded_args(dir.path()), vec!["branch", "--show-current"]);
    }

    #[cfg(unix)]
    #[test]
    fn cli_git_current_branch_rejects_detached_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_bin(dir.path(), "\n");
        let git = CliGit { bin };

        let err = git.current_branch(dir.path()).unwrap_err().to_string();

        assert!(err.contains("git checkout is detached"), "got: {err}");
        assert_eq!(recorded_args(dir.path()), vec!["branch", "--show-current"]);
    }

    #[cfg(unix)]
    #[test]
    fn cli_git_fetch_issues_exact_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_bin(dir.path(), "");
        let git = CliGit { bin };

        git.fetch(dir.path(), "origin", "main").unwrap();

        assert_eq!(recorded_args(dir.path()), vec!["fetch", "origin", "main"]);
    }

    #[cfg(unix)]
    #[test]
    fn cli_git_pull_ff_only_issues_exact_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_bin(dir.path(), "");
        let git = CliGit { bin };

        git.pull_ff_only(dir.path(), "upstream", "trunk").unwrap();

        assert_eq!(
            recorded_args(dir.path()),
            vec!["pull", "--ff-only", "upstream", "trunk"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_git_merge_base_is_ancestor_issues_exact_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_bin(dir.path(), "");
        let git = CliGit { bin };

        git.merge_base_is_ancestor(dir.path(), "abc123", "main")
            .unwrap();

        assert_eq!(
            recorded_args(dir.path()),
            vec!["merge-base", "--is-ancestor", "abc123", "main"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_git_remove_worktree_issues_exact_argv() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_bin(dir.path(), "");
        let git = CliGit { bin };
        let worktree = dir.path().join("worker");

        git.remove_worktree(dir.path(), &worktree).unwrap();

        assert_eq!(
            recorded_args(dir.path()),
            vec![
                "worktree".to_string(),
                "remove".to_string(),
                worktree.display().to_string(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_github_pull_request_issues_exact_argv_and_parses_merged_pr() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "state": "MERGED",
            "mergedAt": "2026-06-28T12:00:00Z",
            "mergeCommit": { "oid": "abc123" },
            "headRefName": "feat/demo",
            "baseRefName": "main",
            "url": "https://github.com/example/repo/pull/12"
        }"#;
        let bin = write_recording_bin(dir.path(), json);
        let github = CliGitHub { bin };

        let pr = github.pull_request(dir.path(), "12").unwrap();

        assert_eq!(
            recorded_args(dir.path()),
            vec![
                "pr",
                "view",
                "12",
                "--json",
                "state,mergedAt,mergeCommit,headRefName,baseRefName,url"
            ]
        );
        assert_eq!(pr.state, "MERGED");
        assert_eq!(pr.merged_at.as_deref(), Some("2026-06-28T12:00:00Z"));
        assert_eq!(pr.merge_commit.as_deref(), Some("abc123"));
        assert_eq!(pr.head_ref_name, "feat/demo");
        assert_eq!(pr.base_ref_name, "main");
        assert_eq!(pr.url, "https://github.com/example/repo/pull/12");
    }

    #[cfg(unix)]
    #[test]
    fn cli_github_pull_request_parses_null_merge_fields() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "state": "CLOSED",
            "mergedAt": null,
            "mergeCommit": null,
            "headRefName": "feat/demo",
            "baseRefName": "main",
            "url": "https://github.com/example/repo/pull/12"
        }"#;
        let bin = write_recording_bin(dir.path(), json);
        let github = CliGitHub { bin };

        let pr = github
            .pull_request(dir.path(), "https://github.com/example/repo/pull/12")
            .unwrap();

        assert_eq!(
            recorded_args(dir.path()),
            vec![
                "pr",
                "view",
                "https://github.com/example/repo/pull/12",
                "--json",
                "state,mergedAt,mergeCommit,headRefName,baseRefName,url"
            ]
        );
        assert_eq!(pr.state, "CLOSED");
        assert_eq!(pr.merged_at, None);
        assert_eq!(pr.merge_commit, None);
    }

    #[cfg(unix)]
    #[test]
    fn cli_github_pull_request_surfaces_json_decode_errors() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_recording_bin(dir.path(), "not json");
        let github = CliGitHub { bin };

        let err = github
            .pull_request(dir.path(), "12")
            .unwrap_err()
            .to_string();

        assert!(err.contains("decoding gh PR JSON"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn cli_git_surfaces_command_failure_detail() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_failing_bin(dir.path(), "fatal: no git repo");
        let git = CliGit { bin };

        let err = git.root(dir.path()).unwrap_err().to_string();

        assert!(err.contains("git rev-parse failed"), "got: {err}");
        assert!(err.contains("fatal: no git repo"), "got: {err}");
    }

    #[cfg(unix)]
    fn write_recording_bin(dir: &Path, response: &str) -> PathBuf {
        let argv = dir.join("argv.txt");
        let resp = dir.join("response.txt");
        fs::write(&resp, response).unwrap();
        let script = format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv:?}\ncat {resp:?}\n");
        write_script(dir.join("fake-bin"), &script)
    }

    #[cfg(unix)]
    fn write_failing_bin(dir: &Path, stderr: &str) -> PathBuf {
        let argv = dir.join("argv.txt");
        let script =
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv:?}\necho {stderr:?} >&2\nexit 1\n");
        write_script(dir.join("fake-bin-fail"), &script)
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
