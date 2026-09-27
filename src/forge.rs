//! Optional forge reads through the GitHub CLI (`gh`), read-only.
//!
//! Git history is the main source for merged PRs: GitHub's merge messages
//! carry the PR number. `gh` is only asked about a Firstmate card whose PR
//! number is not in any lane's history (a rebase merge, or an edited merge
//! message), to learn the commit the PR landed as, and about the promotion PR
//! behind a fast-forward to the Live branch, to learn where the release
//! starts. Answers for merged PRs never change, so they are cached for good;
//! other answers for a while. Before a merge request, a PR's checks are read
//! so the captain's merge word is sent only when they are green.

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

/// A PR's state, draft flag, and checks, as `gh` prints them (JSON).
pub fn pr_checks(pr_url: &str) -> Result<String, GhError> {
    gh(&[
        "pr",
        "view",
        pr_url,
        "--json",
        "state,isDraft,statusCheckRollup",
    ])
}

/// The merged PR into `branch` of the GitHub repository `web` that landed as
/// commit `tip` (a fast-forward promotion): its number and the branch's
/// commit before it.
pub fn promotion(web: &str, branch: &str, tip: &str) -> Result<Option<(u64, String)>, GhError> {
    let repo = web.strip_prefix("https://github.com/").unwrap_or(web);
    let out = gh(&[
        "pr",
        "list",
        "--repo",
        repo,
        "--base",
        branch,
        "--state",
        "merged",
        "--limit",
        "30",
        "--json",
        "number,mergeCommit,baseRefOid",
        "--jq",
        &format!(".[] | select(.mergeCommit.oid == \"{tip}\") | \"\\(.number) \\(.baseRefOid)\""),
    ])?;
    Ok(parse_promotion(&out))
}

fn parse_promotion(out: &str) -> Option<(u64, String)> {
    let (n, oid) = out.lines().next()?.trim().split_once(' ')?;
    Some((n.parse().ok()?, parse_oid(oid)?))
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

    #[test]
    fn promotions_are_a_number_and_a_base_commit() {
        let oid = "3fe5266a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e";
        assert_eq!(
            parse_promotion(&format!("12 {oid}\n")),
            Some((12, oid.to_owned()))
        );
        assert_eq!(parse_promotion(""), None);
        assert_eq!(parse_promotion("12 null\n"), None);
    }
}
