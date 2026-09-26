//! Read-only git access for release tracking.
//!
//! Every command here only reads: refs, logs, trees, diffs between existing
//! commits, and patch equivalence. Kanbr never fetches, checks out, or writes
//! objects, so the board reflects each clone as of its last fetch.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, UNIX_EPOCH};

use crate::firstmate::run_bounded;

pub const GIT_TIMEOUT: Duration = Duration::from_secs(20);

/// A repository, read through the `git` command.
#[derive(Clone, Debug)]
pub struct Git {
    dir: PathBuf,
}

/// One commit from a log read.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<String>,
    pub tree: String,
    /// Committer time, epoch seconds.
    pub time: i64,
    pub committer: String,
    pub subject: String,
    /// The first non-blank body line (a GitHub merge commit's PR title).
    pub body_line: String,
}

/// A branch backing a release lane, and the ref chosen to read it from.
#[derive(Clone, Debug, PartialEq)]
pub struct BranchRef {
    pub refname: String,
    /// The remote the ref tracks, or `None` for a local branch.
    pub remote: Option<String>,
    pub sha: String,
}

/// One path of a tree diff: `None` when the path was deleted.
pub type TreeDiff = HashMap<String, Option<String>>;

impl Git {
    pub fn new(dir: &Path) -> Git {
        Git {
            dir: dir.to_path_buf(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.dir)
            .args([
                "-c",
                "log.showSignature=false",
                "-c",
                "color.ui=false",
                "-c",
                "core.quotepath=off",
            ])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Runs git and returns its exit code (`None` on a signal) and stdout.
    fn exec(&self, args: &[&str]) -> Result<(Option<i32>, String), String> {
        match run_bounded(self.command(args), GIT_TIMEOUT) {
            Ok((Some(status), out, err)) => {
                if status.code().is_none_or(|c| c > 1) {
                    let err = err.trim();
                    return Err(format!(
                        "git {} failed{}",
                        args.first().copied().unwrap_or(""),
                        if err.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", err.lines().last().unwrap_or(err))
                        }
                    ));
                }
                Ok((status.code(), out))
            }
            Ok((None, _, _)) => Err(format!(
                "git {} took longer than {}s",
                args.first().copied().unwrap_or(""),
                GIT_TIMEOUT.as_secs()
            )),
            Err(e) => Err(format!("cannot run git: {e}")),
        }
    }

    /// Runs git and returns stdout, failing on a non-zero exit.
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        match self.exec(args)? {
            (Some(0), out) => Ok(out),
            _ => Err(format!(
                "git {} failed",
                args.first().copied().unwrap_or("")
            )),
        }
    }

    /// Local and remote-tracking branches with the commits they point at.
    pub fn branches(&self) -> Result<Vec<(String, String)>, String> {
        let out = self.run(&[
            "for-each-ref",
            "--format=%(refname)%09%(objectname)",
            "refs/heads",
            "refs/remotes",
        ])?;
        Ok(out
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(r, s)| (r.trim().to_owned(), s.trim().to_owned()))
            .collect())
    }

    /// Up to `max` first-parent commits from `rev`, newest first.
    pub fn first_parent_log(&self, rev: &str, max: usize) -> Result<Vec<Commit>, String> {
        let out = self.run(&[
            "log",
            "--first-parent",
            &format!("--max-count={max}"),
            "--format=%H%x1f%P%x1f%T%x1f%ct%x1f%ce%x1f%s%x1f%b%x1e",
            rev,
            "--",
        ])?;
        Ok(parse_log(&out))
    }

    /// Commits reachable from any of `starts`, walking at most `max`.
    pub fn reachable(&self, starts: &[&str], max: usize) -> Result<HashSet<String>, String> {
        if starts.is_empty() {
            return Ok(HashSet::new());
        }
        let mut args = vec!["rev-list".to_owned(), format!("--max-count={max}")];
        args.extend(starts.iter().map(|s| (*s).to_owned()));
        args.push("--".to_owned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run(&args)?;
        Ok(out.lines().map(|l| l.trim().to_owned()).collect())
    }

    /// Whether `a` is an ancestor of (or equal to) `b`.
    pub fn is_ancestor(&self, a: &str, b: &str) -> bool {
        matches!(
            self.exec(&["merge-base", "--is-ancestor", a, b]),
            Ok((Some(0), _))
        )
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Option<String> {
        match self.exec(&["merge-base", a, b]) {
            Ok((Some(0), out)) => out.lines().next().map(|l| l.trim().to_owned()),
            _ => None,
        }
    }

    /// Paths that differ between two commits, with the blob each has in `b`.
    pub fn tree_diff(&self, a: &str, b: &str) -> Result<TreeDiff, String> {
        let out = self.run(&["diff-tree", "-r", "-z", "--no-renames", a, b])?;
        Ok(parse_raw_diff(&out))
    }

    /// Whether every non-merge commit in `limit..head` is in `upstream`, as
    /// itself or as a patch-equivalent copy; false when the range has none.
    pub fn patches_present(&self, upstream: &str, head: &str, limit: &str) -> bool {
        let own = self.own_commits(head, limit);
        if own.is_empty() {
            return false;
        }
        match self.exec(&["cherry", upstream, head, limit]) {
            Ok((Some(0), out)) => out.lines().all(|l| l.starts_with('-')),
            _ => false,
        }
    }

    /// The non-merge commits in `limit..head`.
    pub fn own_commits(&self, head: &str, limit: &str) -> Vec<String> {
        self.run(&["rev-list", "--no-merges", &format!("{limit}..{head}"), "--"])
            .map(|out| out.lines().map(|l| l.trim().to_owned()).collect())
            .unwrap_or_default()
    }

    /// Patch ids (`git patch-id --stable`) of the commits `git log` selects
    /// with `log_args`, as commit -> patch id.
    pub fn patch_ids(&self, log_args: &[&str]) -> Result<HashMap<String, String>, String> {
        let mut args = vec!["log", "-p", "--no-ext-diff", "--format=commit %H"];
        args.extend_from_slice(log_args);
        let mut log = self.command(&args);
        log.stderr(Stdio::null());
        let mut child = log.spawn().map_err(|e| format!("cannot run git: {e}"))?;
        let Some(out) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("git log produced no output".to_owned());
        };
        let mut pid = self.command(&["patch-id", "--stable"]);
        pid.stdin(Stdio::from(out));
        let res = run_bounded(pid, GIT_TIMEOUT);
        let _ = child.kill();
        let _ = child.wait();
        match res {
            Ok((Some(status), out, _)) if status.success() => Ok(out
                .lines()
                .filter_map(|l| l.split_once(' '))
                .map(|(pid, sha)| (sha.trim().to_owned(), pid.trim().to_owned()))
                .collect()),
            Ok(_) => Err("git patch-id failed".to_owned()),
            Err(e) => Err(format!("cannot run git: {e}")),
        }
    }

    /// Every file path in the tree of `rev`.
    pub fn files(&self, rev: &str) -> Result<Vec<String>, String> {
        let out = self.run(&["ls-tree", "-r", "-z", "--name-only", rev])?;
        Ok(out
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// The contents of `path` at `rev`, if it exists.
    pub fn file_at(&self, rev: &str, path: &str) -> Option<String> {
        match self.exec(&["cat-file", "blob", &format!("{rev}:{path}")]) {
            Ok((Some(0), out)) => Some(out),
            _ => None,
        }
    }

    pub fn remote_url(&self, remote: &str) -> Option<String> {
        match self.exec(&["config", "--get", &format!("remote.{remote}.url")]) {
            Ok((Some(0), out)) => Some(out.trim().to_owned()).filter(|u| !u.is_empty()),
            _ => None,
        }
    }
}

/// When a clone last fetched: the modification time of its `FETCH_HEAD`
/// (in the worktree's git directory or the shared one), read from the file
/// system without running git.
pub fn fetch_time(repo: &Path) -> Option<i64> {
    let dot = repo.join(".git");
    let git_dir = if dot.is_dir() {
        dot
    } else {
        let text = std::fs::read_to_string(&dot).ok()?;
        let p = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if p.is_absolute() { p } else { repo.join(p) }
    };
    let mut dirs = vec![git_dir.clone()];
    if let Ok(common) = std::fs::read_to_string(git_dir.join("commondir")) {
        let p = PathBuf::from(common.trim());
        dirs.push(if p.is_absolute() { p } else { git_dir.join(p) });
    }
    dirs.iter()
        .filter_map(|d| std::fs::metadata(d.join("FETCH_HEAD")).ok())
        .filter_map(|m| m.modified().ok())
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .max()
}

/// Whether `dir` is the top of a git checkout (not merely inside one).
pub fn is_checkout(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Parses `first_parent_log` output.
pub fn parse_log(out: &str) -> Vec<Commit> {
    out.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            if rec.trim().is_empty() {
                return None;
            }
            let f: Vec<&str> = rec.splitn(7, '\x1f').collect();
            if f.len() < 6 {
                return None;
            }
            Some(Commit {
                sha: f[0].trim().to_owned(),
                parents: f[1].split_whitespace().map(str::to_owned).collect(),
                tree: f[2].trim().to_owned(),
                time: f[3].trim().parse().unwrap_or(0),
                committer: f[4].trim().to_owned(),
                subject: f[5].trim().to_owned(),
                body_line: f
                    .get(6)
                    .and_then(|b| b.lines().map(str::trim).find(|l| !l.is_empty()))
                    .unwrap_or("")
                    .to_owned(),
            })
        })
        .collect()
}

/// Parses `diff-tree -r -z` raw output into path -> new blob (`None` = deleted).
pub fn parse_raw_diff(out: &str) -> TreeDiff {
    let mut map = HashMap::new();
    let mut parts = out.split('\0');
    while let Some(meta) = parts.next() {
        let Some(meta) = meta.strip_prefix(':') else {
            continue;
        };
        let Some(path) = parts.next() else { break };
        let f: Vec<&str> = meta.split_whitespace().collect();
        if f.len() < 5 {
            continue;
        }
        let blob = (!f[4].starts_with('D')).then(|| f[3].to_owned());
        map.insert(path.to_owned(), blob);
    }
    map
}

/// The ref to read `branch` from: the `origin` remote-tracking branch, else
/// the local branch, else another remote's branch. Remote-tracking refs are
/// preferred because they show what the forge had at the last fetch,
/// including promotions made directly on GitHub.
pub fn pick_branch(refs: &[(String, String)], branch: &str) -> Option<BranchRef> {
    if branch.is_empty() {
        return None;
    }
    let mut local = None;
    let mut others: Vec<BranchRef> = Vec::new();
    for (refname, sha) in refs {
        if let Some(b) = refname.strip_prefix("refs/heads/") {
            if b == branch {
                local = Some(BranchRef {
                    refname: refname.clone(),
                    remote: None,
                    sha: sha.clone(),
                });
            }
        } else if let Some((remote, b)) = refname
            .strip_prefix("refs/remotes/")
            .and_then(|r| r.split_once('/'))
            && b == branch
        {
            let r = BranchRef {
                refname: refname.clone(),
                remote: Some(remote.to_owned()),
                sha: sha.clone(),
            };
            if remote == "origin" {
                return Some(r);
            }
            others.push(r);
        }
    }
    others.sort_by(|a, b| a.remote.cmp(&b.remote));
    local.or_else(|| others.into_iter().next())
}

/// The web address of a GitHub repository from its remote URL, for PR links.
pub fn github_web(url: &str) -> Option<String> {
    let url = url.trim();
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| {
            url.strip_prefix("https://")
                .and_then(|r| r.split_once('@'))
                .and_then(|(_, r)| r.strip_prefix("github.com/"))
        })?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segs = path.split('/').filter(|s| !s.is_empty());
    let (owner, repo) = (segs.next()?, segs.next()?);
    Some(format!("https://github.com/{owner}/{repo}"))
}

/// The PR a landing commit merged, from GitHub's default merge messages:
/// `Merge pull request #N from owner/branch` (merge commit, title in the body)
/// or `Title (#N)` (squash). Returns the number, PR title, and the merged
/// branch when the message names it.
pub fn pr_of(c: &Commit) -> Option<(u64, String, Option<String>)> {
    if let Some(rest) = c.subject.strip_prefix("Merge pull request #") {
        let (num, rest) = rest.split_once(' ')?;
        let n = num.parse().ok()?;
        let branch = rest
            .strip_prefix("from ")
            .and_then(|r| r.split_once('/'))
            .map(|(_, b)| b.trim().to_owned());
        let title = if c.body_line.is_empty() {
            c.subject.clone()
        } else {
            c.body_line.clone()
        };
        return Some((n, title, branch));
    }
    let s = c.subject.trim_end();
    let open = s.rfind("(#")?;
    let num = s[open + 2..].strip_suffix(')')?;
    let n = num.parse().ok()?;
    let title = s[..open].trim_end().to_owned();
    Some((
        n,
        if title.is_empty() {
            s.to_owned()
        } else {
            title
        },
        None,
    ))
}

/// Whether a changed file is a database migration: any file under a
/// `migrations` or `migrate` directory (Supabase, Rails, Prisma, and
/// hand-rolled `db/migrations` layouts).
pub fn is_migration(path: &str) -> bool {
    let mut dirs: Vec<&str> = path.split('/').collect();
    dirs.pop();
    dirs.iter()
        .any(|d| d.eq_ignore_ascii_case("migrations") || d.eq_ignore_ascii_case("migrate"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(list: &[&str]) -> Vec<(String, String)> {
        list.iter()
            .enumerate()
            .map(|(i, r)| ((*r).to_owned(), format!("sha{i}")))
            .collect()
    }

    #[test]
    fn origin_beats_local_beats_other_remotes() {
        let r = refs(&[
            "refs/heads/dev",
            "refs/remotes/upstream/dev",
            "refs/remotes/origin/dev",
        ]);
        let b = pick_branch(&r, "dev").unwrap();
        assert_eq!(b.refname, "refs/remotes/origin/dev");
        assert_eq!(b.remote.as_deref(), Some("origin"));
        let r = refs(&["refs/remotes/upstream/main", "refs/heads/main"]);
        assert_eq!(pick_branch(&r, "main").unwrap().refname, "refs/heads/main");
        let r = refs(&["refs/remotes/zed/main", "refs/remotes/fork/main"]);
        assert_eq!(
            pick_branch(&r, "main").unwrap().refname,
            "refs/remotes/fork/main"
        );
        assert!(pick_branch(&r, "").is_none());
        assert!(pick_branch(&refs(&["refs/remotes/origin/devtools"]), "dev").is_none());
        let slashed = refs(&["refs/remotes/origin/release/prod"]);
        assert!(pick_branch(&slashed, "release/prod").is_some());
    }

    #[test]
    fn github_web_addresses() {
        for url in [
            "https://github.com/acme/shop.git",
            "https://github.com/acme/shop",
            "git@github.com:acme/shop.git",
            "ssh://git@github.com/acme/shop.git",
            "https://token@github.com/acme/shop.git",
        ] {
            assert_eq!(
                github_web(url).as_deref(),
                Some("https://github.com/acme/shop"),
                "{url}"
            );
        }
        assert_eq!(github_web("https://gitlab.com/acme/shop.git"), None);
        assert_eq!(github_web("/local/path"), None);
    }

    fn commit(subject: &str, body: &str) -> Commit {
        Commit {
            sha: "x".into(),
            parents: vec![],
            tree: "t".into(),
            time: 0,
            committer: String::new(),
            subject: subject.into(),
            body_line: body.into(),
        }
    }

    #[test]
    fn pr_numbers_and_titles_from_merge_messages() {
        assert_eq!(
            pr_of(&commit(
                "Merge pull request #145 from alvinleyble/dev",
                "Promote dev to staging"
            )),
            Some((145, "Promote dev to staging".into(), Some("dev".into())))
        );
        assert_eq!(
            pr_of(&commit("feat(layout): pull to refresh (#144)", "")),
            Some((144, "feat(layout): pull to refresh".into(), None))
        );
        assert_eq!(
            pr_of(&commit("Merge pull request #7 from o/fm/x-y", "")),
            Some((
                7,
                "Merge pull request #7 from o/fm/x-y".into(),
                Some("fm/x-y".into())
            ))
        );
        assert_eq!(pr_of(&commit("fix: plain commit", "")), None);
        assert_eq!(pr_of(&commit("fix: see (#notanumber)", "")), None);
    }

    #[test]
    fn migrations_are_files_under_migration_directories() {
        assert!(is_migration("supabase/migrations/20260926_add.sql"));
        assert!(is_migration("server/db/migrations/049_fee.sql"));
        assert!(is_migration("db/migrate/2026_add.rb"));
        assert!(is_migration("prisma/migrations/2026/migration.sql"));
        assert!(!is_migration("server/db/migrate.js"));
        assert!(!is_migration("docs/migrations.md"));
    }

    #[test]
    fn raw_diff_parses_changes_and_deletions() {
        let out = ":100644 100644 aaa bbb M\0src/a.rs\0:100644 000000 ccc 0000000 D\0old.txt\0:000000 100644 000 ddd A\0new dir/b.sql\0";
        let d = parse_raw_diff(out);
        assert_eq!(d.get("src/a.rs"), Some(&Some("bbb".to_owned())));
        assert_eq!(d.get("old.txt"), Some(&None));
        assert_eq!(d.get("new dir/b.sql"), Some(&Some("ddd".to_owned())));
    }

    #[test]
    fn log_records_parse() {
        let out = "a1\x1fp1 p2\x1ft1\x1f100\x1fme@x\x1fMerge pull request #1 from o/b\x1f\nPR title\n\nmore\n\x1e\nb2\x1f\x1ft2\x1f90\x1fme@x\x1finit\x1f\x1e\n";
        let log = parse_log(out);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].parents, vec!["p1", "p2"]);
        assert_eq!(log[0].body_line, "PR title");
        assert_eq!(log[1].sha, "b2");
        assert!(log[1].parents.is_empty());
        assert_eq!(log[1].time, 90);
    }
}
