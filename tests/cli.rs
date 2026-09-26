//! End-to-end checks of the `kanbr` binary against a fake Firstmate home whose
//! snapshot script prints the recorded fixture.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE: &str = include_str!("fixtures/snapshot.json");

struct FakeHome {
    root: PathBuf,
}

impl FakeHome {
    fn new(name: &str, script: &str) -> FakeHome {
        let root = std::env::temp_dir().join(format!("kanbr-it-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("data")).unwrap();
        fs::create_dir_all(root.join("state")).unwrap();
        fs::write(root.join("snapshot.json"), FIXTURE).unwrap();
        let script_path = root.join("bin/fm-fleet-snapshot.sh");
        fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(
            root.join("data/projects.md"),
            "# Projects\n\n- Shop [direct-PR] - a shop\n- Site [direct-PR] - a site\n- tool - a tool\n- legacy [local-only] - old\n- kanbr [no-mistakes] - the board\n",
        )
        .unwrap();
        FakeHome { root }
    }

    fn serving_fixture(name: &str) -> FakeHome {
        FakeHome::new(
            name,
            "#!/bin/sh\n[ \"$1\" = --json ] || exit 2\n[ -n \"$FM_HOME\" ] || exit 3\ncat \"$FM_HOME/snapshot.json\"\n",
        )
    }
}

impl Drop for FakeHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn kanbr(args: &[&str], home: Option<&Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kanbr"));
    cmd.args(args)
        .env_remove("KANBR_FM_HOME")
        .env_remove("FM_HOME")
        .env_remove("HERDR_PLUGIN_CONFIG_DIR")
        .env(
            "XDG_CONFIG_HOME",
            std::env::temp_dir().join("kanbr-it-no-config"),
        );
    if let Some(h) = home {
        cmd.arg("--home").arg(h);
    }
    cmd.output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn print_shows_the_board() {
    let home = FakeHome::serving_fixture("print");
    let out = kanbr(&["print"], Some(&home.root));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    for heading in [
        "Booked (",
        "Ready (",
        "Building (",
        "Dev (",
        "Staging (",
        "Live (",
    ] {
        assert!(text.contains(heading), "{heading}\n{text}");
    }
    assert!(text.contains("⚑ 4 waiting on you"), "{text}");
    assert!(text.contains("Tabs: All ("), "{text}");
}

#[test]
fn print_tab_filters_and_rejects_unknown_tabs() {
    let home = FakeHome::serving_fixture("tab");
    let out = kanbr(&["print", "--tab", "Shop"], Some(&home.root));
    assert!(out.status.success());
    assert!(!stdout(&out).contains("kanbr ·"));
    let bad = kanbr(&["print", "--tab", "Nope"], Some(&home.root));
    assert_eq!(bad.status.code(), Some(1));
}

#[test]
fn config_file_relabels_columns() {
    let home = FakeHome::serving_fixture("config");
    let config = home.root.join("kanbr.conf");
    fs::write(&config, "live_label = Production\nbooked_label = Backlog\n").unwrap();
    let out = kanbr(
        &["print", "--config", config.to_str().unwrap()],
        Some(&home.root),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(
        text.contains("Production (") && text.contains("Backlog ("),
        "{text}"
    );
}

#[test]
fn doctor_passes_on_a_healthy_home() {
    let home = FakeHome::serving_fixture("doctor-ok");
    let out = kanbr(&["doctor"], Some(&home.root));
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("ok    snapshot "), "{text}");
    assert!(text.contains("ok    backlog rows"), "{text}");
    assert!(text.contains("reads state/<id>.meta"), "{text}");
}

#[test]
fn doctor_fails_when_the_snapshot_breaks() {
    let home = FakeHome::new(
        "doctor-broken",
        "#!/bin/sh\necho 'jq: not found' >&2\nexit 1\n",
    );
    let out = kanbr(&["doctor"], Some(&home.root));
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("FAIL  snapshot"), "{text}");
    assert!(text.contains("jq: not found"), "{text}");
}

#[test]
fn doctor_fails_on_schema_drift() {
    let home = FakeHome::new(
        "doctor-drift",
        "#!/bin/sh\necho '{\"schema\":\"fm-fleet-snapshot.v2\"}'\n",
    );
    let out = kanbr(&["doctor"], Some(&home.root));
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("fm-fleet-snapshot.v2"), "{text}");
}

#[test]
fn doctor_fails_on_a_dropped_field() {
    let home = FakeHome::new(
        "doctor-field",
        "#!/bin/sh\nsed 's/\"hold_kind\"/\"hold_type\"/g' \"$FM_HOME/snapshot.json\"\n",
    );
    let out = kanbr(&["doctor"], Some(&home.root));
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("hold_kind"), "{text}");
}

#[test]
fn print_reports_a_broken_snapshot_instead_of_an_empty_board() {
    let home = FakeHome::new("print-broken", "#!/bin/sh\nexit 4\n");
    let out = kanbr(&["print"], Some(&home.root));
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("exited 4") && err.contains("kanbr doctor"),
        "{err}"
    );
}

#[test]
fn a_wrong_home_is_refused() {
    let out = kanbr(&["print"], Some(Path::new("/definitely/not/firstmate")));
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a Firstmate home"));
}

#[test]
fn board_needs_a_terminal() {
    let home = FakeHome::serving_fixture("tty");
    let out = kanbr(&[], Some(&home.root));
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("kanbr print"));
}

#[test]
fn version_and_help() {
    let v = kanbr(&["--version"], None);
    assert!(stdout(&v).starts_with("kanbr "));
    let h = kanbr(&["--help"], None);
    assert!(stdout(&h).contains("kanbr doctor"));
    let bad = kanbr(&["frobnicate"], None);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn never_writes_into_the_firstmate_home() {
    let home = FakeHome::serving_fixture("readonly");
    let before = listing(&home.root);
    assert!(kanbr(&["print"], Some(&home.root)).status.success());
    assert!(kanbr(&["doctor"], Some(&home.root)).status.success());
    assert_eq!(before, listing(&home.root));
}

fn listing(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push((p, len));
            }
        }
    }
    out.sort();
    out
}
