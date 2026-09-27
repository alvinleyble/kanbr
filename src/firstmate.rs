//! Access to the Firstmate surfaces Kanbr depends on.
//!
//! The primary surface is Firstmate's canonical fleet snapshot,
//! `bin/fm-fleet-snapshot.sh --json` (schema `fm-fleet-snapshot.v1`), the same
//! complete structured contract `fm-bearings-snapshot.sh` projects from. The
//! bearings projection is bounded and truncated for chat and drops the backlog
//! fields a board needs (project, hold kind, queued versus held), so Kanbr reads
//! the canonical snapshot and only falls back to direct reads for what no
//! snapshot carries:
//!
//! - `state/<id>.meta` for a worker's model and effort;
//! - `data/projects.md` for canonical project names;
//! - each project's git history (read-only; see [`crate::releases`]) to place
//!   merged work in Dev, Staging, or Live and describe releases;
//! - optionally `gh pr view`, for a merged PR its history does not name.
//!
//! Acting from the board (see [`crate::actions`]) goes only through
//! Firstmate's own guarded entry points: the captain inbox
//! (`bin/fm-inbox.sh note`, `receipts`) and the keyed-answer intake
//! (`bin/fm-captain-hold.sh answers`).
//!
//! Every surface is listed in [`SURFACES`] and verified by `kanbr doctor`.
//! Kanbr never writes a Firstmate file or a project repository itself.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::json::str_at;

pub const SNAPSHOT_SCRIPT: &str = "bin/fm-fleet-snapshot.sh";
pub const SNAPSHOT_SCHEMA: &str = "fm-fleet-snapshot.v1";
pub const PROJECTS_REGISTRY: &str = "data/projects.md";
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(45);

pub const INBOX_SCRIPT: &str = "bin/fm-inbox.sh";
pub const HOLD_SCRIPT: &str = "bin/fm-captain-hold.sh";

/// Every Firstmate surface Kanbr reads or writes, for documentation and
/// `kanbr doctor`: `(verb, surface, why)`.
pub const SURFACES: &[(&str, &str, &str)] = &[
    (
        "reads",
        "bin/fm-fleet-snapshot.sh --json",
        "canonical fleet snapshot (fm-fleet-snapshot.v1): backlog, workers, second mates",
    ),
    (
        "reads",
        "state/<id>.meta",
        "worker model and effort (not in any snapshot)",
    ),
    ("reads", "data/projects.md", "registered project names"),
    (
        "reads",
        "projects/<name> git history",
        "Dev, Staging, Live branches; how far each merged change has reached; releases (read-only, as of the last fetch)",
    ),
    (
        "reads",
        "gh pr view (optional)",
        "the commit a merged PR landed as, when its merge message has no PR number; a PR's checks before a merge request",
    ),
    (
        "writes",
        "bin/fm-inbox.sh note --request-id --json",
        "each drag request, worker answer, and answer wake-up as a durable captain inbox note that wakes Firstmate",
    ),
    (
        "reads",
        "bin/fm-inbox.sh receipts, ready",
        "whether Firstmate picked a request up and its reply; whether Firstmate is running to receive requests",
    ),
    (
        "writes",
        "bin/fm-captain-hold.sh answers",
        "a decision answer through Firstmate's keyed-answer intake, in the home that owns the task",
    ),
];

/// Snapshot bounds raised so a board sees every second mate's work, not the
/// chat-sized defaults. Unknown variables are ignored by older snapshots.
const SNAPSHOT_BOUNDS: &[(&str, &str)] = &[
    ("FM_SNAPSHOT_SECONDMATES", "100"),
    ("FM_SNAPSHOT_SECONDMATE_CHILDREN", "200"),
    ("FM_SNAPSHOT_SECONDMATE_QUEUED", "500"),
    ("FM_SNAPSHOT_SECONDMATE_DECISIONS", "200"),
    ("FM_SNAPSHOT_SECONDMATE_LANDED_PER_HOME", "100"),
];

/// Runs the canonical snapshot for `home` and checks its schema.
pub fn run_snapshot(home: &Path, timeout: Duration) -> Result<Value, String> {
    let script = home.join(SNAPSHOT_SCRIPT);
    if !script.is_file() {
        return Err(format!("{} is missing", script.display()));
    }
    let mut cmd = Command::new(&script);
    cmd.arg("--json")
        .current_dir(home)
        .env("FM_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in SNAPSHOT_BOUNDS {
        cmd.env(k, v);
    }
    let (status, stdout, stderr) =
        run_bounded(cmd, timeout).map_err(|e| format!("cannot run {}: {e}", script.display()))?;
    let Some(status) = status else {
        return Err(format!(
            "{SNAPSHOT_SCRIPT} did not finish within {}s",
            timeout.as_secs()
        ));
    };
    if !status.success() {
        let err = stderr.trim();
        return Err(format!(
            "{SNAPSHOT_SCRIPT} exited {}{}",
            status
                .code()
                .map_or("by signal".to_owned(), |c| c.to_string()),
            if err.is_empty() {
                String::new()
            } else {
                format!(": {}", last_line(err))
            }
        ));
    }
    let value: Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("{SNAPSHOT_SCRIPT} printed invalid JSON: {e}"))?;
    match str_at(&value, "schema") {
        Some(SNAPSHOT_SCHEMA) => Ok(value),
        Some(other) => Err(format!(
            "{SNAPSHOT_SCRIPT} reports schema {other}; this Kanbr reads {SNAPSHOT_SCHEMA}"
        )),
        None => Err(format!("{SNAPSHOT_SCRIPT} output has no schema field")),
    }
}

pub fn last_line(text: &str) -> &str {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(text)
}

/// Runs a command with piped stdout/stderr, killing it after `timeout`.
/// Returns `None` for the status when it timed out.
pub fn run_bounded(
    mut cmd: Command,
    timeout: Duration,
) -> std::io::Result<(Option<std::process::ExitStatus>, String, String)> {
    let mut child = cmd.spawn()?;
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let out_reader = thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok((status, stdout, stderr))
}

/// Runs a command with `input` on its stdin (never in its arguments, so it
/// does not show in the process list), killing it after `timeout`.
pub fn run_bounded_input(
    mut cmd: Command,
    input: &[u8],
    timeout: Duration,
) -> std::io::Result<(Option<std::process::ExitStatus>, String, String)> {
    use std::io::Write;
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take();
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let out_reader = thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    // Input is small (one request); a child that exits without reading it
    // only makes this write fail, which its exit status then explains.
    if let Some(mut i) = stdin.take() {
        let _ = i.write_all(input);
    }
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok((status, stdout, stderr))
}

/// A `key=value` task meta file.
pub type Meta = HashMap<String, String>;

pub fn parse_meta(text: &str) -> Meta {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

pub fn read_meta(path: &Path) -> Option<Meta> {
    fs::read_to_string(path).ok().map(|t| parse_meta(&t))
}

/// Project names from `data/projects.md` registry lines (`- <name> [mode] - desc`).
pub fn parse_projects_registry(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("- "))
        .filter_map(|rest| {
            let end = [rest.find(" ["), rest.find(" - ")]
                .into_iter()
                .flatten()
                .min()?;
            let name = rest[..end].trim();
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect()
}

pub fn read_projects_registry(home: &Path) -> Result<Vec<String>, String> {
    let path = home.join(PROJECTS_REGISTRY);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(parse_projects_registry(&text))
}

/// Which release lanes a project uses: whether it has the branch configured
/// to back Dev, Staging, and Live (locally or on any remote).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lanes {
    pub dev: bool,
    pub staging: bool,
    pub live: bool,
}

/// A cheap fingerprint of the files the snapshot is derived from, so the
/// board re-reads the full snapshot only when something changed. Pane-derived
/// worker state changes without a file change; the periodic full refresh
/// covers that.
pub fn fingerprint(home: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    let mut stamp = |p: &Path| {
        if let Ok(md) = fs::metadata(p) {
            p.hash(&mut h);
            md.len().hash(&mut h);
            if let Ok(t) = md.modified() {
                t.hash(&mut h);
            }
        }
    };
    for rel in ["data/backlog.md", "data/secondmates.md", PROJECTS_REGISTRY] {
        stamp(&home.join(rel));
    }
    // Only task meta and status files: a spawn or teardown adds or removes one,
    // while the state directory's own mtime churns with unrelated bookkeeping.
    if let Ok(entries) = fs::read_dir(home.join("state")) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("status" | "meta")
                )
            })
            .collect();
        paths.sort();
        for p in paths {
            stamp(&p);
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_meta_lines() {
        let m = parse_meta(
            "model=claude-opus-5-5\neffort = xhigh\nnoequals\n=novalue\nwindow=default:w4R:p2\n",
        );
        assert_eq!(m.get("model").map(String::as_str), Some("claude-opus-5-5"));
        assert_eq!(m.get("effort").map(String::as_str), Some("xhigh"));
        assert_eq!(m.get("window").map(String::as_str), Some("default:w4R:p2"));
        assert!(!m.contains_key(""));
    }

    #[test]
    fn parses_registry_names() {
        let text = "# Projects\n\nFleet registry.\n\n- ipon-love [local-only] - app; origin x (added 2026-08-03)\n- Leyble-Hub [direct-PR] - admin app\n- legacy name - no mode (added 2026-01-01)\n- herdr [direct-PR] - fork - with dashes\n-not a row\n";
        assert_eq!(
            parse_projects_registry(text),
            vec!["ipon-love", "Leyble-Hub", "legacy name", "herdr"]
        );
    }

    #[test]
    fn fingerprint_tracks_backlog_and_task_files() {
        let dir = std::env::temp_dir().join(format!("kanbr-fp-{}", std::process::id()));
        fs::create_dir_all(dir.join("data")).unwrap();
        fs::create_dir_all(dir.join("state")).unwrap();
        fs::write(dir.join("data/backlog.md"), "a").unwrap();
        let a = fingerprint(&dir);
        assert_eq!(a, fingerprint(&dir));
        fs::write(dir.join("state/unrelated.lock"), "x").unwrap();
        assert_eq!(a, fingerprint(&dir), "unrelated state files do not count");
        fs::write(dir.join("state/t1.meta"), "model=x").unwrap();
        let b = fingerprint(&dir);
        assert_ne!(a, b, "a new task meta counts");
        fs::write(dir.join("data/backlog.md"), "ab").unwrap();
        assert_ne!(b, fingerprint(&dir), "a backlog edit counts");
        fs::remove_dir_all(&dir).unwrap();
    }
}
