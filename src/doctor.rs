//! `kanbr doctor`: verifies every Firstmate surface Kanbr reads, so a
//! Firstmate update that breaks one fails loudly here instead of showing a
//! silently empty board.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::{Config, resolve_home};
use crate::dates::now_epoch;
use crate::firstmate::{
    PROJECTS_REGISTRY, SNAPSHOT_SCHEMA, SNAPSHOT_SCRIPT, SNAPSHOT_TIMEOUT, SURFACES, read_lanes,
    read_meta, read_projects_registry, run_bounded, run_snapshot,
};
use crate::json::{arr_at, get, str_at};
use crate::loader::Loader;
use crate::model::{Column, Context, build_board};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub level: Level,
    pub name: &'static str,
    pub detail: String,
}

fn check(level: Level, name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        level,
        name,
        detail: detail.into(),
    }
}

/// Backlog row fields the board reads (`backlog.records[]`, structured rows).
pub const BACKLOG_KEYS: &[&str] = &[
    "id",
    "state",
    "title",
    "repo",
    "kind",
    "hold_reason",
    "hold_kind",
    "hold_until",
    "since",
    "captain_actionable",
    "unresolved_blocker_ids",
    "current_role",
    "completion",
    "pr_url",
];

/// Worker fields the board reads (`tasks[]`).
pub const TASK_KEYS: &[&str] = &[
    "id",
    "kind",
    "harness",
    "project",
    "spawn_gen",
    "current_state",
    "paths",
    "pr",
    "hints",
];

/// Second-mate record fields the board reads (`secondmate_current.records[]`).
pub const SECONDMATE_KEYS: &[&str] = &[
    "id",
    "home",
    "provenance",
    "active_children",
    "queued",
    "decisions_open",
    "landed",
];

/// Whether `path` exists in `v` (the value itself may be `null`).
fn has_path(v: &Value, path: &str) -> bool {
    let mut cur = v;
    for key in path.split('.') {
        match cur.as_object().and_then(|o| o.get(key)) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    true
}

/// Counts rows missing each required key; returns `key (n rows)` descriptions.
fn missing_keys<'a>(rows: impl Iterator<Item = &'a Value>, keys: &[&str]) -> Vec<String> {
    let rows: Vec<&Value> = rows.collect();
    keys.iter()
        .filter_map(|k| {
            let n = rows.iter().filter(|r| !has_path(r, k)).count();
            (n > 0).then(|| format!("{k} ({n} of {} rows)", rows.len()))
        })
        .collect()
}

/// Checks the snapshot carries every field Kanbr maps to the board.
pub fn contract_checks(snap: &Value) -> Vec<Check> {
    let mut out = Vec::new();
    let top = [
        "schema",
        "generated",
        "fm_home",
        "roots.projects",
        "backlog.records",
        "tasks",
        "secondmate_current.registry",
        "secondmate_current.records",
    ];
    let missing: Vec<&str> = top.iter().copied().filter(|k| !has_path(snap, k)).collect();
    let arrays = ["backlog.records", "tasks", "secondmate_current.records"];
    let not_arrays: Vec<&str> = arrays
        .iter()
        .copied()
        .filter(|k| has_path(snap, k) && !get(snap, k).is_array())
        .collect();
    if missing.is_empty() && not_arrays.is_empty() {
        out.push(check(
            Level::Ok,
            "snapshot shape",
            "top-level sections present",
        ));
    } else {
        let mut d = String::new();
        if !missing.is_empty() {
            let _ = write!(d, "missing {}", missing.join(", "));
        }
        if !not_arrays.is_empty() {
            let _ = write!(
                d,
                "{}not arrays: {}",
                if d.is_empty() { "" } else { "; " },
                not_arrays.join(", ")
            );
        }
        out.push(check(Level::Fail, "snapshot shape", d));
    }

    let rows: Vec<&Value> = arr_at(snap, "backlog.records")
        .iter()
        .filter(|r| get(r, "structured").as_bool() == Some(true))
        .collect();
    if rows.is_empty() {
        out.push(check(
            Level::Warn,
            "backlog rows",
            "no structured backlog rows to verify",
        ));
    } else {
        let missing = missing_keys(rows.iter().copied(), BACKLOG_KEYS);
        out.push(if missing.is_empty() {
            check(
                Level::Ok,
                "backlog rows",
                format!(
                    "{} structured rows carry every field Kanbr reads",
                    rows.len()
                ),
            )
        } else {
            check(
                Level::Fail,
                "backlog rows",
                format!("missing {}", missing.join(", ")),
            )
        });
    }

    let tasks: Vec<&Value> = arr_at(snap, "tasks")
        .iter()
        .filter(|t| str_at(t, "kind") != Some("secondmate"))
        .collect();
    if tasks.is_empty() {
        out.push(check(Level::Ok, "worker rows", "no live workers to verify"));
    } else {
        let mut missing = missing_keys(tasks.iter().copied(), TASK_KEYS);
        let nested = [
            ("current_state", "state"),
            ("paths", "meta"),
            ("pr", "url"),
            ("hints", "pending_decision"),
        ];
        for (parent, child) in nested {
            let n = tasks
                .iter()
                .filter(|t| get(t, parent).is_object() && !has_path(get(t, parent), child))
                .count();
            if n > 0 {
                missing.push(format!("{parent}.{child} ({n} of {} rows)", tasks.len()));
            }
        }
        out.push(if missing.is_empty() {
            check(
                Level::Ok,
                "worker rows",
                format!("{} workers carry every field Kanbr reads", tasks.len()),
            )
        } else {
            check(
                Level::Fail,
                "worker rows",
                format!("missing {}", missing.join(", ")),
            )
        });
    }

    let registry = get(snap, "secondmate_current.registry");
    let mates = arr_at(snap, "secondmate_current.records");
    if get(registry, "available").as_bool() == Some(false) {
        out.push(check(
            Level::Fail,
            "second mates",
            format!(
                "registry unavailable: {}",
                str_at(registry, "reason").unwrap_or("read failed")
            ),
        ));
    } else if mates.is_empty() {
        out.push(check(
            Level::Ok,
            "second mates",
            "registry readable; none registered",
        ));
    } else {
        let missing = missing_keys(mates.iter(), SECONDMATE_KEYS);
        let unreadable: Vec<&str> = mates
            .iter()
            .filter(|m| str_at(m, "provenance.selected") != Some("structured-home"))
            .filter_map(|m| str_at(m, "id"))
            .collect();
        out.push(if !missing.is_empty() {
            check(
                Level::Fail,
                "second mates",
                format!("missing {}", missing.join(", ")),
            )
        } else if !unreadable.is_empty() {
            check(
                Level::Warn,
                "second mates",
                format!("home unreadable: {}", unreadable.join(", ")),
            )
        } else {
            check(
                Level::Ok,
                "second mates",
                format!("{} registered, every home readable", mates.len()),
            )
        });
    }
    out
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn git_available() -> bool {
    let mut cmd = Command::new("git");
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    matches!(run_bounded(cmd, Duration::from_secs(5)), Ok((Some(s), _, _)) if s.success())
}

/// Runs every check. `home_flag` and `config_path` are the CLI overrides.
pub fn run(home_flag: Option<&Path>, config_path: Option<&Path>) -> Vec<Check> {
    let mut out = Vec::new();
    let config = match Config::load(config_path) {
        Ok(c) => {
            let where_ = c
                .source
                .as_ref()
                .map_or("defaults (no config file)".to_owned(), |p| {
                    p.display().to_string()
                });
            let [d, s, l] = c.branches();
            out.push(check(
                Level::Ok,
                "config",
                format!("{where_}; branches dev={d} staging={s} live={l}"),
            ));
            c
        }
        Err(e) => {
            out.push(check(Level::Fail, "config", e));
            Config::default()
        }
    };
    let home = match resolve_home(home_flag, &config) {
        Ok((home, source)) => {
            out.push(check(
                Level::Ok,
                "firstmate home",
                format!("{} (from {})", home.display(), source.describe()),
            ));
            home
        }
        Err(e) => {
            out.push(check(Level::Fail, "firstmate home", e));
            return out;
        }
    };

    let script = home.join(SNAPSHOT_SCRIPT);
    if !is_executable(&script) {
        out.push(check(
            Level::Fail,
            "snapshot script",
            format!("{} is missing or not executable", script.display()),
        ));
        return out;
    }
    let started = Instant::now();
    let snap = match run_snapshot(&home, SNAPSHOT_TIMEOUT) {
        Ok(s) => {
            out.push(check(
                Level::Ok,
                "snapshot",
                format!(
                    "{SNAPSHOT_SCRIPT} --json ran in {:.1}s, schema {SNAPSHOT_SCHEMA}",
                    started.elapsed().as_secs_f64()
                ),
            ));
            s
        }
        Err(e) => {
            out.push(check(Level::Fail, "snapshot", e));
            return out;
        }
    };
    out.extend(contract_checks(&snap));

    let metas: Vec<PathBuf> = arr_at(&snap, "tasks")
        .iter()
        .filter(|t| str_at(t, "kind") != Some("secondmate"))
        .filter_map(|t| str_at(t, "paths.meta.path").map(PathBuf::from))
        .collect();
    if metas.is_empty() {
        out.push(check(Level::Ok, "worker meta", "no live workers to read"));
    } else {
        let read: Vec<_> = metas.iter().filter_map(|p| read_meta(p)).collect();
        let with_model = read.iter().filter(|m| m.contains_key("model")).count();
        let detail = format!(
            "{} of {} state/<id>.meta readable, {with_model} name a model",
            read.len(),
            metas.len()
        );
        let level = if read.len() == metas.len() && with_model == read.len() {
            Level::Ok
        } else {
            Level::Warn
        };
        out.push(check(level, "worker meta", detail));
    }

    let registry = match read_projects_registry(&home) {
        Ok(names) if !names.is_empty() => {
            out.push(check(
                Level::Ok,
                "project registry",
                format!("{PROJECTS_REGISTRY}: {} projects", names.len()),
            ));
            names
        }
        Ok(_) => {
            out.push(check(
                Level::Warn,
                "project registry",
                format!("{PROJECTS_REGISTRY} lists no projects"),
            ));
            Vec::new()
        }
        Err(e) => {
            out.push(check(
                Level::Warn,
                "project registry",
                format!("{e}; project names fall back to backlog repo fields"),
            ));
            Vec::new()
        }
    };

    if git_available() {
        let projects_dir = str_at(&snap, "roots.projects")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("projects"));
        let mut read = Vec::new();
        let mut unreadable = Vec::new();
        let names = std::iter::once("firstmate".to_owned()).chain(registry.iter().cloned());
        for name in names {
            let path = if name == "firstmate" {
                home.clone()
            } else {
                projects_dir.join(&name)
            };
            match read_lanes(&path, config.branches()) {
                Some(l) => {
                    let used: Vec<&str> =
                        [(l.dev, "dev"), (l.staging, "staging"), (l.live, "live")]
                            .into_iter()
                            .filter_map(|(u, n)| u.then_some(n))
                            .collect();
                    read.push(format!(
                        "{name} {}",
                        if used.is_empty() {
                            "-".to_owned()
                        } else {
                            used.join("+")
                        }
                    ));
                }
                None => unreadable.push(name),
            }
        }
        let mut detail = read.join(", ");
        if !unreadable.is_empty() {
            let _ = write!(
                detail,
                "; no readable repo (only the last lane assumed): {}",
                unreadable.join(", ")
            );
        }
        out.push(check(
            if unreadable.is_empty() {
                Level::Ok
            } else {
                Level::Warn
            },
            "project lanes",
            detail,
        ));
    } else {
        out.push(check(
            Level::Warn,
            "project lanes",
            "git not found; every project is assumed to use only the last lane",
        ));
    }

    let loader = Loader::new(home.clone(), config.clone());
    let ctx = Context {
        registry: &registry,
        config: &config,
        now: now_epoch(),
        env: &loader,
    };
    let board = build_board(&snap, &ctx);
    let open_rows = arr_at(&snap, "backlog.records")
        .iter()
        .filter(|r| get(r, "structured").as_bool() == Some(true))
        .filter(|r| matches!(str_at(r, "state"), Some("queued" | "in_flight")))
        .count();
    let counts: Vec<String> = Column::ALL
        .iter()
        .map(|c| {
            format!(
                "{} {}",
                config.label(*c),
                board.column_cards(*c, None).len()
            )
        })
        .collect();
    if open_rows > 0 && board.cards.is_empty() {
        out.push(check(
            Level::Fail,
            "board",
            format!(
                "{open_rows} open backlog rows produced no cards: the snapshot shape has drifted"
            ),
        ));
    } else {
        out.push(check(
            Level::Ok,
            "board",
            format!(
                "{} cards ({}), {} waiting on you, {} tabs",
                board.cards.len(),
                counts.join(" · "),
                board.waiting().len(),
                board.projects.len() + 1
            ),
        ));
    }
    for n in &board.notices {
        out.push(check(Level::Warn, "board notice", n.clone()));
    }
    out
}

/// Formats the report and returns it with the process exit code.
pub fn report(checks: &[Check]) -> (String, i32) {
    let mut s = String::from("kanbr doctor: checking every Firstmate surface Kanbr reads\n");
    for (surface, why) in SURFACES {
        let _ = writeln!(s, "  reads {surface}: {why}");
    }
    s.push('\n');
    for c in checks {
        let tag = match c.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        let _ = writeln!(s, "  {tag}  {:<17} {}", c.name, c.detail);
    }
    let fails = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warns = checks.iter().filter(|c| c.level == Level::Warn).count();
    let _ = writeln!(
        s,
        "\n{}",
        if fails > 0 {
            format!("{fails} check(s) failed: the board would be wrong or empty until fixed.")
        } else if warns > 0 {
            format!("All surfaces readable, {warns} warning(s).")
        } else {
            "All surfaces readable.".to_owned()
        }
    );
    (s, i32::from(fails > 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE: &str = include_str!("../tests/fixtures/snapshot.json");

    #[test]
    fn fixture_passes_the_contract() {
        let snap: Value = serde_json::from_str(FIXTURE).unwrap();
        let checks = contract_checks(&snap);
        assert!(checks.iter().all(|c| c.level != Level::Fail), "{checks:?}");
    }

    #[test]
    fn dropped_backlog_field_fails() {
        let mut snap: Value = serde_json::from_str(FIXTURE).unwrap();
        for r in snap["backlog"]["records"].as_array_mut().unwrap() {
            r.as_object_mut().unwrap().remove("hold_kind");
        }
        let checks = contract_checks(&snap);
        let c = checks.iter().find(|c| c.name == "backlog rows").unwrap();
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("hold_kind"), "{}", c.detail);
    }

    #[test]
    fn renamed_section_fails() {
        let snap = json!({"schema": SNAPSHOT_SCHEMA, "generated": "x", "fm_home": "/fm",
            "roots": {"projects": "/fm/projects"}, "backlog": {"rows": []}, "tasks": [],
            "secondmate_current": {"registry": {}, "records": []}});
        let checks = contract_checks(&snap);
        let c = checks.iter().find(|c| c.name == "snapshot shape").unwrap();
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("backlog.records"));
    }

    #[test]
    fn dropped_worker_state_fails() {
        let mut snap: Value = serde_json::from_str(FIXTURE).unwrap();
        snap["tasks"][0]["current_state"]
            .as_object_mut()
            .unwrap()
            .remove("state");
        let checks = contract_checks(&snap);
        let c = checks.iter().find(|c| c.name == "worker rows").unwrap();
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("current_state.state"), "{}", c.detail);
    }

    #[test]
    fn unavailable_second_mate_registry_fails() {
        let mut snap: Value = serde_json::from_str(FIXTURE).unwrap();
        snap["secondmate_current"]["registry"] =
            json!({"available": false, "reason": "unreadable table"});
        let checks = contract_checks(&snap);
        let c = checks.iter().find(|c| c.name == "second mates").unwrap();
        assert_eq!(c.level, Level::Fail);
    }

    #[test]
    fn unreadable_second_mate_home_warns() {
        let snap: Value = serde_json::from_str(FIXTURE).unwrap();
        let c = contract_checks(&snap)
            .into_iter()
            .find(|c| c.name == "second mates")
            .unwrap();
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("sm-beta"));
    }

    #[test]
    fn report_exit_code_reflects_failures() {
        let ok = vec![
            check(Level::Ok, "a", "fine"),
            check(Level::Warn, "b", "meh"),
        ];
        assert_eq!(report(&ok).1, 0);
        let bad = vec![check(Level::Fail, "a", "broken")];
        let (text, code) = report(&bad);
        assert_eq!(code, 1);
        assert!(text.contains("FAIL  a"));
        assert!(text.contains("reads bin/fm-fleet-snapshot.sh --json"));
    }
}
