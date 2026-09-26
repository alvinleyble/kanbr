use super::*;
use crate::dates::parse_date;
use std::collections::HashMap;

const FIXTURE: &str = include_str!("../../tests/fixtures/snapshot.json");

pub(crate) struct FakeEnv {
    pub metas: HashMap<PathBuf, Meta>,
    pub lanes: HashMap<PathBuf, Lanes>,
}

impl Env for FakeEnv {
    fn meta(&self, path: &Path) -> Option<Meta> {
        self.metas.get(path).cloned()
    }
    fn lanes(&self, repo: &Path) -> Option<Lanes> {
        self.lanes.get(repo).copied()
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
    let mut lanes = HashMap::new();
    lanes.insert(
        PathBuf::from("/fm"),
        Lanes {
            dev: false,
            staging: false,
            live: true,
        },
    );
    lanes.insert(
        PathBuf::from("/fm/projects/Shop"),
        Lanes {
            dev: true,
            staging: true,
            live: true,
        },
    );
    lanes.insert(
        PathBuf::from("/fm/projects/Site"),
        Lanes {
            dev: true,
            staging: false,
            live: true,
        },
    );
    lanes.insert(PathBuf::from("/fm/projects/tool"), Lanes::default());
    FakeEnv { metas, lanes }
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
    let snap: Value = serde_json::from_str(FIXTURE).unwrap();
    let env = fake_env();
    let config = Config::default();
    let registry = registry();
    let ctx = Context {
        registry: &registry,
        config: &config,
        now: now(),
        env: &env,
    };
    build_board(&snap, &ctx)
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
    env.lanes.insert(
        PathBuf::from("/fm/projects/Shop"),
        Lanes {
            dev: false,
            staging: true,
            live: true,
        },
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
