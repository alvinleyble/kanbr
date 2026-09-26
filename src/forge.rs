//! Optional forge reads through the GitHub CLI (`gh`), read-only.
//!
//! Git history is the main source for merged PRs: GitHub's merge messages
//! carry the PR number. `gh` is only asked about a Firstmate card whose PR
//! number is not in any lane's history (a rebase merge, or an edited merge
//! message), to learn the commit the PR landed as. Answers for merged PRs
//! never change, so they are cached for good; other answers for a while.

use std::process::{Command, Stdio};
use std::time::Duration;

use crate::firstmate::run_bounded;

pub const GH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, PartialEq, Eq)]
pub enum GhError {
    /// `gh` is not installed.
    Missing,
    /// `gh` ran but failed (not signed in, no network, no such PR).
    Failed(String),
}

fn gh(args: &[&str]) -> Result<String, GhError> {
    let mut cmd = Command::new("gh");
    cmd.args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match run_bounded(cmd, GH_TIMEOUT) {
        Ok((Some(status), out, _)) if status.success() => Ok(out),
        Ok((Some(_), _, err)) => Err(GhError::Failed(
            err.lines()
                .rfind(|l| !l.trim().is_empty())
                .unwrap_or("failed")
                .trim()
                .to_owned(),
        )),
        Ok((None, _, _)) => Err(GhError::Failed(format!(
            "took longer than {}s",
            GH_TIMEOUT.as_secs()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(GhError::Missing),
        Err(e) => Err(GhError::Failed(e.to_string())),
    }
}

/// The commit a merged PR landed as, or `None` when it is not merged.
pub fn merged_commit(pr_url: &str) -> Result<Option<String>, GhError> {
    let out = gh(&[
        "pr",
        "view",
        pr_url,
        "--json",
        "state,mergeCommit",
        "--jq",
        "if .state == \"MERGED\" then .mergeCommit.oid else \"\" end",
    ])?;
    Ok(parse_oid(&out))
}

fn parse_oid(out: &str) -> Option<String> {
    let oid = out.trim();
    (oid.len() >= 40 && oid.chars().all(|c| c.is_ascii_hexdigit())).then(|| oid.to_owned())
}

/// Whether `gh` is installed and signed in, for `kanbr doctor`.
pub fn status() -> Result<String, GhError> {
    gh(&["--version"])?;
    gh(&["auth", "status"]).map(|_| "installed and signed in".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oids_are_full_hex_hashes() {
        let oid = "3fe5266a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e";
        assert_eq!(parse_oid(&format!("{oid}\n")).as_deref(), Some(oid));
        assert_eq!(parse_oid("\n"), None);
        assert_eq!(parse_oid("null"), None);
    }
}
