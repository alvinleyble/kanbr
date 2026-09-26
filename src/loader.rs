//! Loads a board from a Firstmate home, caching the cheap side reads between
//! refreshes (meta files by modification time, git lanes for a few minutes).

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::config::Config;
use crate::dates::now_epoch;
use crate::firstmate::{
    Lanes, Meta, SNAPSHOT_TIMEOUT, read_lanes, read_meta, read_projects_registry, run_snapshot,
};
use crate::model::{Board, Context, Env, build_board};

const LANES_TTL: Duration = Duration::from_secs(300);

/// Meta files by path, with the modification time they were read at.
type MetaCache = HashMap<PathBuf, (Option<SystemTime>, Option<Meta>)>;

pub struct Loader {
    pub home: PathBuf,
    pub config: Config,
    metas: RefCell<MetaCache>,
    lanes: RefCell<HashMap<PathBuf, (Instant, Option<Lanes>)>>,
}

impl Loader {
    pub fn new(home: PathBuf, config: Config) -> Self {
        Loader {
            home,
            config,
            metas: RefCell::new(HashMap::new()),
            lanes: RefCell::new(HashMap::new()),
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

    fn lanes(&self, repo: &Path) -> Option<Lanes> {
        let mut cache = self.lanes.borrow_mut();
        if let Some((at, lanes)) = cache.get(repo)
            && at.elapsed() < LANES_TTL
        {
            return *lanes;
        }
        let lanes = read_lanes(repo, self.config.branches());
        cache.insert(repo.to_path_buf(), (Instant::now(), lanes));
        lanes
    }
}
