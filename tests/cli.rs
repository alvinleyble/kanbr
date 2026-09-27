//! End-to-end checks of the `kanbr` binary against a fake Firstmate home whose
//! snapshot script prints the recorded fixture.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE: &str = include_str!("fixtures/snapshot.json");

/// A captain inbox that answers only the read-only probes; any request it is
/// sent leaves a file behind, so a test can prove none was.
const INBOX: &str = r#"#!/bin/sh
case "$1" in
  --help) printf 'Usage:\n  fm-inbox.sh note [--request-id <id>] [--json] [--] <text>...\n  fm-inbox.sh note [--request-id <id>] [--json] -   (body from stdin)\n  fm-inbox.sh reply [--json] <id> <text>...\n  fm-inbox.sh receipts [--after <cursor>]\n  fm-inbox.sh ready\n' ;;
  receipts) printf '{"schema":"fm-inbox-receipts.v1","pending":[],"handled":[{"id":"n1","reply":null}],"replies":[]}\n' ;;
  ready) printf '{"schema":"fm-primary-ready.v1","lock":{"state":"held"},"wake_consumer":{"state":"healthy","reason":"supervised"},"can_receive":true}\n' ;;
  *) : > "$FM_HOME/REQUEST-SENT"; exit 1 ;;
esac
"#;

const HOLD: &str = r#"#!/bin/sh
case "$1" in
  --help) printf 'Usage:\n  fm-captain-hold.sh answers [<legacy-origin> | --any-origin] --source <provenance>   (keyed answers on stdin)\n' ;;
  *) : > "$FM_HOME/ANSWER-SENT"; exit 1 ;;
esac
"#;

fn write_script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

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
        write_script(&root.join("bin/fm-fleet-snapshot.sh"), script);
        write_script(&root.join("bin/fm-inbox.sh"), INBOX);
        write_script(&root.join("bin/fm-captain-hold.sh"), HOLD);
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
    for line in [
        "ok    inbox requests",
        "ok    inbox receipts    bin/fm-inbox.sh receipts reads 0 pending and 1 handled",
        "ok    firstmate ready",
        "ok    answer intake",
        "writes bin/fm-inbox.sh note --request-id --json",
        "writes bin/fm-captain-hold.sh answers",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    // The fixture's second mate lives at a path this machine does not have.
    assert!(text.contains("warn  mate intakes"), "{text}");
}

#[test]
fn doctor_fails_when_firstmate_cannot_take_requests() {
    let home = FakeHome::serving_fixture("doctor-inbox");
    write_script(
        &home.root.join("bin/fm-inbox.sh"),
        "#!/bin/sh\n[ \"$1\" = --help ] && printf 'fm-inbox.sh note <text>...\\nfm-inbox.sh receipts\\n' && exit 0\nexit 1\n",
    );
    fs::remove_file(home.root.join("bin/fm-captain-hold.sh")).unwrap();
    let out = kanbr(&["doctor"], Some(&home.root));
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("FAIL  inbox requests    bin/fm-inbox.sh lacks note --request-id"),
        "{text}"
    );
    assert!(text.contains("FAIL  inbox receipts"), "{text}");
    assert!(text.contains("warn  firstmate ready"), "{text}");
    assert!(text.contains("FAIL  answer intake"), "{text}");
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

/// Runs git in `dir` with a fixed identity and commit time.
fn git(dir: &Path, args: &[&str], time: i64) {
    let date = format!("@{time} +0000");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A fake home whose snapshot points at its own projects directory, with a
/// Shop repository: the fixture's PR #139 and a hand-opened PR #140 merged to
/// dev and promoted through staging to main (#150), then #141 merged to dev.
fn home_with_shop_repo(name: &str) -> FakeHome {
    let home = FakeHome::serving_fixture(name);
    let projects = home.root.join("projects");
    fs::write(
        home.root.join("snapshot.json"),
        FIXTURE.replace(
            "\"/fm/projects\"",
            &format!("{:?}", projects.display().to_string()),
        ),
    )
    .unwrap();
    let repo = projects.join("Shop");
    fs::create_dir_all(&repo).unwrap();
    let mut t = 1_790_400_000;
    let mut g = |args: &[&str]| {
        t += 60;
        git(&repo, args, t);
    };
    g(&["init", "-q", "-b", "main"]);
    fs::write(repo.join("README"), "shop\n").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-q", "-m", "init"]);
    g(&["branch", "dev"]);
    g(&["branch", "staging"]);
    g(&["checkout", "-q", "dev"]);
    fs::write(repo.join("hook.yml"), "deploy\n").unwrap();
    g(&["add", "-A"]);
    g(&[
        "commit",
        "-q",
        "-m",
        "ci: trigger the staging deploy hook (#139)",
    ]);
    fs::write(repo.join("fee.txt"), "fee\n").unwrap();
    g(&["add", "-A"]);
    g(&[
        "commit",
        "-q",
        "-m",
        "feat(orders): delivery fee per customer (#140)",
    ]);
    g(&["checkout", "-q", "staging"]);
    g(&[
        "merge",
        "-q",
        "--no-ff",
        "dev",
        "-m",
        "Merge pull request #149 from acme/dev",
    ]);
    g(&["checkout", "-q", "main"]);
    g(&[
        "merge",
        "-q",
        "--no-ff",
        "staging",
        "-m",
        "Merge pull request #150 from acme/staging",
    ]);
    g(&["checkout", "-q", "dev"]);
    fs::create_dir_all(repo.join("supabase/migrations")).unwrap();
    fs::write(repo.join("supabase/migrations/001_carts.sql"), "create;\n").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-q", "-m", "feat: saved carts (#141)"]);
    g(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/shop.git",
    ]);
    home
}

#[test]
fn releases_come_from_the_project_repository() {
    let home = home_with_shop_repo("releases");
    let out = kanbr(&["print", "--tab", "Shop"], Some(&home.root));
    let text = stdout(&out);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let live = text.split("\nLive (").nth(1).unwrap_or_default();
    assert!(live.contains("#150 · 2 changes"), "{text}");
    assert!(
        live.contains("Shop #139 · Trigger staging deploy hook (shop-deploy-hook)"),
        "{text}"
    );
    assert!(live.contains("[no card] Shop #140"), "{text}");
    let dev = text.split("\nDev (").nth(1).unwrap_or_default();
    assert!(dev.starts_with("1)"), "{text}");
    assert!(
        dev.contains("[no card] Shop #141 · feat: saved carts"),
        "{text}"
    );
    assert!(
        text.contains("Promotion PR  https://github.com/acme/shop/pull/150"),
        "{text}"
    );

    let notes = stdout(&kanbr(&["notes", "--tab", "Shop"], Some(&home.root)));
    assert!(
        notes.contains("\nNew\n- Delivery fee per customer\n"),
        "{notes}"
    );
    assert!(
        notes.contains("Behind the scenes: 1 change (ci)."),
        "{notes}"
    );

    let doctor = stdout(&kanbr(&["doctor"], Some(&home.root)));
    assert!(
        doctor.contains("project git       Shop: dev staging main; latest release"),
        "{doctor}"
    );
}

#[test]
fn never_writes_into_a_project_repository() {
    let home = home_with_shop_repo("repo-readonly");
    let before = listing(&home.root);
    for args in [&["print"][..], &["notes"], &["doctor"]] {
        kanbr(args, Some(&home.root));
    }
    assert_eq!(before, listing(&home.root));
}
