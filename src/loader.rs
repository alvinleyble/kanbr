//! Loads a board from a Firstmate home, caching the side reads between
//! refreshes: meta files by modification time, each project's release
//! analysis until one of its lane branches moves or the clone fetches, and
//! forge answers about merged PRs.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::config::Config;
use crate::dates::now_epoch;
use crate::firstmate::{Meta, SNAPSHOT_TIMEOUT, read_meta, read_projects_registry, run_snapshot};
use crate::forge::{self, GhError};
use crate::git::{Git, fetch_time, is_checkout};
use crate::model::{Board, Context, Env, build_board};
use crate::releases::{Memo, ProjectGit, analyze_refs, lane_tips};

/// How long an answer about an unmerged or unknown PR is trusted.
const FORGE_TTL: Duration = Duration::from_secs(600);

/// Meta files by path, with the modification time they were read at.
type MetaCache = HashMap<PathBuf, (Option<SystemTime>, Option<Meta>)>;

/// Promotion PRs by (repository, Live branch, tip), with when they were asked.
type PromotionCache = HashMap<(String, String, String), (Instant, Option<(u64, String)>)>;

/// What a release analysis was computed from: the lane tips and the last fetch.
type GitKey = (Vec<Option<String>>, Option<i64>);

pub struct Loader {
    pub home: PathBuf,
    pub config: Config,
    metas: RefCell<MetaCache>,
    gits: RefCell<HashMap<PathBuf, (GitKey, Arc<ProjectGit>)>>,
    memos: RefCell<HashMap<PathBuf, Memo>>,
    forge: RefCell<HashMap<String, (Instant, Option<String>)>>,
    promotions: RefCell<PromotionCache>,
    gh_missing: Cell<bool>,
}

impl Loader {
    pub fn new(home: PathBuf, config: Config) -> Self {
        Loader {
            home,
            config,
            metas: RefCell::new(HashMap::new()),
            gits: RefCell::new(HashMap::new()),
            memos: RefCell::new(HashMap::new()),
            forge: RefCell::new(HashMap::new()),
            promotions: RefCell::new(HashMap::new()),
            gh_missing: Cell::new(false),
        }
    }

    pub fn load(&self) -> Result<Board, String> {
        let snapshot = run_snapshot(&self.home, SNAPSHOT_TIMEOUT)?;
        let (registry, registry_error) = match read_projects_registry(&self.home) {
            Ok(r) => (r, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let ctx = Context {
            registry: &registry,
            config: &self.config,
            now: now_epoch(),
            env: self,
        };
        let mut board = build_board(&snapshot, &ctx);
        if let Some(e) = registry_error {
            board.notices.push(format!(
                "project registry unreadable, names may differ in case: {e}"
            ));
        }
        Ok(board)
    }
}

impl Env for Loader {
    fn meta(&self, path: &Path) -> Option<Meta> {
        let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
        let mut cache = self.metas.borrow_mut();
        if let Some((cached_mtime, meta)) = cache.get(path)
            && *cached_mtime == mtime
        {
            return meta.clone();
        }
        let meta = read_meta(path);
        cache.insert(path.to_path_buf(), (mtime, meta.clone()));
        meta
    }

    fn git(&self, repo: &Path) -> Option<Result<Arc<ProjectGit>, String>> {
        if !is_checkout(repo) {
            return None;
        }
        let git = Git::new(repo);
        let refs = match git.branches() {
            Ok(r) => r,
            Err(e) => return Some(Err(e)),
        };
        let branches = self.config.branches();
        let key = (lane_tips(&refs, branches), fetch_time(repo));
        if let Some((k, pg)) = self.gits.borrow().get(repo)
            && *k == key
        {
            return Some(Ok(pg.clone()));
        }
        let mut memos = self.memos.borrow_mut();
        let memo = memos.entry(repo.to_path_buf()).or_default();
        let promotions = |web: &str, branch: &str, tip: &str| {
            if self.gh_missing.get() {
                return None;
            }
            let key = (web.to_owned(), branch.to_owned(), tip.to_owned());
            if let Some((at, p)) = self.promotions.borrow().get(&key)
                && (p.is_some() || at.elapsed() < FORGE_TTL)
            {
                return p.clone();
            }
            let p = match forge::promotion(web, branch, tip) {
                Ok(p) => p,
                Err(GhError::Missing) => {
                    self.gh_missing.set(true);
                    return None;
                }
                Err(GhError::Failed(_)) => None,
            };
            self.promotions
                .borrow_mut()
                .insert(key, (Instant::now(), p.clone()));
            p
        };
        let result = analyze_refs(&git, &refs, branches, memo, &promotions).map(Arc::new);
        if let Ok(pg) = &result {
            self.gits
                .borrow_mut()
                .insert(repo.to_path_buf(), (key, pg.clone()));
        }
        Some(result)
    }

    fn merged_commit(&self, pr_url: &str) -> Option<String> {
        if self.gh_missing.get() {
            return None;
        }
        if let Some((at, sha)) = self.forge.borrow().get(pr_url)
            && (sha.is_some() || at.elapsed() < FORGE_TTL)
        {
            return sha.clone();
        }
        let sha = match forge::merged_commit(pr_url) {
            Ok(sha) => sha,
            Err(GhError::Missing) => {
                self.gh_missing.set(true);
                return None;
            }
            Err(GhError::Failed(_)) => None,
        };
        self.forge
            .borrow_mut()
            .insert(pr_url.to_owned(), (Instant::now(), sha.clone()));
        sha
    }
}
