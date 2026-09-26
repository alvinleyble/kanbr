//! Release tracking against throwaway git repositories built in each test:
//! merge-commit, squash, and rebase promotions, back-merges, hotfixes, and
//! the release boundary.

use super::*;
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Repo {
    dir: PathBuf,
    clock: Cell<i64>,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let dir = std::env::temp_dir().join(format!("kanbr-rel-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let r = Repo {
            dir,
            clock: Cell::new(1_790_000_000),
        };
        r.git(&["init", "-q", "-b", "main"]);
        r.commit("README", "hello\n", "init");
        r
    }

    fn git_at(&self, args: &[&str], time: i64) -> String {
        let date = format!("@{time} +0000");
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
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
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        let t = self.clock.get() + 60;
        self.clock.set(t);
        self.git_at(args, t)
    }

    fn write(&self, file: &str, content: &str) {
        let path = self.dir.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn commit(&self, file: &str, content: &str, msg: &str) -> String {
        self.write(file, content);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
        self.sha("HEAD")
    }

    fn sha(&self, rev: &str) -> String {
        self.git_at(&["rev-parse", rev], 0)
    }

    fn checkout(&self, branch: &str) {
        self.git(&["checkout", "-q", branch]);
    }

    fn branch_from(&self, name: &str, from: &str) {
        self.git(&["branch", name, from]);
    }

    /// A feature branch off `base` with one commit per file, merged into
    /// `base` with a GitHub merge commit for PR `n`.
    fn merge_pr(&self, base: &str, n: u64, title: &str, files: &[&str]) -> String {
        let feature = format!("feat-{n}");
        self.checkout(base);
        self.git(&["checkout", "-q", "-b", &feature]);
        for f in files {
            self.commit(f, &format!("{f} from #{n}\n"), &format!("work on {f}"));
        }
        self.checkout(base);
        self.git(&[
            "merge",
            "-q",
            "--no-ff",
            &feature,
            "-m",
            &format!("Merge pull request #{n} from o/{feature}"),
            "-m",
            title,
        ]);
        self.sha("HEAD")
    }

    /// PR `n` squash-merged into `base` as one commit.
    fn squash_pr(&self, base: &str, n: u64, title: &str, file: &str) -> String {
        self.checkout(base);
        self.commit(
            file,
            &format!("{file} from #{n}\n"),
            &format!("{title} (#{n})"),
        )
    }

    /// Promotes `from` into `to` with a GitHub merge commit (PR `n`).
    fn merge_promote(&self, from: &str, to: &str, n: u64) -> String {
        self.checkout(to);
        self.git(&[
            "merge",
            "-q",
            "--no-ff",
            from,
            "-m",
            &format!("Merge pull request #{n} from o/{from}"),
            "-m",
            &format!("Promote {from} to {to}"),
        ]);
        self.sha("HEAD")
    }

    /// Promotes `from` into `to` as one squashed commit (PR `n`).
    fn squash_promote(&self, from: &str, to: &str, n: u64) -> String {
        self.checkout(to);
        self.git(&["merge", "-q", "--squash", from]);
        self.git(&[
            "commit",
            "-q",
            "-m",
            &format!("Promote {from} to {to} (#{n})"),
        ]);
        self.sha("HEAD")
    }

    /// Promotes `from` into `to` the way a GitHub rebase merge does: every
    /// commit `to` lacks is replayed, with one committer time for the run.
    fn rebase_promote(&self, from: &str, to: &str) {
        self.checkout(to);
        let list = self.git_at(
            &[
                "rev-list",
                "--reverse",
                "--no-merges",
                "--cherry-pick",
                "--right-only",
                &format!("{to}...{from}"),
            ],
            0,
        );
        let commits: Vec<&str> = list.lines().collect();
        let t = self.clock.get() + 60;
        self.clock.set(t);
        let mut args = vec!["cherry-pick", "--allow-empty"];
        args.extend(commits);
        self.git_at(&args, t);
    }

    fn analyze(&self) -> ProjectGit {
        self.analyze_with(["dev", "staging", "main"])
    }

    fn analyze_with(&self, branches: [&str; 3]) -> ProjectGit {
        analyze(&Git::new(&self.dir), branches, &mut Memo::default()).unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn change(pg: &ProjectGit, pr: u64) -> &Change {
    match pg.by_pr.get(&pr) {
        Some(Role::Change(i)) => &pg.changes[*i],
        other => panic!("PR #{pr} is {other:?}, not a change"),
    }
}

fn change_titled<'a>(pg: &'a ProjectGit, title: &str) -> &'a Change {
    pg.changes
        .iter()
        .find(|c| c.title == title)
        .unwrap_or_else(|| panic!("no change {title}: {:?}", pg.changes))
}

fn three_lanes(name: &str) -> Repo {
    let r = Repo::new(name);
    r.branch_from("dev", "main");
    r.branch_from("staging", "main");
    r
}

#[test]
fn merge_commit_promotions_place_each_change_in_its_furthest_lane() {
    let r = three_lanes("merge");
    r.merge_pr("dev", 1, "Add the order list", &["a1", "a2"]);
    r.squash_pr("dev", 2, "fix(orders): stop double saves", "b");
    r.checkout("dev");
    let direct = r.commit("c", "c\n", "chore: tidy config");
    r.merge_promote("dev", "staging", 10);
    r.merge_pr("dev", 3, "Add receipts", &["d"]);
    r.merge_promote("staging", "main", 11);
    r.merge_pr("dev", 4, "Add exports", &["e"]);

    let pg = r.analyze();
    assert_eq!(
        pg.lanes,
        Lanes {
            dev: true,
            staging: true,
            live: true
        }
    );
    for pr in [1, 2] {
        let c = change(&pg, pr);
        assert_eq!(c.column, Column::Live, "#{pr}");
        assert_eq!(c.home, Column::Dev);
        assert!(c.in_release, "#{pr}");
    }
    assert_eq!(change(&pg, 1).title, "Add the order list");
    assert_eq!(change(&pg, 2).title, "fix(orders): stop double saves");
    let d = &pg.changes[match pg.by_commit[&direct] {
        Role::Change(i) => i,
        other => panic!("{other:?}"),
    }];
    assert_eq!((d.column, d.pr), (Column::Live, None));
    assert_eq!(d.title, "chore: tidy config");
    assert_eq!(change(&pg, 3).column, Column::Dev);
    assert_eq!(change(&pg, 4).column, Column::Dev);
    assert_eq!(pg.by_pr.get(&10), Some(&Role::Promotion));
    assert_eq!(pg.by_pr.get(&11), Some(&Role::Promotion));
    let rel = pg.release.as_ref().unwrap();
    assert_eq!(rel.pr, Some(11));
    assert_eq!(rel.commit, r.sha("main"));

    // Promoting again moves #3 to Staging, then a new release clears Live.
    r.merge_promote("dev", "staging", 12);
    let pg = r.analyze();
    assert_eq!(change(&pg, 3).column, Column::Staging);
    assert_eq!(change(&pg, 4).column, Column::Staging);
    r.merge_promote("staging", "main", 13);
    let pg = r.analyze();
    for pr in [3, 4] {
        assert_eq!(change(&pg, pr).column, Column::Live);
        assert!(change(&pg, pr).in_release, "#{pr}");
    }
    for pr in [1, 2] {
        assert!(!change(&pg, pr).in_release, "#{pr} was an earlier release");
    }
    assert_eq!(pg.release.unwrap().pr, Some(13));
}

#[test]
fn squash_promotions_are_found_by_content_even_after_a_hotfix() {
    let r = three_lanes("squash");
    r.squash_pr("dev", 1, "feat: one", "one");
    r.merge_pr("dev", 2, "Two", &["two"]);
    r.squash_promote("dev", "staging", 10);
    let pg = r.analyze();
    assert_eq!(change(&pg, 1).column, Column::Staging);
    assert_eq!(change(&pg, 2).column, Column::Staging);
    assert_eq!(pg.by_pr.get(&10), Some(&Role::Promotion));

    // A hotfix lands directly on main, so main and staging diverge before
    // the squash promotion: no tree matches, the file-by-file check must.
    r.squash_pr("main", 20, "fix: urgent", "hot");
    r.squash_promote("staging", "main", 11);
    r.squash_pr("dev", 3, "feat: three", "three");
    let pg = r.analyze();
    for pr in [1, 2] {
        assert_eq!(change(&pg, pr).column, Column::Live, "#{pr}");
        assert!(change(&pg, pr).in_release, "#{pr}");
    }
    let hot = change(&pg, 20);
    assert_eq!((hot.home, hot.column), (Column::Live, Column::Live));
    assert!(!hot.in_release, "the hotfix was its own, earlier release");
    assert_eq!(pg.by_pr.get(&11), Some(&Role::Promotion));
    assert_eq!(change(&pg, 3).column, Column::Dev);
    assert_eq!(pg.release.as_ref().unwrap().pr, Some(11));

    // The next squash round releases only #3; files the earlier squash
    // already brought no longer count.
    r.squash_promote("dev", "staging", 12);
    r.squash_promote("staging", "main", 13);
    let pg = r.analyze();
    assert!(change(&pg, 3).in_release);
    assert_eq!(change(&pg, 3).column, Column::Live);
    for pr in [1, 2, 20] {
        assert!(!change(&pg, pr).in_release, "#{pr}");
    }
    assert_eq!(pg.release.unwrap().pr, Some(13));
}

#[test]
fn rebase_promotions_are_found_by_tree_or_patch_id() {
    let r = three_lanes("rebase");
    r.squash_pr("dev", 1, "feat: one", "one");
    r.merge_pr("dev", 2, "Two", &["two-a", "two-b"]);
    r.rebase_promote("dev", "staging");
    let pg = r.analyze();
    assert_eq!(change(&pg, 1).column, Column::Staging, "same trees");
    assert_eq!(change(&pg, 2).column, Column::Staging);
    assert!(
        !pg.changes.iter().any(|c| c.home == Column::Staging),
        "rebased copies are not new changes: {:?}",
        pg.changes
    );

    // Staging gets its own commit, so the next rebase has different trees.
    r.checkout("staging");
    r.commit("staging.cfg", "x\n", "chore: staging-only setting");
    r.squash_pr("dev", 3, "feat: three", "three");
    r.merge_pr("dev", 4, "Four", &["four-a", "four-b"]);
    r.rebase_promote("dev", "staging");
    let pg = r.analyze();
    assert_eq!(change(&pg, 3).column, Column::Staging, "patch-id copy");
    assert_eq!(
        change(&pg, 4).column,
        Column::Staging,
        "every PR commit copied"
    );
    let own = change_titled(&pg, "chore: staging-only setting");
    assert_eq!((own.home, own.column), (Column::Staging, Column::Staging));

    r.merge_promote("staging", "main", 11);
    let pg = r.analyze();
    for pr in [1, 2, 3, 4] {
        assert_eq!(change(&pg, pr).column, Column::Live, "#{pr}");
        assert!(change(&pg, pr).in_release, "#{pr}");
    }
    assert!(change_titled(&pg, "chore: staging-only setting").in_release);
}

#[test]
fn a_cherry_picked_fix_moves_only_that_change() {
    let r = three_lanes("cherry");
    r.checkout("staging");
    r.commit("staging.cfg", "x\n", "chore: staging-only setting");
    r.merge_pr("dev", 5, "Five", &["five-a", "five-b"]);
    let six = r.squash_pr("dev", 6, "fix: six", "six");
    r.checkout("staging");
    r.git(&["cherry-pick", &six]);
    let pg = r.analyze();
    assert_eq!(change(&pg, 6).column, Column::Staging, "copied by patch id");
    assert_eq!(change(&pg, 5).column, Column::Dev, "not picked");
    assert!(
        !pg.changes
            .iter()
            .any(|c| c.title == "fix: six" && c.home == Column::Staging),
        "the copy is not a second change"
    );
}

#[test]
fn a_promotion_branch_merged_into_staging_is_a_promotion() {
    let r = three_lanes("promo-branch");
    r.checkout("staging");
    r.commit("staging.cfg", "x\n", "chore: staging-only setting");
    r.squash_pr("dev", 7, "feat: seven", "seven");
    let eight = r.squash_pr("dev", 8, "fix: eight", "eight");
    // A branch off staging carrying a pick of #8 only, merged as PR #132.
    r.branch_from("promote-eight", "staging");
    r.checkout("promote-eight");
    r.git(&["cherry-pick", &eight]);
    r.checkout("staging");
    r.git(&[
        "merge",
        "-q",
        "--no-ff",
        "promote-eight",
        "-m",
        "Merge pull request #132 from o/promote-eight",
        "-m",
        "promote: dev to staging - fix eight",
    ]);
    let pg = r.analyze();
    assert_eq!(pg.by_pr.get(&132), Some(&Role::Promotion));
    assert_eq!(change(&pg, 8).column, Column::Staging);
    assert_eq!(change(&pg, 7).column, Column::Dev);
}

#[test]
fn back_merges_are_not_changes_and_a_hotfix_is_its_own_release() {
    let r = Repo::new("backmerge");
    r.branch_from("dev", "main");
    r.merge_pr("dev", 1, "One", &["one"]);
    r.merge_promote("dev", "main", 10);
    r.checkout("main");
    r.commit("hot", "hot\n", "fix: urgent hotfix");
    r.checkout("dev");
    r.git(&[
        "merge",
        "-q",
        "--no-ff",
        "main",
        "-m",
        "Merge branch 'main' into dev",
    ]);
    let back = r.sha("dev");
    r.merge_pr("dev", 2, "Two", &["two"]);

    let pg = r.analyze();
    assert_eq!(pg.by_commit.get(&back), Some(&Role::BackMerge));
    let hot = change_titled(&pg, "fix: urgent hotfix");
    assert_eq!((hot.home, hot.column), (Column::Live, Column::Live));
    assert!(hot.in_release);
    assert!(!change(&pg, 1).in_release);
    assert_eq!(change(&pg, 1).column, Column::Live);
    assert_eq!(change(&pg, 2).column, Column::Dev);
    assert_eq!(
        pg.changes.iter().filter(|c| c.in_release).count(),
        1,
        "{:?}",
        pg.changes
    );
    let rel = pg.release.unwrap();
    assert_eq!((rel.pr, rel.title.as_str()), (None, "fix: urgent hotfix"));
}

#[test]
fn a_project_with_only_live_releases_each_landing() {
    let r = Repo::new("liveonly");
    r.squash_pr("main", 1, "feat: board", "board");
    r.squash_pr("main", 2, "feat: releases", "releases");
    let pg = r.analyze();
    assert_eq!(
        pg.lanes,
        Lanes {
            dev: false,
            staging: false,
            live: true
        }
    );
    assert!(change(&pg, 2).in_release);
    assert!(!change(&pg, 1).in_release);
    assert!(!change_titled(&pg, "init").in_release);
    assert_eq!(pg.release.unwrap().pr, Some(2));
}

#[test]
fn a_rebase_merge_that_lands_several_commits_is_one_release() {
    let r = Repo::new("rebaserun");
    r.branch_from("topic", "main");
    r.checkout("topic");
    r.commit("x", "x\n", "feat: part one");
    r.commit("y", "y\n", "feat: part two");
    r.checkout("main");
    r.commit("z", "z\n", "fix: earlier release");
    r.rebase_promote("topic", "main");
    let pg = r.analyze();
    let released: Vec<&str> = pg
        .changes
        .iter()
        .filter(|c| c.in_release)
        .map(|c| c.title.as_str())
        .collect();
    assert_eq!(released, vec!["feat: part two", "feat: part one"]);
}

#[test]
fn a_project_without_staging_goes_from_dev_to_live() {
    let r = Repo::new("devlive");
    r.branch_from("dev", "main");
    r.merge_pr("dev", 1, "One", &["one"]);
    r.merge_promote("dev", "main", 10);
    r.merge_pr("dev", 2, "Two", &["two"]);
    let pg = r.analyze();
    assert_eq!(
        pg.lanes,
        Lanes {
            dev: true,
            staging: false,
            live: true
        }
    );
    assert_eq!(change(&pg, 1).column, Column::Live);
    assert!(change(&pg, 1).in_release);
    assert_eq!(change(&pg, 2).column, Column::Dev);
    assert!(pg.migrations.is_empty());
}

#[test]
fn configured_branches_back_the_lanes() {
    let r = Repo::new("configured");
    r.branch_from("develop", "main");
    r.merge_pr("develop", 1, "One", &["one"]);
    let pg = r.analyze_with(["develop", "", "main"]);
    assert_eq!(
        pg.lanes,
        Lanes {
            dev: true,
            staging: false,
            live: true
        }
    );
    assert_eq!(pg.refs[0].branch, "develop");
    assert_eq!(change(&pg, 1).column, Column::Dev);
    let none = r.analyze_with(["", "", "production"]);
    assert_eq!(none.lanes, Lanes::default());
    assert!(none.changes.is_empty() && none.release.is_none());
}

#[test]
fn staging_preview_lists_database_changes_not_yet_live() {
    let r = three_lanes("migrations");
    r.checkout("dev");
    r.commit(
        "supabase/migrations/20260926_add_fee.sql",
        "alter table x;\n",
        "feat: delivery fee (#1)",
    );
    r.commit("server/db/migrate.js", "run()\n", "chore: runner (#2)");
    r.commit(
        "server/db/migrations/050_keys.sql",
        "create;\n",
        "feat: keys (#3)",
    );
    r.merge_promote("dev", "staging", 10);
    let pg = r.analyze();
    assert_eq!(
        pg.migrations,
        vec![
            "server/db/migrations/050_keys.sql".to_owned(),
            "supabase/migrations/20260926_add_fee.sql".to_owned()
        ]
    );
    r.merge_promote("staging", "main", 11);
    assert!(r.analyze().migrations.is_empty());
}

#[test]
fn app_version_comes_from_the_live_branch() {
    let r = Repo::new("version");
    r.commit(
        "package.json",
        r#"{"name":"x","private":true,"version":"0.1.0"}"#,
        "chore: app",
    );
    assert_eq!(
        r.analyze().release.unwrap().version,
        None,
        "private package"
    );
    r.commit(
        "client/android/app/build.gradle",
        "android {\n  defaultConfig {\n    versionCode 4\n    versionName \"1.2.1\"\n  }\n}\n",
        "chore: android",
    );
    assert_eq!(
        r.analyze().release.unwrap().version.as_deref(),
        Some("1.2.1")
    );

    let c = Repo::new("version-cargo");
    c.commit(
        "Cargo.toml",
        "[package]\nname = \"kanbr\"\nversion = \"0.2.0\"\n\n[dependencies]\nx = { version = \"1\" }\n",
        "chore: crate",
    );
    assert_eq!(
        c.analyze().release.unwrap().version.as_deref(),
        Some("0.2.0")
    );
}

#[test]
fn remote_tracking_branches_win_over_stale_local_ones() {
    let r = Repo::new("tracking");
    r.branch_from("dev", "main");
    let stale = r.sha("dev");
    r.merge_pr("dev", 1, "One", &["one"]);
    let fresh = r.sha("dev");
    r.git(&["update-ref", "refs/remotes/origin/dev", &fresh]);
    r.git(&["update-ref", "refs/heads/dev", &stale]);
    r.git(&["remote", "add", "origin", "git@github.com:acme/shop.git"]);
    let pg = r.analyze();
    assert_eq!(pg.refs[0].refname, "refs/remotes/origin/dev");
    assert_eq!(change(&pg, 1).column, Column::Dev);
    assert_eq!(pg.web.as_deref(), Some("https://github.com/acme/shop"));
    assert_eq!(
        pg.pr_url(1).as_deref(),
        Some("https://github.com/acme/shop/pull/1")
    );
}

#[test]
fn analysis_never_writes_to_the_repository() {
    let r = three_lanes("readonly");
    r.squash_pr("dev", 1, "feat: one", "one");
    r.squash_promote("dev", "staging", 10);
    r.squash_pr("main", 20, "fix: hot", "hot");
    r.squash_promote("staging", "main", 11);
    r.checkout("staging");
    r.commit("s", "s\n", "chore: staging only");
    r.squash_pr("dev", 2, "feat: two", "two");
    r.rebase_promote("dev", "staging");
    let before = listing(&r.dir.join(".git"));
    let pg = r.analyze();
    assert_eq!(change(&pg, 1).column, Column::Live);
    assert_eq!(change(&pg, 2).column, Column::Staging);
    assert_eq!(before, listing(&r.dir.join(".git")));
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
                out.push((p, e.metadata().map(|m| m.len()).unwrap_or(0)));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn visibility_keeps_pending_and_released_changes() {
    let pg = ProjectGit {
        refs: vec![LaneRef {
            column: Column::Live,
            branch: "main".into(),
            refname: "refs/heads/main".into(),
            tip: "x".into(),
        }],
        ..ProjectGit::default()
    };
    let mut c = Change {
        commit: "x".into(),
        home: Column::Live,
        column: Column::Live,
        in_release: false,
        pr: None,
        title: "t".into(),
        time: 0,
    };
    assert!(!pg.visible(&c, true));
    c.in_release = true;
    assert!(pg.visible(&c, false));
    c.column = Column::Dev;
    assert!(pg.visible(&c, false));
}

#[test]
fn version_parsers() {
    assert_eq!(
        android_version("versionName = \"2.0\"\n").as_deref(),
        Some("2.0")
    );
    assert_eq!(android_version("versionName '3.1'").as_deref(), Some("3.1"));
    assert_eq!(android_version("versionNameSuffix \"-x\""), None);
    assert_eq!(
        toml_version(
            "[tool.poetry]\nversion = \"1.0\"\n[project]\nversion = \"2.0\"\n",
            &["project", "tool.poetry"]
        )
        .as_deref(),
        Some("2.0")
    );
    assert_eq!(
        toml_version("[package]\nversion.workspace = true\n", &["package"]),
        None
    );
}
