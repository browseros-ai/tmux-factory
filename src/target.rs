//! Pure target data and validation. No I/O lives here.

use anyhow::{bail, Result};

/// Validate a target or session name as a single path-safe token.
///
/// Rejects empty/whitespace-only values, `.` and `..`, and any value
/// containing a slash, backslash, tab, CR, LF, or space. This is what keeps a
/// name from escaping its directory.
pub fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\t', '\r', '\n', ' '])
    {
        bail!("invalid name \"{name}\": use a single path-safe token");
    }
    Ok(())
}

/// Validate a target role; one of `mediator` or `agent`.
pub fn validate_role(role: &str) -> Result<()> {
    match role {
        "mediator" | "agent" => Ok(()),
        _ => bail!("invalid role \"{role}\": want mediator or agent"),
    }
}

/// Validate a target kind; one of `claude`, `codex`, or `generic`.
pub fn validate_kind(kind: &str) -> Result<()> {
    match kind {
        "claude" | "codex" | "generic" => Ok(()),
        _ => bail!("invalid kind \"{kind}\": want claude, codex, or generic"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_token() {
        assert!(validate_name("agent1").is_ok());
        assert!(validate_name("mediator").is_ok());
    }

    #[test]
    fn rejects_empty_or_whitespace_name() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("\t").is_err());
    }

    #[test]
    fn rejects_dot_and_dotdot() {
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
    }

    #[test]
    fn rejects_path_separators_and_inner_whitespace() {
        for bad in ["a/b", "a\\b", "a b", "a\tb", "a\rb", "a\nb"] {
            assert!(validate_name(bad).is_err(), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn name_error_mentions_path_safe_token() {
        let err = validate_name("a/b").unwrap_err().to_string();
        assert!(err.contains("path-safe token"), "got: {err}");
        assert!(err.contains("a/b"), "got: {err}");
    }

    #[test]
    fn role_accepts_mediator_and_agent() {
        assert!(validate_role("mediator").is_ok());
        assert!(validate_role("agent").is_ok());
    }

    #[test]
    fn role_rejects_unknown() {
        let err = validate_role("boss").unwrap_err().to_string();
        assert!(err.contains("invalid role"), "got: {err}");
    }

    #[test]
    fn kind_accepts_known() {
        for k in ["claude", "codex", "generic"] {
            assert!(validate_kind(k).is_ok(), "expected {k} to be accepted");
        }
    }

    #[test]
    fn kind_rejects_unknown() {
        let err = validate_kind("gpt").unwrap_err().to_string();
        assert!(err.contains("invalid kind"), "got: {err}");
    }
}
