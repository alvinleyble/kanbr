use super::*;
use crate::model::tests::release_board;
use std::cell::RefCell;
use std::fs;

fn card(b: &Board, id: &str) -> Card {
    b.cards
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no card {id}"))
        .clone()
}

/// The release board with a PR on two Building cards.
fn board() -> Board {
    let mut b = release_board();
    for c in &mut b.cards {
        match c.id.as_str() {
            "shop-force-update" => c.pr_url = Some("https://github.com/acme/shop/pull/160".into()),
            "kanbr-1-board" => c.pr_url = Some("https://github.com/acme/kanbr/pull/3".into()),
            _ => {}
        }
    }
    b
}

fn plan_of(b: &Board, id: &str, to: Column) -> Result<Plan, String> {
    plan(b, &Config::default(), &card(b, id), to)
}

#[test]
fn drags_go_forward_one_lane_at_a_time() {
    let b = board();
    let err = plan_of(&b, "shop-print-queue", Column::Building).unwrap_err();
    assert_eq!(err, "one lane at a time: Booked to Ready first");
    let err = plan_of(&b, "Shop#144", Column::Dev).unwrap_err();
    assert!(err.contains("only moves work forward"), "{err}");
    assert!(err.contains("revert"), "{err}");
    let err = plan_of(&b, "Shop#141", Column::Live).unwrap_err();
    assert!(err.contains("already in Live"), "{err}");
    let err = plan_of(&b, "site-hero-copy", Column::Staging).unwrap_err();
    assert_eq!(err, "Site does not use Staging");
    let err = plan_of(&b, "legacy-capture-fix", Column::Ready).unwrap_err();
    assert!(err.contains("halted"), "{err}");
}

#[test]
fn next_lane_skips_lanes_the_project_does_not_use() {
    use crate::firstmate::Lanes;
    let all = Lanes {
        dev: true,
        staging: true,
        live: true,
    };
    let main_only = Lanes {
        live: true,
        ..Lanes::default()
    };
    let no_staging = Lanes {
        dev: true,
        staging: false,
        live: true,
    };
    assert_eq!(next_column(Column::Booked, all), Some(Column::Ready));
    assert_eq!(next_column(Column::Ready, all), Some(Column::Building));
    assert_eq!(next_column(Column::Building, all), Some(Column::Dev));
    assert_eq!(next_column(Column::Building, main_only), Some(Column::Live));
    assert_eq!(next_column(Column::Dev, all), Some(Column::Staging));
    assert_eq!(next_column(Column::Dev, no_staging), Some(Column::Live));
    assert_eq!(next_column(Column::Staging, all), Some(Column::Live));
    assert_eq!(next_column(Column::Live, all), None);
}

#[test]
fn each_drag_means_one_request() {
    let b = board();
    let ready = plan_of(&b, "site-launch-post", Column::Ready).unwrap();
    assert_eq!(ready.action, Action::Ready);
    assert_eq!(
        (ready.branch.clone(), ready.passphrase.clone()),
        (None, None)
    );

    let worker = plan_of(&b, "site-dark-mode", Column::Building).unwrap();
    assert_eq!(worker.action, Action::Worker);
    assert_eq!(worker.blocked_by, vec!["site-contact-form"]);
    assert_eq!(worker.owner, Owner::Secondmate("sm-alpha".into()));

    let err = plan_of(&b, "site-contact-form", Column::Dev).unwrap_err();
    assert!(err.contains("no PR"), "{err}");
    let merge = plan_of(&b, "shop-force-update", Column::Dev).unwrap();
    assert_eq!(merge.action, Action::Merge);
    assert_eq!(merge.branch.as_deref(), Some("dev"));
    assert_eq!(merge.passphrase, None, "dev is not a passphrase lane");
    assert_eq!(
        merge.headline(),
        "merge https://github.com/acme/shop/pull/160 into dev"
    );
    // A project that only has main merges straight to Live, with its word.
    let live_merge = plan_of(&b, "kanbr-1-board", Column::Live).unwrap();
    assert_eq!(live_merge.action, Action::Merge);
    assert_eq!(live_merge.passphrase.as_deref(), Some("main"));

    let promote = plan_of(&b, "Shop#144", Column::Live).unwrap();
    assert_eq!(promote.action, Action::Promote);
    assert_eq!(promote.source_branch.as_deref(), Some("staging"));
    assert_eq!(promote.branch.as_deref(), Some("main"));
    assert_eq!(promote.passphrase.as_deref(), Some("main"));
    assert_eq!(promote.moves, vec!["#144 feat: saved carts"]);
    assert_eq!(
        promote.migrations,
        vec!["supabase/migrations/20260926_fee.sql"]
    );
    assert_eq!(promote.headline(), "promote Shop to Live (staging -> main)");

    let site = plan_of(&b, "site-hero-copy", Column::Live).unwrap();
    assert_eq!(site.action, Action::Promote);
    assert_eq!(site.source_branch.as_deref(), Some("dev"));
    assert_eq!(site.moves.len(), 2, "every Site card in Dev goes along");
    assert!(
        site.migrations.is_empty(),
        "migrations are for Staging to Live"
    );
}

#[test]
fn a_promotion_moves_only_the_lanes_changes() {
    let mut b = board();
    let mut promo = card(&b, "Shop#144");
    promo.id = "shop-promote-dev".into();
    promo.title = "Promote dev to staging".into();
    promo.pr_number = Some(148);
    promo.promotion = true;
    let mut report = card(&b, "Shop#144");
    report.id = "shop-emulator".into();
    report.title = "Set up emulator build".into();
    report.pr_number = None;
    report.not_release = true;
    b.cards.extend([promo, report]);
    assert_eq!(b.column_cards(Column::Staging, Some("Shop")).len(), 3);

    let promote = plan_of(&b, "Shop#144", Column::Live).unwrap();
    assert_eq!(promote.moves, vec!["#144 feat: saved carts"]);
    assert_eq!(
        promote.moves.len(),
        b.changes_in(Column::Staging, Some("Shop")).len(),
        "the request counts what the lane header counts"
    );
}

#[test]
fn passphrase_lanes_come_from_the_config() {
    let b = board();
    let mut config = Config {
        passphrase_lanes: Vec::new(),
        ..Config::default()
    };
    let p = plan(&b, &config, &card(&b, "Shop#144"), Column::Live).unwrap();
    assert_eq!(p.passphrase, None);
    config.passphrase_lanes = vec![Column::Dev];
    let p = plan(&b, &config, &card(&b, "shop-force-update"), Column::Dev).unwrap();
    assert_eq!(p.passphrase.as_deref(), Some("dev"));
}

#[test]
fn a_promotion_needs_the_projects_branches() {
    let b = board();
    let mut c = card(&b, "site-hero-copy");
    c.project = "kanbr".into();
    let err = plan(&b, &Config::default(), &c, Column::Live).unwrap_err();
    assert!(err.contains("cannot read kanbr's branches"), "{err}");
}

#[test]
fn request_ids_are_valid_inbox_ids() {
    assert_eq!(
        request_id("merge", "Shop#144", 1_790_000_000),
        "kanbr-merge-Shop-144-1790000000"
    );
    assert_eq!(
        request_id("ready", "##", 1_790_000_000),
        "kanbr-ready-card-1790000000"
    );
    let long = request_id("promote", &"x".repeat(300), 1_790_000_000);
    assert!(long.len() <= 128, "{}", long.len());
    assert!(
        long.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    );
    assert!(long.ends_with("-1790000000"));
}

fn secret(s: &str) -> Secret {
    let mut out = Secret::new();
    out.push_str(s);
    out
}

#[test]
fn the_passphrase_is_the_last_line_and_never_in_the_wake_summary() {
    let b = board();
    let mut p = plan_of(&b, "Shop#144", Column::Live).unwrap();
    p.title = "two\nlines\tand a tab".into();
    let word = secret("hunter2-word");
    let body = request_body(&p, "kanbr-promote-Shop-144-1", None, Some(&word));
    let text = body.expose().to_owned();
    let last = text.lines().last().unwrap();
    assert_eq!(last, "passphrase (main): hunter2-word");
    assert_eq!(text.matches("hunter2-word").count(), 1);
    // Firstmate's inbox quotes the first 100 characters in its wake-up line.
    let summary: String = text.replace(['\n', '\t'], " ").chars().take(100).collect();
    assert!(!summary.contains("hunter2"), "{summary}");
    assert!(text.find("hunter2").unwrap() > 200);
    for field in [
        "schema: kanbr-request.v1",
        "request: kanbr-promote-Shop-144-1",
        "action: promote",
        "project: Shop",
        "card: Shop#144",
        "title: two lines and a tab",
        "from: Staging",
        "to: Live",
        "branches: staging -> main",
        "moves: 1 change(s): #144 feat: saved carts",
        "migrations: 1 database migration(s)",
        "refused: <reason>",
    ] {
        assert!(text.contains(field), "missing {field}\n{text}");
    }
    assert!(text.starts_with("Kanbr request (kanbr-request.v1): promote Shop to Live"));
    // Without a passphrase lane there is no passphrase line at all.
    let ready = plan_of(&b, "site-launch-post", Column::Ready).unwrap();
    let text = request_body(&ready, "r", None, Some(&word))
        .expose()
        .to_owned();
    assert!(!text.contains("passphrase"), "{text}");
    assert!(text.contains("talked through; ready"), "{text}");
}

#[test]
fn keyed_lines_match_the_intake() {
    assert_eq!(
        keyed_line("shop-print-queue", "Print queue", "build it", false).unwrap(),
        "shop-print-queue\tbuild it\tPrint queue -> build it\tdone"
    );
    let line = keyed_line("site-pricing-page", "Pricing", "yes\tthree\ntiers", true).unwrap();
    assert_eq!(line.matches('\t').count(), 3, "{line}");
    assert!(line.ends_with("\trelease"));
    assert!(line.contains("\tyes three tiers\t"), "{line}");
    assert!(keyed_line("Shop#144", "x", "y", false).is_err());
    assert!(keyed_line("", "x", "y", false).is_err());
}

#[test]
fn answers_are_checked_before_sending() {
    assert!(check_answer("  ").is_err());
    let reserved = check_answer(" Reconcile ").unwrap_err();
    assert!(reserved.contains("reserved"), "{reserved}");
    assert!(check_answer(&"x".repeat(ANSWER_LIMIT + 1)).is_err());
    assert!(check_answer("go with option B").is_ok());
}

#[test]
fn inbox_receipts_and_checks_parse() {
    let saved = parse_saved(
        r#"{"schema":"fm-inbox-note.v1","outcome":"created","id":"1790-abc","request_id":"r","saved":true,"announced":true,"acknowledged":false,"path":"/x"}"#,
    )
    .unwrap();
    assert_eq!(
        saved,
        Saved {
            note_id: "1790-abc".into(),
            announced: true
        }
    );
    let unwoken = parse_saved(
        r#"{"schema":"fm-inbox-note.v1","outcome":"created","id":"n","saved":true,"announced":false,"acknowledged":false}"#,
    )
    .unwrap();
    assert!(!unwoken.announced);
    assert!(parse_saved(r#"{"schema":"other","id":"n","saved":true}"#).is_err());
    assert!(parse_saved("queued n").is_err());

    let receipts = parse_receipts(
        r#"{"schema":"fm-inbox-receipts.v1","pending":[{"id":"n1","body":"passphrase (main): w","reply":null}],
            "handled":[{"id":"n2","body":"b","reply":{"id":"n2","body":"done: merged","cursor":"000000000001"}}],"replies":[],"omitted":[]}"#,
    )
    .unwrap();
    assert_eq!(
        receipts,
        vec![
            Receipt {
                id: "n1".into(),
                acknowledged: false,
                reply: None
            },
            Receipt {
                id: "n2".into(),
                acknowledged: true,
                reply: Some("done: merged".into())
            },
        ]
    );
    assert!(parse_receipts(r#"{"schema":"fm-inbox-receipts.v2"}"#).is_err());
    assert!(parse_receipts(r#"{"schema":"fm-inbox-receipts.v1","pending":[]}"#).is_err());

    let green = parse_checks(
        r#"{"state":"OPEN","isDraft":false,"statusCheckRollup":[
            {"__typename":"CheckRun","name":"ci","status":"COMPLETED","conclusion":"SUCCESS"},
            {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"SKIPPED"},
            {"__typename":"StatusContext","context":"deploy","state":"SUCCESS"}]}"#,
    )
    .unwrap();
    assert_eq!(green.problem(), None);
    assert_eq!(green.summary(), "3 check(s) green");
    let red = parse_checks(
        r#"{"state":"OPEN","isDraft":false,"statusCheckRollup":[
            {"__typename":"CheckRun","name":"ci","status":"COMPLETED","conclusion":"FAILURE"},
            {"__typename":"CheckRun","name":"e2e","status":"IN_PROGRESS","conclusion":""},
            {"__typename":"StatusContext","context":"deploy","state":"PENDING"}]}"#,
    )
    .unwrap();
    assert_eq!(
        red.problem().unwrap(),
        "checks are not green: 1 failing (ci); 2 still running (e2e, deploy)"
    );
    let draft = parse_checks(r#"{"state":"OPEN","isDraft":true,"statusCheckRollup":[]}"#).unwrap();
    assert_eq!(draft.problem().unwrap(), "the PR is a draft");
    let merged = parse_checks(r#"{"state":"MERGED","isDraft":false}"#).unwrap();
    assert!(merged.problem().unwrap().contains("already merged"));
    let none = parse_checks(r#"{"state":"OPEN","isDraft":false,"statusCheckRollup":[]}"#).unwrap();
    assert_eq!(
        none.problem().unwrap(),
        "checks are not green: no checks reported on the PR yet"
    );
    let unreported = parse_checks(r#"{"state":"OPEN","isDraft":false}"#).unwrap();
    assert!(unreported.problem().is_some());
}

/// Records every call; answers from canned results.
#[derive(Default)]
struct Fake {
    notes: RefCell<Vec<(String, String)>>,
    answers: RefCell<Vec<(Option<PathBuf>, String)>>,
    checks: Option<Result<Checks, String>>,
    answer_result: Option<Result<String, String>>,
    note_fails: bool,
}

impl Transport for Fake {
    fn note(&self, request_id: &str, body: &[u8]) -> Result<Saved, String> {
        if self.note_fails {
            return Err("fm-inbox: refusing".into());
        }
        self.notes.borrow_mut().push((
            request_id.to_owned(),
            String::from_utf8_lossy(body).into_owned(),
        ));
        Ok(Saved {
            note_id: format!("note-{}", self.notes.borrow().len()),
            announced: true,
        })
    }
    fn receipts(&self) -> Result<Vec<Receipt>, String> {
        Ok(Vec::new())
    }
    fn answer(&self, home: Option<&Path>, line: &str) -> Result<String, String> {
        self.answers
            .borrow_mut()
            .push((home.map(Path::to_path_buf), line.to_owned()));
        self.answer_result
            .clone()
            .unwrap_or_else(|| Ok("closed: done".into()))
    }
    fn checks(&self, _: &str) -> Result<Checks, String> {
        self.checks
            .clone()
            .unwrap_or_else(|| Err("gh is not installed".into()))
    }
}

fn green() -> Checks {
    Checks {
        state: "OPEN".into(),
        passed: 2,
        ..Checks::default()
    }
}

#[test]
fn a_merge_is_asked_for_only_on_green_checks() {
    let b = board();
    let plan = plan_of(&b, "shop-force-update", Column::Dev).unwrap();
    let job = |plan: &Plan| Job::Request {
        plan: plan.clone(),
        request_id: "kanbr-merge-x-1".into(),
        passphrase: None,
    };
    let red = Fake {
        checks: Some(Ok(Checks {
            state: "OPEN".into(),
            failed: vec!["ci".into()],
            ..Checks::default()
        })),
        ..Fake::default()
    };
    let out = run_job(&red, job(&plan));
    assert!(
        matches!(&out, Outcome::NotSent { reason, .. } if reason.contains("1 failing (ci)")),
        "{out:?}"
    );
    assert!(red.notes.borrow().is_empty(), "nothing reaches Firstmate");

    let unchecked = Fake {
        checks: Some(Ok(Checks {
            state: "OPEN".into(),
            ..Checks::default()
        })),
        ..Fake::default()
    };
    let out = run_job(&unchecked, job(&plan));
    assert!(
        matches!(&out, Outcome::NotSent { reason, .. } if reason.contains("no checks reported") && reason.contains("waits for green checks")),
        "{out:?}"
    );
    assert!(
        unchecked.notes.borrow().is_empty(),
        "nothing reaches Firstmate"
    );

    let unknown = Fake::default();
    let out = run_job(&unknown, job(&plan));
    assert!(
        matches!(&out, Outcome::NotSent { reason, .. } if reason.contains("cannot confirm")),
        "{out:?}"
    );
    assert!(unknown.notes.borrow().is_empty());

    let ok = Fake {
        checks: Some(Ok(green())),
        ..Fake::default()
    };
    let out = run_job(&ok, job(&plan));
    assert_eq!(
        out,
        Outcome::Requested {
            card_id: "shop-force-update".into(),
            request_id: "kanbr-merge-x-1".into(),
            note_id: "note-1".into(),
            warning: None
        }
    );
    let notes = ok.notes.borrow();
    assert_eq!(notes[0].0, "kanbr-merge-x-1");
    assert!(
        notes[0]
            .1
            .contains("checks: 2 check(s) green, read by Kanbr at")
    );
    assert!(notes[0].1.contains("action: merge"));
}

#[test]
fn a_promotion_carries_its_passphrase_only_in_the_note() {
    let b = board();
    let plan = plan_of(&b, "Shop#144", Column::Live).unwrap();
    let fake = Fake::default();
    let out = run_job(
        &fake,
        Job::Request {
            plan,
            request_id: "kanbr-promote-Shop-144-1".into(),
            passphrase: Some(secret("s3cret-main")),
        },
    );
    assert!(matches!(out, Outcome::Requested { .. }), "{out:?}");
    let notes = fake.notes.borrow();
    assert!(notes[0].1.ends_with("passphrase (main): s3cret-main\n"));
    assert!(!format!("{out:?}").contains("s3cret"));
    let failing = Fake {
        note_fails: true,
        ..Fake::default()
    };
    let out = run_job(
        &failing,
        Job::Request {
            plan: plan_of(&b, "Shop#144", Column::Live).unwrap(),
            request_id: "r".into(),
            passphrase: Some(secret("s3cret-main")),
        },
    );
    assert!(
        matches!(&out, Outcome::NotSent { reason, .. } if reason == "fm-inbox: refusing"),
        "{out:?}"
    );
}

fn answer_job(b: &Board, id: &str, answer: &str, release: bool) -> AnswerJob {
    let c = card(b, id);
    AnswerJob {
        card_id: c.id.clone(),
        title: c.title.clone(),
        project: c.project.clone(),
        owner: c.owner.clone(),
        ask: c.ask.clone().unwrap(),
        answer: answer.into(),
        release,
        request_id: format!("kanbr-answer-{id}-1"),
    }
}

#[test]
fn a_held_call_is_answered_through_the_intake_of_its_home_then_firstmate_is_woken() {
    let b = board();
    let fake = Fake::default();
    let out = run_job(
        &fake,
        Job::Answer(answer_job(&b, "site-pricing-page", "three tiers", true)),
    );
    assert_eq!(
        out,
        Outcome::Answered {
            card_id: "site-pricing-page".into(),
            request_id: "kanbr-answer-site-pricing-page-1".into(),
            detail: "closed: done".into(),
            note_id: None,
            warning: None
        }
    );
    let answers = fake.answers.borrow();
    assert_eq!(answers[0].0.as_deref(), Some(Path::new("/mates/alpha")));
    assert_eq!(
        answers[0].1,
        "site-pricing-page\tthree tiers\tDecide the pricing page -> three tiers\trelease"
    );
    let notes = fake.notes.borrow();
    assert_eq!(notes.len(), 1, "one wake-up note");
    assert!(notes[0].1.contains("action: answered"));
    assert!(notes[0].1.contains("relay it to second mate sm-alpha"));

    // The main home's own call; the intake refuses it.
    let refusing = Fake {
        answer_result: Some(Err(
            "skipped: shop-print-queue (no captain-held task with that id)".into(),
        )),
        ..Fake::default()
    };
    let out = run_job(
        &refusing,
        Job::Answer(answer_job(&b, "shop-print-queue", "not now", false)),
    );
    assert!(
        matches!(&out, Outcome::NotSent { reason, .. } if reason.contains("no captain-held task")),
        "{out:?}"
    );
    assert_eq!(refusing.answers.borrow()[0].0, None, "the board's own home");
    assert!(
        refusing.notes.borrow().is_empty(),
        "no wake for a refused answer"
    );
}

#[test]
fn a_workers_question_is_answered_through_a_note() {
    let b = board();
    let fake = Fake::default();
    let out = run_job(
        &fake,
        Job::Answer(answer_job(&b, "tool-cli-flags", "use --dry-run", false)),
    );
    assert!(
        matches!(&out, Outcome::Answered { note_id: Some(n), .. } if n == "note-1"),
        "{out:?}"
    );
    assert!(
        fake.answers.borrow().is_empty(),
        "no keyed intake for a worker"
    );
    let body = &fake.notes.borrow()[0].1;
    assert!(body.contains("question: Pick flag names [key=tool-flags-naming]"));
    assert!(body.contains("answer: use --dry-run"));
}

#[cfg(unix)]
fn fake_home(name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("kanbr-act-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("bin")).unwrap();
    let inbox = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FM_HOME/args.log"
case "$1" in
  note)
    cat > "$FM_HOME/body.log"
    grep -q unwoken "$FM_HOME/body.log" && { printf '{"schema":"fm-inbox-note.v1","outcome":"created","id":"n2","saved":true,"announced":false,"acknowledged":false}\n'; exit 3; }
    printf '{"schema":"fm-inbox-note.v1","outcome":"created","id":"n1","request_id":"%s","saved":true,"announced":true,"acknowledged":false}\n' "$3" ;;
  receipts)
    printf '{"schema":"fm-inbox-receipts.v1","pending":[],"handled":[{"id":"n1","reply":{"body":"done: promoted"}}],"replies":[]}\n' ;;
  *) echo "fm-inbox: bad" >&2; exit 1 ;;
esac
"#;
    let hold = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FM_HOME/args.log"
cat > "$FM_HOME/answer.log"
grep -q '^bad' "$FM_HOME/answer.log" && { echo "skipped: bad (no captain-held task with that id)"; exit 1; }
echo "closed: $(cut -f1 "$FM_HOME/answer.log")"
"#;
    for (file, text) in [("fm-inbox.sh", inbox), ("fm-captain-hold.sh", hold)] {
        let p = root.join("bin").join(file);
        fs::write(&p, text).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    root
}

#[cfg(unix)]
#[test]
fn the_real_transport_uses_firstmates_scripts_with_the_body_on_stdin() {
    let home = fake_home("main");
    let mate = fake_home("mate");
    let t = Firstmate { home: home.clone() };
    let saved = t
        .note(
            "kanbr-promote-Shop-1",
            b"body line\npassphrase (main): w0rd\n",
        )
        .unwrap();
    assert_eq!(
        saved,
        Saved {
            note_id: "n1".into(),
            announced: true
        }
    );
    let args = fs::read_to_string(home.join("args.log")).unwrap();
    assert_eq!(args, "note --request-id kanbr-promote-Shop-1 --json -\n");
    assert!(
        !args.contains("w0rd"),
        "the passphrase never rides in arguments"
    );
    assert_eq!(
        fs::read_to_string(home.join("body.log")).unwrap(),
        "body line\npassphrase (main): w0rd\n"
    );
    let unwoken = t.note("r2", b"unwoken\n").unwrap();
    assert!(!unwoken.announced, "exit 3: saved but not woken");

    let receipts = t.receipts().unwrap();
    assert_eq!(receipts[0].reply.as_deref(), Some("done: promoted"));

    let ok = t
        .answer(Some(&mate), "site-pricing-page\tyes\tl\trelease")
        .unwrap();
    assert_eq!(ok, "closed: site-pricing-page");
    assert_eq!(
        fs::read_to_string(mate.join("args.log")).unwrap(),
        "answers --source kanbr board\n"
    );
    assert_eq!(
        fs::read_to_string(mate.join("answer.log")).unwrap(),
        "site-pricing-page\tyes\tl\trelease\n"
    );
    let err = t.answer(None, "bad\tno\tl\tdone").unwrap_err();
    assert!(err.contains("no captain-held task"), "{err}");

    let missing = Firstmate {
        home: home.join("nowhere"),
    };
    assert!(missing.note("r", b"x").unwrap_err().contains("missing"));
    fs::remove_dir_all(&home).unwrap();
    fs::remove_dir_all(&mate).unwrap();
}
