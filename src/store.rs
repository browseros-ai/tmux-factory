//! `~/.tfmux` storage layout and session resolution.
//!
//! Layout:
//! ```text
//! <base>/<YYYY-MM-DD>/<session>/session.json
//! <base>/<YYYY-MM-DD>/<session>/targets/<name>.json
//! ```
//! There is deliberately no global `<base>/current` pointer; session identity
//! travels with the pane (see design §4 and §5).

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::target::{validate_name, Target};

/// A tfmux session: a dated directory holding bound targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    /// RFC3339 UTC timestamp of session creation.
    pub created_at: String,
}

/// Filesystem-backed session/target store rooted at `base_dir`.
pub struct Store {
    base_dir: PathBuf,
}

impl Store {
    /// Create a store rooted at `base_dir` (`$TFMUX_HOME` or `~/.tfmux`).
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Find an existing session directory by name, newest date first.
    /// Returns `None` if no session by that name exists.
    pub fn find_session_dir(&self, name: &str) -> Result<Option<PathBuf>> {
        let entries = match fs::read_dir(&self.base_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(anyhow!("reading {}: {}", self.base_dir.display(), e)),
        };
        let mut dates: Vec<String> = Vec::new();
        for entry in entries {
            let entry = entry.context("reading base dir entry")?;
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(s) = entry.file_name().to_str() {
                    dates.push(s.to_string());
                }
            }
        }
        dates.sort();
        for date in dates.into_iter().rev() {
            let candidate = self.base_dir.join(date).join(name);
            if candidate.is_dir() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// Create `<base>/<today>/<name>/` with a `session.json`, returning the dir.
    /// The date segment is the local calendar date of `now`.
    pub fn create_session(&self, name: &str, now: DateTime<Utc>) -> Result<PathBuf> {
        let date = now.with_timezone(&Local).format("%Y-%m-%d").to_string();
        let dir = self.base_dir.join(date).join(name);
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating session dir {}", dir.display()))?;
        let session = Session {
            name: name.to_string(),
            created_at: rfc3339(now),
        };
        write_json_atomic(&dir.join("session.json"), &session)?;
        Ok(dir)
    }

    /// Persist a target into `<session_dir>/targets/<name>.json` atomically.
    pub fn save_target(&self, session_dir: &Path, target: &Target) -> Result<()> {
        let targets_dir = session_dir.join("targets");
        fs::create_dir_all(&targets_dir)
            .with_context(|| format!("creating targets dir {}", targets_dir.display()))?;
        write_json_atomic(&targets_dir.join(format!("{}.json", target.name)), target)
    }

    /// Load `<session_dir>/targets/<name>.json`, returning `None` when absent.
    ///
    /// # Errors
    /// Returns an error when `name` is invalid, the target file cannot be read,
    /// or the JSON cannot be decoded as a `Target`.
    pub fn load_target(&self, session_dir: &Path, name: &str) -> Result<Option<Target>> {
        validate_name(name)?;
        let path = session_dir.join("targets").join(format!("{name}.json"));
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(anyhow!("reading {}: {}", path.display(), e)),
        };
        let target = serde_json::from_str(&raw)
            .with_context(|| format!("decoding target {}", path.display()))?;
        Ok(Some(target))
    }

    /// Delete `<session_dir>/targets/<name>.json`.
    ///
    /// Returns `true` when a target file was removed and `false` when it was
    /// already absent.
    ///
    /// # Errors
    /// Returns an error when `name` is invalid or the target file cannot be
    /// deleted.
    pub fn delete_target(&self, session_dir: &Path, name: &str) -> Result<bool> {
        validate_name(name)?;
        let path = session_dir.join("targets").join(format!("{name}.json"));
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(anyhow!("deleting {}: {}", path.display(), e)),
        }
    }

    /// Load all JSON targets from `<session_dir>/targets/`, sorted by name.
    ///
    /// Non-JSON files are ignored. Malformed JSON target files are surfaced as
    /// errors because they represent corrupted tfmux state.
    pub fn list_targets(&self, session_dir: &Path) -> Result<Vec<Target>> {
        let targets_dir = session_dir.join("targets");
        let entries = match fs::read_dir(&targets_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(anyhow!("reading {}: {}", targets_dir.display(), e)),
        };

        let mut targets = Vec::new();
        for entry in entries {
            let entry = entry.context("reading targets dir entry")?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("reading target {}", path.display()))?;
            let target: Target = serde_json::from_str(&raw)
                .with_context(|| format!("decoding target {}", path.display()))?;
            targets.push(target);
        }
        targets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(targets)
    }
}

/// Resolve the session name from the precedence chain
/// `--session` > `TFMUX_SESSION` > `.llm/tfmux-session`.
/// Blank sources are skipped; the chosen name is validated as a path-safe token.
pub fn resolve_session_name(
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
    bail!("no session name; pass --session NAME, set TFMUX_SESSION, or add .llm/tfmux-session")
}

/// Read the local `.llm/tfmux-session` marker (first line) from `cwd`.
/// Returns `None` if the file is absent or its first line is blank.
pub fn read_session_marker(cwd: &Path) -> Result<Option<String>> {
    let path = cwd.join(".llm").join("tfmux-session");
    match fs::read_to_string(&path) {
        Ok(content) => {
            let name = content.lines().next().unwrap_or("").trim().to_string();
            Ok(if name.is_empty() { None } else { Some(name) })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow!("reading {}: {}", path.display(), e)),
    }
}

/// Format a UTC instant as RFC3339 with seconds precision and a `Z` suffix.
pub fn rfc3339(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Serialize `value` as pretty JSON with a trailing newline and write it via a
/// same-directory temp file + atomic rename.
fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    let mut json = serde_json::to_string_pretty(value).context("encoding JSON")?;
    json.push('\n');

    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("creating temp file in {}", dir.display()))?;
    tmp.write_all(json.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    tmp.flush().context("flushing temp file")?;
    tmp.persist(path)
        .map_err(|e| anyhow!("persisting {}: {}", path.display(), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::fs;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap()
    }

    fn sample_target() -> Target {
        Target {
            name: "agent1".into(),
            role: "agent".into(),
            kind: "claude".into(),
            input: "sess:1.0".into(),
            pane_id: "%5".into(),
            session: "sess".into(),
            window: "1".into(),
            pane_index: "0".into(),
            socket: String::new(),
            bound_at: "2026-06-28T12:00:00Z".into(),
        }
    }

    // ---- resolve_session_name precedence ----

    #[test]
    fn session_flag_wins_over_env_and_marker() {
        let got =
            resolve_session_name(Some("flagsess"), Some("envsess"), Some("markersess")).unwrap();
        assert_eq!(got, "flagsess");
    }

    #[test]
    fn session_env_wins_over_marker_when_no_flag() {
        let got = resolve_session_name(None, Some("envsess"), Some("markersess")).unwrap();
        assert_eq!(got, "envsess");
    }

    #[test]
    fn session_marker_used_when_no_flag_or_env() {
        let got = resolve_session_name(None, None, Some("markersess")).unwrap();
        assert_eq!(got, "markersess");
    }

    #[test]
    fn session_blank_sources_are_skipped() {
        let got = resolve_session_name(Some("  "), Some(""), Some("markersess")).unwrap();
        assert_eq!(got, "markersess");
    }

    #[test]
    fn session_none_errors_with_all_three_sources() {
        let err = resolve_session_name(None, None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--session"), "got: {err}");
        assert!(err.contains("TFMUX_SESSION"), "got: {err}");
        assert!(err.contains(".llm/tfmux-session"), "got: {err}");
    }

    #[test]
    fn session_invalid_name_rejected() {
        let err = resolve_session_name(Some("a/b"), None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("path-safe token"), "got: {err}");
    }

    // ---- read_session_marker ----

    #[test]
    fn marker_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_session_marker(dir.path()).unwrap(), None);
    }

    #[test]
    fn marker_reads_first_line_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".llm")).unwrap();
        fs::write(dir.path().join(".llm/tfmux-session"), "demo\nignored\n").unwrap();
        assert_eq!(
            read_session_marker(dir.path()).unwrap(),
            Some("demo".to_string())
        );
    }

    #[test]
    fn marker_empty_is_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".llm")).unwrap();
        fs::write(dir.path().join(".llm/tfmux-session"), "\n").unwrap();
        assert_eq!(read_session_marker(dir.path()).unwrap(), None);
    }

    // ---- create_session ----

    #[test]
    fn create_session_writes_dated_session_json() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();

        let session_json = dir.join("session.json");
        assert!(session_json.is_file());
        let parsed: Session =
            serde_json::from_str(&fs::read_to_string(&session_json).unwrap()).unwrap();
        assert_eq!(parsed.name, "demo");
        assert_eq!(parsed.created_at, "2026-06-28T12:00:00Z");

        // dir is <base>/<YYYY-MM-DD>/demo
        let date_dir = dir.parent().unwrap();
        let date_name = date_dir.file_name().unwrap().to_str().unwrap();
        let b = date_name.as_bytes();
        assert!(
            date_name.len() == 10 && b[4] == b'-' && b[7] == b'-',
            "date dir not YYYY-MM-DD: {date_name}"
        );
        assert_eq!(date_dir.parent().unwrap(), base.path());
    }

    #[test]
    fn create_session_does_not_create_global_current() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        store.create_session("demo", fixed_now()).unwrap();
        assert!(
            !base.path().join("current").exists(),
            "no global current pointer should be created"
        );
    }

    // ---- find_session_dir ----

    #[test]
    fn find_session_absent_is_none() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        assert_eq!(store.find_session_dir("demo").unwrap(), None);
    }

    #[test]
    fn find_session_returns_created_dir() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let created = store.create_session("demo", fixed_now()).unwrap();
        assert_eq!(store.find_session_dir("demo").unwrap(), Some(created));
    }

    #[test]
    fn find_session_prefers_newest_date() {
        let base = tempfile::tempdir().unwrap();
        fs::create_dir_all(base.path().join("2026-06-01/demo")).unwrap();
        fs::create_dir_all(base.path().join("2026-06-28/demo")).unwrap();
        let store = Store::new(base.path().to_path_buf());
        let got = store.find_session_dir("demo").unwrap().unwrap();
        assert_eq!(got, base.path().join("2026-06-28/demo"));
    }

    // ---- save_target ----

    #[test]
    fn save_target_round_trips_via_file() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();
        let t = sample_target();
        store.save_target(&dir, &t).unwrap();

        let path = dir.join("targets/agent1.json");
        assert!(path.is_file());
        let raw = fs::read_to_string(&path).unwrap();
        assert!(
            raw.ends_with("}\n"),
            "expected trailing newline, got: {raw:?}"
        );
        let parsed: Target = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, t);
    }

    #[test]
    fn save_target_json_field_order_matches_design() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();
        store.save_target(&dir, &sample_target()).unwrap();

        let raw = fs::read_to_string(dir.join("targets/agent1.json")).unwrap();
        let order = [
            "name",
            "role",
            "kind",
            "input",
            "pane_id",
            "session",
            "window",
            "pane_index",
            "socket",
            "bound_at",
        ];
        let mut last = 0usize;
        for key in order {
            let pos = raw
                .find(&format!("\"{key}\""))
                .unwrap_or_else(|| panic!("missing key {key} in {raw}"));
            assert!(pos >= last, "key {key} out of declared order");
            last = pos;
        }
    }

    // ---- load_target ----

    #[test]
    fn load_target_reads_bound_target_json() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();
        let target = sample_target();
        store.save_target(&dir, &target).unwrap();

        let loaded = store.load_target(&dir, "agent1").unwrap();
        assert_eq!(loaded, Some(target));
    }

    #[test]
    fn load_legacy_target_without_socket_defaults_to_empty() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();
        fs::create_dir_all(dir.join("targets")).unwrap();
        fs::write(
            dir.join("targets/agent1.json"),
            r#"{
  "name": "agent1",
  "role": "agent",
  "kind": "claude",
  "input": "sess:1.0",
  "pane_id": "%5",
  "session": "sess",
  "window": "1",
  "pane_index": "0",
  "bound_at": "2026-06-28T12:00:00Z"
}
"#,
        )
        .unwrap();

        let loaded = store.load_target(&dir, "agent1").unwrap().unwrap();

        assert_eq!(loaded.socket, "");
    }

    #[test]
    fn load_target_missing_is_none() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();

        assert_eq!(store.load_target(&dir, "agent1").unwrap(), None);
        assert!(!base.path().join("current").exists());
    }

    // ---- delete_target ----

    #[test]
    fn delete_target_removes_existing_target_file() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();
        store.save_target(&dir, &sample_target()).unwrap();

        let removed = store.delete_target(&dir, "agent1").unwrap();

        assert!(removed);
        assert!(!dir.join("targets/agent1.json").exists());
        assert!(dir.join("session.json").exists());
        assert!(!base.path().join("current").exists());
    }

    #[test]
    fn delete_target_missing_returns_false() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();

        let removed = store.delete_target(&dir, "agent1").unwrap();

        assert!(!removed);
        assert!(dir.join("session.json").exists());
        assert!(!base.path().join("current").exists());
    }

    #[test]
    fn delete_target_is_constrained_to_targets_file() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();
        fs::write(dir.join("agent1.json"), "outside target dir").unwrap();
        store.save_target(&dir, &sample_target()).unwrap();

        let removed = store.delete_target(&dir, "agent1").unwrap();

        assert!(removed);
        assert!(!dir.join("targets/agent1.json").exists());
        assert_eq!(
            fs::read_to_string(dir.join("agent1.json")).unwrap(),
            "outside target dir"
        );
    }

    // ---- list_targets ----

    #[test]
    fn list_targets_missing_targets_dir_is_empty() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();

        assert_eq!(store.list_targets(&dir).unwrap(), Vec::<Target>::new());
    }

    #[test]
    fn list_targets_reads_json_files_sorted_by_name() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();

        let mut beta = sample_target();
        beta.name = "beta".to_string();
        let mut alpha = sample_target();
        alpha.name = "alpha".to_string();
        store.save_target(&dir, &beta).unwrap();
        store.save_target(&dir, &alpha).unwrap();

        let listed = store.list_targets(&dir).unwrap();
        assert_eq!(listed, vec![alpha, beta]);
    }

    #[test]
    fn list_targets_ignores_non_json_junk() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();
        let target = sample_target();
        store.save_target(&dir, &target).unwrap();
        fs::write(dir.join("targets/README.txt"), "not a target").unwrap();

        assert_eq!(store.list_targets(&dir).unwrap(), vec![target]);
    }

    #[test]
    fn list_targets_surfaces_malformed_json_files() {
        let base = tempfile::tempdir().unwrap();
        let store = Store::new(base.path().to_path_buf());
        let dir = store.create_session("demo", fixed_now()).unwrap();
        fs::create_dir_all(dir.join("targets")).unwrap();
        fs::write(dir.join("targets/broken.json"), "{not json").unwrap();

        let err = store.list_targets(&dir).unwrap_err().to_string();
        assert!(err.contains("decoding target"), "got: {err}");
        assert!(err.contains("broken.json"), "got: {err}");
    }
}
