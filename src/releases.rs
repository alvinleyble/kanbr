//! Release tracking from git: how far each merged change has travelled
//! through Dev, Staging, and Live, the latest Live release, and the next
//! release waiting in Staging (design decisions 6, 7, 8, 19).
//!
//! Each lane is read from the branch configured to back it. Every commit on a
//! lane's first-parent history is one landing, of one of three kinds:
//!
//! - a **change**: a merged PR (merge commit or squash) or a direct commit
//!   that landed on this lane first;
//! - a **promotion**: content brought up from a lower lane, by a merge whose
//!   merged side was on that lane, a fast-forward, a squash whose tree (or
//!   file-by-file result, when the lanes have diverged) matches a point on
//!   that lane, or a rebased copy of that lane's patches;
//! - a **back-merge**: a higher lane merged back down.
//!
//! A change has reached a lane when its landing commit is an ancestor of the
//! lane (merge and fast-forward promotions), an ancestor of the source point
//! a squash promotion carried, or when a rebased copy of its patches landed
//! there (patch-id equivalence). It is placed in the furthest lane reached.
//!
//! The latest Live release is the newest landing on the Live branch (a
//! promotion, a merged PR, or a direct commit; a rebase that landed several
//! commits at once counts as one). Its changes are those present on Live now
//! but not just before it.
//!
//! Everything here is read-only git; results that cannot change for the same
//! commits are memoised across refreshes in [`Memo`].

use std::collections::{HashMap, HashSet};

use crate::firstmate::Lanes;
use crate::git::{
    BranchRef, Commit, Git, fetch_time, github_web, is_migration, pick_branch, pr_of,
};
use crate::model::Column;

/// First-parent landings read per lane.
pub const SCAN: usize = 300;
/// Commits walked when collecting what a lane contains.
const REACH: usize = 20_000;
/// Allowed clock difference between machines that made commits, in seconds.
const SKEW: i64 = 120;
/// Source-lane states tried, newest first, when matching a squash promotion.
const SQUASH_CANDIDATES: usize = 3;
/// Lower-lane commits whose patch ids are compared with rebased copies.
const COPY_WINDOW: &str = "--max-count=1000";

/// A release lane and the ref it is read from.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneRef {
    pub column: Column,
    pub branch: String,
    /// `refs/remotes/origin/dev` or `refs/heads/dev`.
    pub refname: String,
    pub tip: String,
}

/// One change: a merged PR or a direct commit.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// The commit that landed it on its first lane.
    pub commit: String,
    /// The lane it landed on first.
    pub home: Column,
    /// The furthest lane it has reached.
    pub column: Column,
    /// Part of the latest Live release.
    pub in_release: bool,
    pub pr: Option<u64>,
    /// The PR title, or the commit subject for a direct commit.
    pub title: String,
    /// When it landed (committer time).
    pub time: i64,
}

/// The latest Live release.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub commit: String,
    pub time: i64,
    /// The promotion (or merged) PR that made the release, when it was one.
    pub pr: Option<u64>,
    pub title: String,
    /// The app version on the Live branch, where the project has one.
    pub version: Option<String>,
}

/// What a scanned landing commit or PR number turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Index into [`ProjectGit::changes`].
    Change(usize),
    Promotion,
    BackMerge,
}

/// A project's release state, derived from its repository.
#[derive(Clone, Debug, Default)]
pub struct ProjectGit {
    pub lanes: Lanes,
    /// The lanes the project uses, lowest first.
    pub refs: Vec<LaneRef>,
    /// Every change found in the scanned history.
    pub changes: Vec<Change>,
    pub by_pr: HashMap<u64, Role>,
    pub by_commit: HashMap<String, Role>,
    pub release: Option<Release>,
    /// Database migrations on Staging that Live does not have yet.
    pub migrations: Vec<String>,
    /// `https://github.com/owner/repo`, for PR links.
    pub web: Option<String>,
    /// When the clone last fetched (epoch seconds).
    pub fetched: Option<i64>,
}

impl ProjectGit {
    /// The last lane the project uses.
    pub fn top(&self) -> Option<Column> {
        self.refs.last().map(|r| r.column)
    }

    pub fn pr_url(&self, n: u64) -> Option<String> {
        self.web.as_ref().map(|w| format!("{w}/pull/{n}"))
    }

    /// Whether a change is on the board: still on its way to the last lane,
    /// or part of the latest Live release. Changes that reached a last lane
    /// other than Live are kept while they are recent (`recent`).
    pub fn visible(&self, change: &Change, recent: bool) -> bool {
        match self.top() {
            Some(top) if change.column == top => {
                change.in_release || (top != Column::Live && recent)
            }
            _ => true,
        }
    }
}

/// Results that stay true for the same commits, kept across refreshes.
#[derive(Default)]
pub struct Memo {
    ancestors: HashMap<(String, String), bool>,
    squashes: HashMap<(String, String), bool>,
    copies: HashMap<(String, String), bool>,
    versions: HashMap<String, Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Change,
    /// From lane `from`; everything up to `source` came with it, or only this
    /// commit's patch when `source` is `None` (a rebased or cherry-picked copy).
    Promotion {
        from: usize,
        source: Option<String>,
    },
    BackMerge,
}

struct Lane {
    column: Column,
    branch: String,
    r: BranchRef,
    /// First-parent history, newest first.
    log: Vec<Commit>,
    index: HashMap<String, usize>,
    /// Tree -> newest position in `log` with that tree.
    trees: HashMap<String, usize>,
    /// Commits reachable from the tip.
    reach: HashSet<String>,
}

/// What a lane state contains: commits by ancestry (including squash
/// promotions' sources) and commits whose patches were copied in.
struct Evidence {
    reach: HashSet<String>,
    copies: HashSet<String>,
}

/// Analyses a repository for the `[dev, staging, live]` branch names.
#[cfg(test)]
pub fn analyze(git: &Git, branches: [&str; 3], memo: &mut Memo) -> Result<ProjectGit, String> {
    analyze_refs(git, &git.branches()?, branches, memo)
}

/// The tips of the branches backing `[dev, staging, live]`, as read from
/// `refs` (a cache key: the analysis only changes when one moves).
pub fn lane_tips(refs: &[(String, String)], branches: [&str; 3]) -> Vec<Option<String>> {
    branches
        .iter()
        .map(|b| pick_branch(refs, b).map(|r| r.sha))
        .collect()
}

/// [`analyze`] with the repository's branches already read.
pub fn analyze_refs(
    git: &Git,
    refs: &[(String, String)],
    branches: [&str; 3],
    memo: &mut Memo,
) -> Result<ProjectGit, String> {
    let columns = [Column::Dev, Column::Staging, Column::Live];
    let mut lanes: Vec<Lane> = Vec::new();
    for (i, branch) in branches.iter().enumerate() {
        let Some(r) = pick_branch(refs, branch) else {
            continue;
        };
        if lanes.iter().any(|l| l.branch == *branch) {
            continue;
        }
        let log = git.first_parent_log(&r.sha, SCAN)?;
        let reach = git.reachable(&[&r.sha], REACH)?;
        let index = log
            .iter()
            .enumerate()
            .map(|(i, c)| (c.sha.clone(), i))
            .collect();
        let mut trees = HashMap::new();
        for (i, c) in log.iter().enumerate() {
            trees.entry(c.tree.clone()).or_insert(i);
        }
        lanes.push(Lane {
            column: columns[i],
            branch: (*branch).to_owned(),
            r,
            log,
            index,
            trees,
            reach,
        });
    }
    let uses = |c: Column| lanes.iter().any(|l| l.column == c);
    let mut pg = ProjectGit {
        lanes: Lanes {
            dev: uses(Column::Dev),
            staging: uses(Column::Staging),
            live: uses(Column::Live),
        },
        refs: lanes
            .iter()
            .map(|l| LaneRef {
                column: l.column,
                branch: l.branch.clone(),
                refname: l.r.refname.clone(),
                tip: l.r.sha.clone(),
            })
            .collect(),
        fetched: fetch_time(git.dir()),
        ..ProjectGit::default()
    };
    let Some(top) = lanes.last() else {
        return Ok(pg);
    };
    pg.web = github_web(
        &git.remote_url(top.r.remote.as_deref().unwrap_or("origin"))
            .or_else(|| git.remote_url("origin"))
            .unwrap_or_default(),
    );
    if lanes.iter().any(|l| l.log.is_empty()) {
        return Ok(pg);
    }
    let mut a = Analysis {
        git,
        memo,
        lanes,
        kinds: HashMap::new(),
        pids: None,
    };
    a.run(&mut pg)?;
    Ok(pg)
}

struct Analysis<'a> {
    git: &'a Git,
    memo: &'a mut Memo,
    lanes: Vec<Lane>,
    kinds: HashMap<(usize, usize), Kind>,
    /// Patch id of each rebased copy and of the lower-lane commits, loaded
    /// only when a copy exists.
    pids: Option<HashMap<String, Vec<String>>>,
}

/// How many landings at the head of `log` arrived together, and the position
/// of the landing before them. A rebase merge lands several commits at once
/// with one committer and time.
fn release_group(log: &[Commit]) -> (usize, Option<usize>) {
    let together = |newer: &Commit, older: &Commit| {
        newer.parents.len() == 1
            && older.parents.len() == 1
            && !newer.committer.is_empty()
            && newer.committer == older.committer
            && (newer.time - older.time).abs() <= 1
    };
    let mut g = 1;
    while g < log.len() && together(&log[g - 1], &log[g]) {
        g += 1;
    }
    (g, (g < log.len()).then_some(g))
}

impl Analysis<'_> {
    fn run(&mut self, pg: &mut ProjectGit) -> Result<(), String> {
        let n = self.lanes.len();
        let top = n - 1;
        let live = self.lanes[top].column == Column::Live;
        let (_, prev) = release_group(&self.lanes[top].log);
        // What each lane holds now; the lowest lane is never a target.
        let mut tips = vec![Evidence {
            reach: HashSet::new(),
            copies: HashSet::new(),
        }];
        for j in 1..n {
            tips.push(self.evidence(j, 0)?);
        }
        let before = match prev {
            Some(p) if live => Some(self.evidence(top, p)?),
            _ => None,
        };
        for j in 0..n {
            for i in 0..self.lanes[j].log.len() {
                let c = self.lanes[j].log[i].clone();
                let pr = pr_of(&c);
                let role = match self.kind(j, i) {
                    Kind::Change => {
                        let mut reached = j;
                        for (k, tip) in tips.iter().enumerate().skip(j + 1) {
                            if self.present(&c, tip)? {
                                reached = k;
                            }
                        }
                        let in_release = live
                            && reached == top
                            && match &before {
                                Some(b) => !self.present(&c, b)?,
                                None => true,
                            };
                        let idx = pg.changes.len();
                        pg.changes.push(Change {
                            commit: c.sha.clone(),
                            home: self.lanes[j].column,
                            column: self.lanes[reached].column,
                            in_release,
                            pr: pr.as_ref().map(|p| p.0),
                            title: pr
                                .as_ref()
                                .map_or_else(|| c.subject.clone(), |p| p.1.clone()),
                            time: c.time,
                        });
                        pg.by_commit.insert(c.sha.clone(), Role::Change(idx));
                        if let Some((num, _, _)) = pr {
                            pg.by_pr.insert(num, Role::Change(idx));
                        }
                        continue;
                    }
                    Kind::Promotion { .. } => Role::Promotion,
                    Kind::BackMerge => Role::BackMerge,
                };
                pg.by_commit.entry(c.sha.clone()).or_insert(role);
                if let Some((num, _, _)) = pr {
                    pg.by_pr.entry(num).or_insert(role);
                }
            }
        }

        if live {
            let l = self.lanes[top].log[0].clone();
            let version = match self.memo.versions.get(&l.sha) {
                Some(v) => v.clone(),
                None => {
                    let v = detect_version(self.git, &l.sha);
                    self.memo.versions.insert(l.sha.clone(), v.clone());
                    v
                }
            };
            let pr = pr_of(&l);
            pg.release = Some(Release {
                commit: l.sha.clone(),
                time: l.time,
                pr: pr.as_ref().map(|p| p.0),
                title: pr.map_or(l.subject.clone(), |p| p.1),
                version,
            });
        }

        let lane = |c: Column| self.lanes.iter().find(|l| l.column == c);
        if let (Some(staging), Some(live)) = (lane(Column::Staging), lane(Column::Live)) {
            let diff = self.git.tree_diff(&live.r.sha, &staging.r.sha)?;
            let mut m: Vec<String> = diff
                .into_iter()
                .filter(|(p, blob)| blob.is_some() && is_migration(p))
                .map(|(p, _)| p)
                .collect();
            m.sort();
            pg.migrations = m;
        }
        Ok(())
    }

    fn kind(&mut self, j: usize, i: usize) -> Kind {
        if let Some(k) = self.kinds.get(&(j, i)) {
            return k.clone();
        }
        let k = self.classify(j, i);
        self.kinds.insert((j, i), k.clone());
        k
    }

    fn classify(&mut self, j: usize, i: usize) -> Kind {
        let c = self.lanes[j].log[i].clone();
        for k in (0..j).rev() {
            if self.lanes[k].index.contains_key(&c.sha) {
                return Kind::Promotion {
                    from: k,
                    source: Some(c.sha),
                };
            }
        }
        if let Some(merged) = c.parents.get(1) {
            for k in (0..j).rev() {
                if self.was_on(k, merged, c.time + SKEW) {
                    return Kind::Promotion {
                        from: k,
                        source: Some(merged.clone()),
                    };
                }
            }
            for k in j + 1..self.lanes.len() {
                if self.was_on(k, merged, c.time - SKEW) {
                    return Kind::BackMerge;
                }
            }
        }
        // A squash, or a merged promotion branch: content that matches a lower
        // lane (the merged side may itself be staging plus a merge of dev,
        // made to resolve conflicts).
        for k in (0..j).rev() {
            if let Some(&d) = self.lanes[k].trees.get(&c.tree) {
                let d = self.lanes[k].log[d].clone();
                // A lower-lane commit that already contains this one is a
                // back-merge of it, not its source.
                if d.time <= c.time + SKEW && !self.ancestor(&c.sha, &d.sha) {
                    return Kind::Promotion {
                        from: k,
                        source: Some(d.sha),
                    };
                }
            }
            if let Some(s) = self.squash_source(k, &c) {
                return Kind::Promotion {
                    from: k,
                    source: Some(s),
                };
            }
        }
        for k in (0..j).rev() {
            if self.copied_from(k, &c) {
                return Kind::Promotion {
                    from: k,
                    source: None,
                };
            }
        }
        Kind::Change
    }

    /// The newest landing on lane `k` by time `t` (allowing for clock skew).
    fn at(&self, k: usize, t: i64) -> Option<usize> {
        self.lanes[k].log.iter().position(|c| c.time <= t + SKEW)
    }

    fn ancestor(&mut self, a: &str, b: &str) -> bool {
        let key = (a.to_owned(), b.to_owned());
        if let Some(v) = self.memo.ancestors.get(&key) {
            return *v;
        }
        let v = self.git.is_ancestor(a, b);
        self.memo.ancestors.insert(key, v);
        v
    }

    /// Whether commit `p` was on lane `k` at time `t` (no skew allowance:
    /// callers shift `t` toward the reading they need).
    fn was_on(&mut self, k: usize, p: &str, t: i64) -> bool {
        if self.lanes[k].index.contains_key(p) {
            return true;
        }
        if !self.lanes[k].reach.contains(p) {
            return false;
        }
        let Some(pos) = self.lanes[k].log.iter().position(|c| c.time <= t) else {
            return false;
        };
        let state = self.lanes[k].log[pos].sha.clone();
        self.ancestor(p, &state)
    }

    /// The lane-`k` state that squash commit `c` brought over, when its
    /// changes are exactly that state's changes (lanes may have diverged).
    fn squash_source(&mut self, k: usize, c: &Commit) -> Option<String> {
        let parent = c.parents.first()?.clone();
        let start = self.at(k, c.time)?;
        let end = (start + SQUASH_CANDIDATES).min(self.lanes[k].log.len());
        for pos in start..end {
            let s = self.lanes[k].log[pos].sha.clone();
            let key = (c.sha.clone(), s.clone());
            let hit = match self.memo.squashes.get(&key) {
                Some(h) => *h,
                None => {
                    let h = self.squash_of(c, &parent, &s);
                    self.memo.squashes.insert(key, h);
                    h
                }
            };
            if hit {
                return Some(s);
            }
        }
        None
    }

    /// Whether `c` (on top of `parent`) applies exactly the changes `source`
    /// made since the two diverged: every path it changes, the source changed,
    /// and it brings the source's version of every path only the source changed.
    fn squash_of(&self, c: &Commit, parent: &str, source: &str) -> bool {
        let Some(base) = self.git.merge_base(parent, source) else {
            return false;
        };
        if base == source || base == parent {
            return false;
        }
        let Ok(landed) = self.git.tree_diff(parent, &c.sha) else {
            return false;
        };
        if landed.is_empty() {
            return false;
        }
        let Ok(src) = self.git.tree_diff(&base, source) else {
            return false;
        };
        if landed.keys().any(|p| !src.contains_key(p)) {
            return false;
        }
        let Ok(target) = self.git.tree_diff(&base, parent) else {
            return false;
        };
        let mut verified = false;
        for (path, blob) in &src {
            match (target.contains_key(path), landed.get(path)) {
                (_, Some(b)) if b == blob => verified = true,
                (false, _) => return false,
                (true, _) => {}
            }
        }
        verified
    }

    /// Whether `c`'s own commits (itself, or those a merge brought) are all
    /// on lane `k` at the time it landed, as commits or copies of their patches.
    fn copied_from(&mut self, k: usize, c: &Commit) -> bool {
        let Some(parent) = c.parents.first() else {
            return false;
        };
        let Some(pos) = self.at(k, c.time) else {
            return false;
        };
        let upstream = self.lanes[k].log[pos].sha.clone();
        // Already on that lane by ancestry: merged back down after landing,
        // so this is the original, not a copy.
        if self.lanes[k].reach.contains(&c.sha) && self.ancestor(&c.sha, &upstream) {
            return false;
        }
        let key = (upstream.clone(), c.sha.clone());
        if let Some(v) = self.memo.copies.get(&key) {
            return *v;
        }
        let v = self.git.patches_present(&upstream, &c.sha, parent);
        self.memo.copies.insert(key, v);
        v
    }

    /// What lane `j` contained at landing `pos`.
    fn evidence(&mut self, j: usize, pos: usize) -> Result<Evidence, String> {
        let mut starts = vec![self.lanes[j].log[pos].sha.clone()];
        let mut copies = Vec::new();
        let mut seen = HashMap::new();
        self.collect(j, pos, &mut starts, &mut copies, &mut seen);
        let reach = if starts.len() == 1 && pos == 0 {
            self.lanes[j].reach.clone()
        } else {
            let s: Vec<&str> = starts.iter().map(String::as_str).collect();
            self.git.reachable(&s, REACH)?
        };
        let copies = self.originals(&copies)?;
        Ok(Evidence { reach, copies })
    }

    /// Gathers the squash sources and rebased copies behind lane `j` from
    /// landing `pos` back, following promotions down to lower lanes.
    fn collect(
        &mut self,
        j: usize,
        pos: usize,
        starts: &mut Vec<String>,
        copies: &mut Vec<String>,
        seen: &mut HashMap<usize, usize>,
    ) {
        if seen.get(&j).is_some_and(|s| *s <= pos) {
            return;
        }
        seen.insert(j, pos);
        for i in pos..self.lanes[j].log.len() {
            match self.kind(j, i) {
                Kind::Promotion {
                    from,
                    source: Some(s),
                } => {
                    let ancestor = pos == 0 && self.lanes[j].reach.contains(&s);
                    if !ancestor && !starts.contains(&s) {
                        starts.push(s.clone());
                    }
                    let t = self.lanes[j].log[i].time;
                    let from_pos = self.lanes[from]
                        .index
                        .get(&s)
                        .copied()
                        .or_else(|| self.at(from, t));
                    if let Some(p) = from_pos {
                        self.collect(from, p, starts, copies, seen);
                    }
                }
                Kind::Promotion { source: None, .. } => {
                    copies.extend(self.copy_commits(j, i));
                }
                _ => {}
            }
        }
    }

    /// The commits whose patches the given rebased copies carry.
    fn originals(&mut self, copies: &[String]) -> Result<HashSet<String>, String> {
        if copies.is_empty() {
            return Ok(HashSet::new());
        }
        if self.pids.is_none() {
            self.pids = Some(self.load_pids()?);
        }
        let by_pid = self.pids.as_ref().expect("loaded");
        let pid_of: HashMap<&str, &str> = by_pid
            .iter()
            .flat_map(|(pid, shas)| shas.iter().map(move |s| (s.as_str(), pid.as_str())))
            .collect();
        let mut out = HashSet::new();
        for c in copies {
            if let Some(pid) = pid_of.get(c.as_str()) {
                out.extend(by_pid[*pid].iter().filter(|s| *s != c).cloned());
            }
        }
        Ok(out)
    }

    /// The commits a rebased or cherry-picked copy landing carries: itself,
    /// or the non-merge commits a merge brought.
    fn copy_commits(&self, j: usize, i: usize) -> Vec<String> {
        let c = &self.lanes[j].log[i];
        match c.parents.as_slice() {
            [first, _, ..] => self.git.own_commits(&c.sha, first),
            _ => vec![c.sha.clone()],
        }
    }

    fn load_pids(&self) -> Result<HashMap<String, Vec<String>>, String> {
        let n = self.lanes.len();
        let mut copies = Vec::new();
        for ((j, i), k) in &self.kinds {
            if matches!(k, Kind::Promotion { source: None, .. }) {
                copies.extend(self.copy_commits(*j, *i));
            }
        }
        let mut args: Vec<String> = vec!["--no-walk".into(), "--no-merges".into()];
        args.extend(copies);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut pids = self.git.patch_ids(&arg_refs)?;
        let top_tip = format!("^{}", self.lanes[n - 1].r.sha);
        let mut lower: Vec<&str> = vec!["--no-merges", COPY_WINDOW];
        lower.extend(self.lanes[..n - 1].iter().map(|l| l.r.sha.as_str()));
        lower.push(&top_tip);
        pids.extend(self.git.patch_ids(&lower)?);
        let mut by_pid: HashMap<String, Vec<String>> = HashMap::new();
        for (sha, pid) in pids {
            by_pid.entry(pid).or_default().push(sha);
        }
        Ok(by_pid)
    }

    /// Whether change `c` is in a lane state.
    fn present(&self, c: &Commit, ev: &Evidence) -> Result<bool, String> {
        if ev.reach.contains(&c.sha) || ev.copies.contains(&c.sha) {
            return Ok(true);
        }
        if ev.copies.is_empty() || c.parents.len() < 2 {
            return Ok(false);
        }
        let own = self.git.own_commits(&c.sha, &c.parents[0]);
        Ok(!own.is_empty()
            && own
                .iter()
                .all(|s| ev.copies.contains(s) || ev.reach.contains(s)))
    }
}

/// The app version at `rev`: an Android app's `versionName`, else the root
/// `Cargo.toml`, `pyproject.toml`, or a published (not private) root
/// `package.json`. `None` where the project has none.
pub fn detect_version(git: &Git, rev: &str) -> Option<String> {
    let files = git.files(rev).ok()?;
    let mut gradle: Vec<&String> = files
        .iter()
        .filter(|p| {
            ["app/build.gradle", "app/build.gradle.kts"]
                .iter()
                .any(|f| p.as_str() == *f || p.ends_with(&format!("/{f}")))
        })
        .collect();
    gradle.sort_by_key(|p| p.matches('/').count());
    for p in gradle {
        if let Some(v) = git.file_at(rev, p).and_then(|t| android_version(&t)) {
            return Some(v);
        }
    }
    let has = |name: &str| files.iter().any(|f| f == name);
    if has("Cargo.toml")
        && let Some(v) = git
            .file_at(rev, "Cargo.toml")
            .and_then(|t| toml_version(&t, &["package"]))
    {
        return Some(v);
    }
    if has("pyproject.toml")
        && let Some(v) = git
            .file_at(rev, "pyproject.toml")
            .and_then(|t| toml_version(&t, &["project", "tool.poetry"]))
    {
        return Some(v);
    }
    if has("package.json") {
        let v: serde_json::Value = serde_json::from_str(&git.file_at(rev, "package.json")?).ok()?;
        if v.get("private").and_then(serde_json::Value::as_bool) != Some(true) {
            return v
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
        }
    }
    None
}

/// `versionName "1.2.1"` (Groovy) or `versionName = "1.2.1"` (Kotlin DSL).
pub fn android_version(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("versionName")?;
        let rest = rest.trim_start().trim_start_matches('=').trim_start();
        let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'')?;
        let inner = &rest[1..];
        let end = inner.find(quote)?;
        Some(inner[..end].to_owned()).filter(|v| !v.is_empty())
    })
}

/// `version = "x"` from the first of `sections` present in a TOML file.
pub fn toml_version(text: &str, sections: &[&str]) -> Option<String> {
    let mut current = String::new();
    let mut found: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim().to_owned();
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "version"
        {
            let value = value.trim();
            if let Some(v) = value
                .strip_prefix('"')
                .and_then(|v| v.split_once('"'))
                .map(|(v, _)| v)
            {
                found.entry(current.clone()).or_insert_with(|| v.to_owned());
            }
        }
    }
    sections.iter().find_map(|s| found.get(*s).cloned())
}

#[cfg(test)]
mod tests;
