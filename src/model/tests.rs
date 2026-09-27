use super::*;
use crate::dates::parse_date;
use crate::releases::{Change, LaneRef, Release};
use std::collections::HashMap;

const FIXTURE: &str = include_str!("../../tests/fixtures/snapshot.json");

pub(crate) struct FakeEnv {
    pub metas: HashMap<PathBuf, Meta>,
    pub gits: HashMap<PathBuf, ProjectGit>,
    /// PR URL -> the commit the forge says it merged as.
    pub forge: HashMap<String, String>,
}

impl Env for FakeEnv {
    fn meta(&self, path: &Path) -> Option<Meta> {
        self.metas.get(path).cloned()
    }
    fn git(&self, repo: &Path) -> Option<Result<Arc<ProjectGit>, String>> {
        self.gits.get(repo).map(|g| Ok(Arc::new(g.clone())))
    }
    fn merged_commit(&self, pr_url: &str) -> Option<String> {
        self.forge.get(pr_url).cloned()
    }
}

/// A repository with the given lanes and no history.
pub(crate) fn lanes_only(lanes: Lanes) -> ProjectGit {
    let refs = [
        (lanes.dev, Column::Dev, "dev"),
        (lanes.staging, Column::Staging, "staging"),
        (lanes.live, Column::Live, "main"),
    ]
    .into_iter()
    .filter(|(used, _, _)| *used)
    .map(|(_, column, b)| LaneRef {
        column,
        branch: b.to_owned(),
        refname: format!("refs/remotes/origin/{b}"),
        tip: format!("{b}-tip"),
    })
    .collect();
    ProjectGit {
        lanes,
        refs,
        ..ProjectGit::default()
    }
}

pub(crate) fn fake_env() -> FakeEnv {
    let mut metas = HashMap::new();
    metas.insert(
        PathBuf::from("/fm/state/kanbr-1-board.meta"),
        crate::firstmate::parse_meta("model=claude-opus-5-5\neffort=xhigh\nharness=claude\n"),
    );
    metas.insert(
        PathBuf::from("/mates/alpha/state/site-contact-form.meta"),
        crate::firstmate::parse_meta("model=gpt-5.5\nharness=codex\nspawn_gen=s1790420000.1.1\n"),
    );
    let mut gits = HashMap::new();
    gits.insert(
        PathBuf::from("/fm"),
        lanes_only(Lanes {
            dev: false,
            staging: false,
            live: true,
        }),
    );
    gits.insert(
        PathBuf::from("/fm/projects/Shop"),
        lanes_only(Lanes {
            dev: true,
            staging: true,
            live: true,
        }),
    );
    gits.insert(
        PathBuf::from("/fm/projects/Site"),
        lanes_only(Lanes {
            dev: true,
            staging: false,
            live: true,
        }),
    );
    gits.insert(
        PathBuf::from("/fm/projects/tool"),
        lanes_only(Lanes::default()),
    );
    FakeEnv {
        metas,
        gits,
        forge: HashMap::new(),
    }
}

fn shop_change(n: u64, title: &str, column: Column, in_release: bool) -> Change {
    Change {
        commit: format!("c{n:0>39}"),
        home: Column::Dev,
        column,
        in_release,
        pr: Some(n),
        title: title.to_owned(),
        time: parse_date("2026-09-24T10:00:00Z").unwrap(),
    }
}

/// Shop's history: a release (#150) of four changes, one of them the
/// Firstmate card shop-deploy-hook (#139), one change waiting in Staging, and
/// an earlier release.
pub(crate) fn shop_git() -> ProjectGit {
    let mut g = lanes_only(Lanes {
        dev: true,
        staging: true,
        live: true,
    });
    g.web = Some("https://github.com/acme/shop".into());
    g.changes = vec![
        shop_change(
            139,
            "fix: stop double saves on slow networks",
            Column::Live,
            true,
        ),
        shop_change(
            141,
            "feat(orders): persistent delivery fee per customer",
            Column::Live,
            true,
        ),
        shop_change(
            142,
            "Receipt printing waits for the printer",
            Column::Live,
            true,
        ),
        shop_change(143, "docs: explain the fee", Column::Live, true),
        shop_change(144, "feat: saved carts", Column::Staging, false),
        shop_change(100, "Old release", Column::Live, false),
        shop_change(1, "Old finished work", Column::Live, false),
    ];
    for (i, c) in g.changes.iter().enumerate() {
        g.by_pr.insert(c.pr.unwrap(), Role::Change(i));
        g.by_commit.insert(c.commit.clone(), Role::Change(i));
    }
    g.promotions = vec![
        Change {
            commit: "r".repeat(40),
            home: Column::Live,
            column: Column::Live,
            in_release: true,
            pr: Some(150),
            title: "Promote staging to main".into(),
            time: parse_date("2026-09-25T10:00:00Z").unwrap(),
        },
        Change {
            commit: format!("p{:0>39}", 148),
            home: Column::Staging,
            column: Column::Staging,
            in_release: false,
            pr: Some(148),
            title: "Promote dev to staging".into(),
            time: parse_date("2026-09-26T10:00:00Z").unwrap(),
        },
        Change {
            commit: format!("p{:0>39}", 120),
            home: Column::Live,
            column: Column::Live,
            in_release: false,
            pr: Some(120),
            title: "Promote staging to main".into(),
            time: parse_date("2026-09-20T10:00:00Z").unwrap(),
        },
    ];
    for (i, p) in g.promotions.iter().enumerate() {
        g.by_pr.insert(p.pr.unwrap(), Role::Promotion(i));
        g.by_commit.insert(p.commit.clone(), Role::Promotion(i));
    }
    g.release = Some(Release {
        commit: "r".repeat(40),
        time: parse_date("2026-09-25T10:00:00Z").unwrap(),
        pr: Some(150),
        title: "Promote staging to main".into(),
        version: Some("1.2.1".into()),
        fast_forward: false,
        known: true,
    });
    g.migrations = vec!["supabase/migrations/20260926_fee.sql".into()];
    g.fetched = Some(parse_date("2026-09-26T09:00:00Z").unwrap());
    g
}

pub(crate) fn release_env() -> FakeEnv {
    let mut env = fake_env();
    env.gits
        .insert(PathBuf::from("/fm/projects/Shop"), shop_git());
    env
}

pub(crate) fn release_board() -> Board {
    board_with(&serde_json::from_str(FIXTURE).unwrap(), &release_env())
}

pub(crate) fn registry() -> Vec<String> {
    ["Shop", "Site", "tool", "legacy", "legacy-archived", "kanbr"]
        .into_iter()
        .map(String::from)
        .collect()
}

pub(crate) fn now() -> i64 {
    parse_date("2026-09-26T12:00:00Z").unwrap()
}

pub(crate) fn fixture_board() -> Board {
    board_from(&serde_json::from_str(FIXTURE).unwrap())
}

fn board_from(snap: &Value) -> Board {
    board_with(snap, &fake_env())
}

fn board_with(snap: &Value, env: &FakeEnv) -> Board {
    let config = Config::default();
    let registry = registry();
    let ctx = Context {
        registry: &registry,
        config: &config,
        now: now(),
        env,
    };
    build_board(snap, &ctx)
}

fn card<'a>(b: &'a Board, id: &str) -> &'a Card {
    b.cards
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no card {id}"))
}

fn has(b: &Board, id: &str) -> bool {
    b.cards.iter().any(|c| c.id == id)
}

#[test]
fn queued_unheld_is_ready() {
    let b = fixture_board();
    let c = card(&b, "fm-sync-guard");
    assert_eq!(c.column, Column::Ready);
    assert_eq!(c.project, "firstmate");
    assert_eq!(c.state, "queued");
    assert!(!c.decision && !c.grill && !c.paused);
}

#[test]
fn blocked_queued_stays_ready_with_dependency_badge() {
    let b = fixture_board();
    let c = card(&b, "kanbr-2-releases");
    assert_eq!(c.column, Column::Ready);
    assert_eq!(c.blocked_by, vec!["kanbr-1-board"]);
    assert_eq!(c.state, "after kanbr-1-board");
}

#[test]
fn captain_decision_is_booked_with_red_badge_and_grill_tag() {
    let b = fixture_board();
    let c = card(&b, "shop-print-queue");
    assert_eq!(c.column, Column::Booked);
    assert!(c.decision);
    assert!(c.grill);
    assert_eq!(c.project, "Shop");
    assert_eq!(c.state, "your call");
}

#[test]
fn halted_project_cards_are_paused_and_greyed() {
    let b = fixture_board();
    for id in ["legacy-capture-fix", "legacy-archive-repo"] {
        let c = card(&b, id);
        assert_eq!(c.column, Column::Booked, "{id}");
        assert!(c.paused, "{id}");
        assert_eq!(c.state, "paused");
        assert_eq!(
            c.project, "legacy",
            "{id} resolves through repo path or id prefix"
        );
    }
    let tab = b.projects.iter().find(|p| p.name == "legacy").unwrap();
    assert!(tab.halted);
    assert_eq!(
        b.projects.last().unwrap().name,
        "legacy",
        "halted tabs sort last"
    );
}

#[test]
fn future_hold_is_booked_and_expired_hold_is_ready() {
    let b = fixture_board();
    let future = card(&b, "site-launch-post");
    assert_eq!(future.column, Column::Booked);
    assert_eq!(future.state, "until 10-10");
    let expired = card(&b, "site-expired-hold");
    assert_eq!(expired.column, Column::Ready);
}

#[test]
fn project_names_from_repo_paths_and_urls() {
    let env = fake_env();
    let config = Config::default();
    let registry = registry();
    let ctx = Context {
        registry: &registry,
        config: &config,
        now: now(),
        env: &env,
    };
    let snap: Value = serde_json::from_str(FIXTURE).unwrap();
    let b = Builder::new(&snap, &ctx);
    assert_eq!(b.project(Some("/fm"), "x"), "firstmate");
    assert_eq!(b.project(Some("/fm/"), "x"), "firstmate");
    assert_eq!(b.project(Some("projects/shop"), "x"), "Shop");
    assert_eq!(
        b.project(Some("https://github.com/acme/site.git"), "x"),
        "Site"
    );
    assert_eq!(
        b.project(Some("unregistered-thing"), "x"),
        "unregistered-thing"
    );
    assert_eq!(
        b.project(None, "legacy-archived-cleanup"),
        "legacy-archived"
    );
    assert_eq!(b.project(None, "fm-anything"), "firstmate");
    assert_eq!(b.project(None, "zzz-unknown"), "other");
}

#[test]
fn project_inferred_from_id_prefix_when_repo_missing() {
    let b = fixture_board();
    assert_eq!(card(&b, "shop-promote-main").project, "Shop");
}

#[test]
fn in_flight_worker_is_building_with_model_and_elapsed() {
    let b = fixture_board();
    let c = card(&b, "kanbr-1-board");
    assert_eq!(c.column, Column::Building);
    assert_eq!(c.model.as_deref(), Some("opus-5-5"));
    assert_eq!(c.state, "working");
    assert_eq!(c.tone, Tone::Active);
    assert!(!c.since_is_date);
    assert_eq!(
        c.elapsed(now()),
        Some(format_elapsed(now() - 1_790_424_000))
    );
}

#[test]
fn held_in_flight_without_live_worker_is_booked() {
    let b = fixture_board();
    let c = card(&b, "shop-held-work");
    assert_eq!(c.column, Column::Booked);
    assert_eq!(c.state, "external hold");
}

#[test]
fn programs_are_not_cards() {
    assert!(!has(&fixture_board(), "fm-monthly-program"));
}

#[test]
fn orphan_task_with_null_fields_is_building() {
    let b = fixture_board();
    let c = card(&b, "shop-force-update");
    assert_eq!(c.column, Column::Building);
    assert_eq!(c.project, "Shop");
    assert_eq!(c.state, "unknown");
    assert_eq!(c.model.as_deref(), Some("claude"));
    assert!(c.since.is_none());
}

#[test]
fn second_mate_tasks_are_not_cards() {
    assert!(!has(&fixture_board(), "sm-alpha"));
}

#[test]
fn merged_work_lands_in_first_lane_the_project_uses() {
    let b = fixture_board();
    let shop = card(&b, "shop-deploy-hook");
    assert_eq!(shop.column, Column::Dev);
    assert_eq!(shop.pr_number, Some(139));
    assert_eq!(card(&b, "site-hero-copy").column, Column::Dev);
    assert_eq!(
        card(&b, "fm-upstream-sync").column,
        Column::Live,
        "firstmate has no dev or staging"
    );
}

#[test]
fn project_missing_every_configured_branch_shows_no_finished_cards() {
    let b = fixture_board();
    assert!(!has(&b, "tool-readme"));
    assert!(
        b.notices
            .iter()
            .any(|n| n.contains("1 finished card(s) not shown: tool")),
        "{:?}",
        b.notices
    );
}

#[test]
fn unreadable_repository_assumes_only_the_last_lane() {
    let b = fixture_board();
    assert_eq!(card(&b, "kanbr-0-design").column, Column::Live);
}

#[test]
fn configured_branches_move_landing_lane() {
    let snap: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut env = fake_env();
    env.gits.insert(
        PathBuf::from("/fm/projects/Shop"),
        lanes_only(Lanes {
            dev: false,
            staging: true,
            live: true,
        }),
    );
    let config = Config {
        dev_branch: String::new(),
        ..Config::default()
    };
    let registry = registry();
    let ctx = Context {
        registry: &registry,
        config: &config,
        now: now(),
        env: &env,
    };
    let b = build_board(&snap, &ctx);
    assert_eq!(card(&b, "shop-deploy-hook").column, Column::Staging);
}

#[test]
fn finished_without_pr_is_live_and_old_work_drops_off() {
    let b = fixture_board();
    let c = card(&b, "shop-worth-fixing");
    assert_eq!(c.column, Column::Live);
    assert_eq!(c.state, "reported");
    assert!(!has(&b, "shop-ancient"));
}

#[test]
fn second_mate_work_mixes_into_project_columns_with_tag() {
    let b = fixture_board();
    let child = card(&b, "site-contact-form");
    assert_eq!(child.column, Column::Building);
    assert_eq!(child.owner, Owner::Secondmate("sm-alpha".into()));
    assert_eq!(child.project, "Site");
    assert_eq!(child.model.as_deref(), Some("gpt-5.5"));
    assert!(child.is_secondmate());

    let flagged = card(&b, "tool-cli-flags");
    assert_eq!(flagged.state, "needs decision");
    assert!(flagged.decision);
    assert_eq!(flagged.column, Column::Building);
    assert_eq!(flagged.project, "tool");

    let queued = card(&b, "site-dark-mode");
    assert_eq!(queued.column, Column::Ready);
    assert_eq!(queued.blocked_by, vec!["site-contact-form"]);

    let decision = card(&b, "site-pricing-page");
    assert_eq!(decision.column, Column::Booked);
    assert!(decision.decision);

    let landed = card(&b, "site-footer");
    assert_eq!(landed.column, Column::Dev);
    assert!(landed.is_secondmate());
}

#[test]
fn waiting_lists_every_captain_decision() {
    let b = fixture_board();
    let ids: Vec<&str> = b.waiting().iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), 4);
    for id in [
        "shop-print-queue",
        "shop-promote-main",
        "site-pricing-page",
        "tool-cli-flags",
    ] {
        assert!(ids.contains(&id), "{id}");
    }
}

#[test]
fn decisions_sort_first_and_paused_last_in_booked() {
    let b = fixture_board();
    let booked = b.column_cards(Column::Booked, None);
    assert!(booked.first().unwrap().decision);
    assert!(booked.last().unwrap().paused);
}

#[test]
fn tabs_count_cards_per_project() {
    let b = fixture_board();
    let total: usize = b.projects.iter().map(|p| p.count).sum();
    assert_eq!(total, b.cards.len());
    let shop = b.projects.iter().find(|p| p.name == "Shop").unwrap();
    assert_eq!(
        shop.count,
        b.cards.iter().filter(|c| c.project == "Shop").count()
    );
    assert!(b.projects.iter().all(|p| p.count > 0));
}

#[test]
fn notices_disclose_what_is_missing() {
    let b = fixture_board();
    let all = b.notices.join("\n");
    assert!(all.contains("free-form"), "{all}");
    assert!(all.contains("sm-beta"), "{all}");
    assert!(all.contains("3 queued omitted"), "{all}");
}

#[test]
fn empty_or_garbage_snapshot_yields_empty_board() {
    let env = fake_env();
    let config = Config::default();
    let registry = registry();
    let ctx = Context {
        registry: &registry,
        config: &config,
        now: now(),
        env: &env,
    };
    for text in [
        "{}",
        "null",
        "[]",
        r#"{"backlog":{"records":null},"tasks":null,"secondmate_current":null}"#,
    ] {
        let snap: Value = serde_json::from_str(text).unwrap();
        let b = build_board(&snap, &ctx);
        assert!(b.cards.is_empty(), "{text}");
    }
}

#[test]
fn configurable_words_change_grill_and_halted() {
    let snap: Value = serde_json::from_str(FIXTURE).unwrap();
    let env = fake_env();
    let config = Config {
        grill_words: vec!["workshop".into()],
        halted_words: vec!["frozen".into()],
        ..Config::default()
    };
    let registry = registry();
    let ctx = Context {
        registry: &registry,
        config: &config,
        now: now(),
        env: &env,
    };
    let b = build_board(&snap, &ctx);
    assert!(!card(&b, "shop-print-queue").grill);
    assert!(!card(&b, "legacy-capture-fix").paused);
}

#[test]
fn short_model_labels() {
    assert_eq!(
        short_model(Some("claude-opus-5-5"), Some("claude")).as_deref(),
        Some("opus-5-5")
    );
    assert_eq!(
        short_model(Some("default"), Some("claude")).as_deref(),
        Some("claude")
    );
    assert_eq!(
        short_model(Some("openai/gpt-5.5"), None).as_deref(),
        Some("gpt-5.5")
    );
    assert_eq!(short_model(None, None), None);
}

#[test]
fn pr_numbers_parse_from_urls() {
    assert_eq!(pr_number("https://github.com/o/r/pull/42"), Some(42));
    assert_eq!(pr_number("https://github.com/o/r/pull/42/files"), Some(42));
    assert_eq!(pr_number("https://example.com/"), None);
}

#[test]
fn main_worker_pending_decision_waits_on_captain_in_building() {
    let mut snap: Value = serde_json::from_str(FIXTURE).unwrap();
    snap["tasks"][0]["hints"]["pending_decision"] = Value::Bool(true);
    let b = board_from(&snap);
    let c = card(&b, "kanbr-1-board");
    assert_eq!(c.column, Column::Building);
    assert_eq!(c.state, "needs decision");
    assert!(c.decision);
    assert!(b.waiting().iter().any(|w| w.id == "kanbr-1-board"));
}

#[test]
fn a_second_mate_call_is_answered_in_its_own_home_or_not_in_place() {
    let b = fixture_board();
    assert_eq!(
        card(&b, "site-pricing-page").ask,
        Some(Ask::Hold {
            home: Some(PathBuf::from("/mates/alpha")),
            work_item: true,
        })
    );

    let mut snap: Value = serde_json::from_str(FIXTURE).unwrap();
    snap["secondmate_current"]["records"][0]["home"] = Value::Null;
    let b = board_from(&snap);
    let c = card(&b, "site-pricing-page");
    assert!(c.decision);
    assert_eq!(c.ask, None);
}

#[test]
fn merged_cards_sit_in_the_lane_their_change_reached() {
    let b = release_board();
    let c = card(&b, "shop-deploy-hook");
    assert_eq!(c.column, Column::Live);
    assert!(c.in_release && !c.outside);
    assert_eq!(
        c.change_title.as_deref(),
        Some("fix: stop double saves on slow networks")
    );
    assert!(
        c.details
            .iter()
            .any(|(k, v)| *k == "Landed" && v == "c000000 on dev"),
        "{:?}",
        c.details
    );
    assert!(
        !has(&b, "shop-ancient"),
        "released before the latest release"
    );
    assert!(
        !b.notices.iter().any(|n| n.contains("shop-deploy-hook")),
        "{:?}",
        b.notices
    );
}

#[test]
fn changes_without_a_firstmate_card_are_plain_grey_cards() {
    let b = release_board();
    let c = card(&b, "Shop#141");
    assert!(c.outside);
    assert_eq!(c.tone, Tone::Muted);
    assert_eq!(
        c.title,
        "feat(orders): persistent delivery fee per customer"
    );
    assert_eq!(c.column, Column::Live);
    assert!(c.in_release);
    assert_eq!(
        c.pr_url.as_deref(),
        Some("https://github.com/acme/shop/pull/141")
    );
    assert_eq!(card(&b, "Shop#144").column, Column::Staging);
    assert!(!has(&b, "Shop#100"), "an earlier release is cleared");
    assert!(!has(&b, "Shop#139"), "tracked by shop-deploy-hook");
    assert!(!has(&b, "Shop#1"), "released long ago");
}

#[test]
fn live_holds_only_the_latest_release() {
    let b = release_board();
    let ids: Vec<&str> = b
        .release_cards(Some("Shop"))
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(ids.len(), 4, "{ids:?}");
    let live = b.column_cards(Column::Live, Some("Shop"));
    assert!(live.first().unwrap().in_release, "release cards come first");
    let rel = b.release("Shop").unwrap();
    let live_rel = rel.live.as_ref().unwrap();
    assert_eq!(live_rel.pr, Some(150));
    assert_eq!(
        live_rel.pr_url.as_deref(),
        Some("https://github.com/acme/shop/pull/150")
    );
    assert_eq!(live_rel.version.as_deref(), Some("1.2.1"));
    assert_eq!(rel.refs.last().unwrap().1, "origin/main");
}

/// The fixture with a finished Shop card per PR URL, each titled by its id.
fn with_shop_cards(prs: &[(&str, &str)]) -> Value {
    let mut snap: Value = serde_json::from_str(FIXTURE).unwrap();
    let base = snap["backlog"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "shop-deploy-hook")
        .unwrap()
        .clone();
    for (id, url) in prs {
        let mut rec = base.clone();
        rec["id"] = (*id).into();
        rec["title"] = (*id).into();
        rec["kind"] = "ship".into();
        rec["completion"] = serde_json::json!({"verb": "done", "date": "2026-09-26"});
        rec["pr_url"] = (*url).into();
        snap["backlog"]["records"].as_array_mut().unwrap().push(rec);
    }
    snap
}

#[test]
fn promotion_cards_sit_in_the_lane_their_promotion_reached() {
    let snap = with_shop_cards(&[
        (
            "shop-promote-staging",
            "https://github.com/acme/shop/pull/148",
        ),
        (
            "shop-promote-main-done",
            "https://github.com/acme/shop/pull/150",
        ),
        ("shop-promote-old", "https://github.com/acme/shop/pull/120"),
    ]);
    let b = board_with(&snap, &release_env());

    let staged = card(&b, "shop-promote-staging");
    assert_eq!(staged.column, Column::Staging);
    assert!(staged.promotion && !staged.not_release && !staged.in_release);
    assert_eq!(staged.state, "promoted");
    assert_eq!(staged.tone, Tone::Finished);
    assert_eq!(staged.pr_number, Some(148));
    assert!(
        staged
            .details
            .iter()
            .any(|(k, v)| *k == "Promotion" && v == "reached Staging"),
        "{:?}",
        staged.details
    );

    let released = card(&b, "shop-promote-main-done");
    assert_eq!(released.column, Column::Live);
    assert!(released.promotion && released.in_release);
    assert_eq!(
        b.release_cards(Some("Shop")).len(),
        4,
        "a promotion is not one of the release's changes"
    );

    assert!(
        !has(&b, "shop-promote-old"),
        "promoted to Live before the latest release"
    );
}

#[test]
fn a_back_merge_pr_is_not_a_card() {
    let mut env = release_env();
    let shop = env.gits.get_mut(Path::new("/fm/projects/Shop")).unwrap();
    shop.by_pr.insert(151, Role::BackMerge);
    let snap = with_shop_cards(&[("shop-back-merge", "https://github.com/acme/shop/pull/151")]);
    let b = board_with(&snap, &env);
    assert!(!has(&b, "shop-back-merge"));
}

#[test]
fn unmerged_finished_work_stays_in_live_until_the_next_release() {
    let b = release_board();
    let c = card(&b, "shop-worth-fixing");
    assert_eq!(c.column, Column::Live);
    assert!(!c.in_release, "a report is not part of the release");
    assert!(c.not_release && !c.promotion);
    assert_eq!(c.tone, Tone::Muted);
    let live = b.column_cards(Column::Live, Some("Shop"));
    assert!(
        live.last().unwrap().not_release,
        "work that is not a release sits below the release"
    );
    assert_eq!(live.iter().filter(|c| c.not_release).count(), 1);
    let mut env = release_env();
    let shop = env.gits.get_mut(Path::new("/fm/projects/Shop")).unwrap();
    shop.release.as_mut().unwrap().time = parse_date("2026-09-26T08:00:00Z").unwrap();
    let b = board_with(&serde_json::from_str(FIXTURE).unwrap(), &env);
    assert!(!has(&b, "shop-worth-fixing"), "a newer release clears it");
}

#[test]
fn the_forge_finds_a_pr_the_merge_messages_do_not_name() {
    let mut snap: Value = serde_json::from_str(FIXTURE).unwrap();
    for r in snap["backlog"]["records"].as_array_mut().unwrap() {
        if r["id"] == "shop-deploy-hook" {
            r["pr_url"] = "https://github.com/acme/shop/pull/160".into();
        }
    }
    let mut env = release_env();
    env.forge.insert(
        "https://github.com/acme/shop/pull/160".into(),
        format!("c{:0>39}", 144),
    );
    let b = board_with(&snap, &env);
    let c = card(&b, "shop-deploy-hook");
    assert_eq!(c.column, Column::Staging);
    assert!(!has(&b, "Shop#144"), "the change is the Firstmate card");
}

#[test]
fn a_pr_missing_from_git_falls_back_and_says_so() {
    let b = fixture_board();
    let c = card(&b, "shop-deploy-hook");
    assert_eq!(c.column, Column::Dev);
    assert!(
        c.details.iter().any(|(k, _)| *k == "Git"),
        "{:?}",
        c.details
    );
    assert!(
        b.notices
            .iter()
            .any(|n| n.contains("not found in their project's git history")
                && n.contains("shop-deploy-hook")),
        "{:?}",
        b.notices
    );
}

#[test]
fn another_repositorys_pr_number_is_not_matched() {
    let mut snap: Value = serde_json::from_str(FIXTURE).unwrap();
    for r in snap["backlog"]["records"].as_array_mut().unwrap() {
        if r["id"] == "shop-deploy-hook" {
            r["pr_url"] = "https://github.com/upstream/shop/pull/141".into();
        }
    }
    let b = board_with(&snap, &release_env());
    assert!(has(&b, "Shop#141"), "#141 of acme/shop stays its own card");
    assert_eq!(card(&b, "shop-deploy-hook").column, Column::Dev);
}

#[test]
fn halted_projects_get_no_git_only_cards() {
    let mut env = release_env();
    let mut legacy = shop_git();
    legacy.web = Some("https://github.com/acme/legacy".into());
    env.gits
        .insert(PathBuf::from("/fm/projects/legacy"), legacy);
    let b = board_with(&serde_json::from_str(FIXTURE).unwrap(), &env);
    assert!(!b.cards.iter().any(|c| c.project == "legacy" && c.outside));
    assert!(b.release("legacy").is_none());
    assert!(b.release("Shop").is_some());
}

#[test]
fn only_projects_on_the_board_are_read_for_releases() {
    let b = release_board();
    let names: Vec<&str> = b.releases.iter().map(|r| r.project.as_str()).collect();
    assert!(
        names.contains(&"Shop") && names.contains(&"Site"),
        "{names:?}"
    );
    assert!(!names.contains(&"tool"), "tool uses no lane: {names:?}");
}
