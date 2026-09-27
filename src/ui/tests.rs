use super::*;
use crate::model::tests::{fixture_board, now};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyModifiers;

fn app() -> App {
    App::with_board(fixture_board(), "/fm".into(), now())
}

fn draw(app: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| render(app, f)).unwrap();
    let buf = t.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn click(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn tab_index(app: &App, name: &str) -> usize {
    app.tabs().iter().position(|t| t.0 == name).unwrap()
}

#[test]
fn renders_six_columns_tabs_and_waiting_strip() {
    let mut a = app();
    let screen = draw(&mut a, 200, 50);
    for title in ["Booked", "Ready", "Building", "Dev", "Staging", "Live"] {
        assert!(screen.contains(title), "missing column {title}\n{screen}");
    }
    assert!(screen.contains("1 All"), "{screen}");
    assert!(screen.contains("4 waiting on you"), "{screen}");
    assert!(screen.contains("opus-5-5"), "{screen}");
}

#[test]
fn configured_labels_replace_column_titles() {
    let mut a = app();
    a.labels[5] = "Production".into();
    a.labels[0] = "Backlog".into();
    let screen = draw(&mut a, 200, 40);
    assert!(screen.contains("Production"), "{screen}");
    assert!(screen.contains("Backlog"), "{screen}");
}

#[test]
fn number_keys_switch_to_a_project_tab() {
    let mut a = app();
    let shop = tab_index(&a, "Shop");
    press(
        &mut a,
        KeyCode::Char(char::from_digit(shop as u32 + 1, 10).unwrap()),
    );
    assert_eq!(a.tab, shop);
    for col in 0..6 {
        assert!(a.column_cards(col).iter().all(|c| c.project == "Shop"));
    }
    press(&mut a, KeyCode::Char('1'));
    assert_eq!(a.tab, 0);
}

#[test]
fn clicking_a_tab_selects_it() {
    let mut a = app();
    draw(&mut a, 200, 40);
    let (rect, idx) = a.hits.tabs[2];
    click(&mut a, rect.x + 1, rect.y);
    assert_eq!(a.tab, idx);
}

#[test]
fn w_jumps_through_every_waiting_card() {
    let mut a = app();
    let mut seen = Vec::new();
    for _ in 0..4 {
        press(&mut a, KeyCode::Char('w'));
        let c = a.selected_card().unwrap();
        assert!(c.decision, "{} is not waiting", c.id);
        seen.push(c.id.clone());
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 4);
}

#[test]
fn jumping_from_a_tab_with_nothing_waiting_switches_to_all() {
    let mut a = app();
    a.select_tab(tab_index(&a, "firstmate"));
    press(&mut a, KeyCode::Char('w'));
    assert_eq!(a.tab, 0);
    assert!(a.selected_card().unwrap().decision);
}

#[test]
fn clicking_the_strip_jumps() {
    let mut a = app();
    draw(&mut a, 200, 40);
    let strip = a.hits.strip.unwrap();
    click(&mut a, strip.x + 2, strip.y);
    assert!(a.selected_card().unwrap().decision);
}

#[test]
fn enter_opens_details_and_esc_closes() {
    let mut a = app();
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Right);
    assert_eq!(a.col, 2);
    press(&mut a, KeyCode::Enter);
    let Modal::Details(id) = a.modal.clone() else {
        panic!("no details")
    };
    assert_eq!(id, "kanbr-1-board");
    let screen = draw(&mut a, 160, 50);
    assert!(screen.contains("xhigh"), "{screen}");
    assert!(screen.contains("/wt/kanbr"), "{screen}");
    press(&mut a, KeyCode::Esc);
    assert_eq!(a.modal, Modal::None);
}

#[test]
fn double_click_opens_details() {
    let mut a = app();
    draw(&mut a, 200, 50);
    let hits = a.hits.cards.clone();
    let plain = hits
        .iter()
        .copied()
        .find(|&(_, col, idx)| !a.column_cards(col)[idx].decision)
        .unwrap();
    let (rect, col, idx) = plain;
    click(&mut a, rect.x + 2, rect.y);
    click(&mut a, rect.x + 2, rect.y);
    assert_eq!((a.col, a.sel[col]), (col, idx));
    assert!(matches!(a.modal, Modal::Details(_)));
    press(&mut a, KeyCode::Esc);
    // A card waiting on the captain opens its decision instead.
    let (rect, _, _) = hits[0];
    click(&mut a, rect.x + 2, rect.y + 1);
    click(&mut a, rect.x + 2, rect.y + 1);
    assert!(matches!(a.modal, Modal::Decide(_)), "{:?}", a.modal);
}

#[test]
fn cards_fit_their_width() {
    let b = fixture_board();
    for width in [12usize, 24, 40] {
        for c in &b.cards {
            for line in card_lines(c, width, false, now(), None) {
                assert!(line.width() <= width, "{} at {width}: {:?}", c.id, line);
            }
        }
    }
}

#[test]
fn card_header_has_project_pr_and_second_mate_tag() {
    let b = fixture_board();
    let c = b.cards.iter().find(|c| c.id == "site-footer").unwrap();
    let lines = card_lines(c, 30, false, now(), None);
    let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        header.contains("Site") && header.contains("#15") && header.contains("2nd"),
        "{header}"
    );
    let d = b.cards.iter().find(|c| c.id == "shop-print-queue").unwrap();
    let title: String = card_lines(d, 40, false, now(), None)[1]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(title.contains('⚑') && title.contains("grill"), "{title}");
}

#[test]
fn tab_that_disappears_falls_back_to_all() {
    let mut a = app();
    a.select_tab(tab_index(&a, "legacy"));
    let mut next = fixture_board();
    next.cards.retain(|c| c.project != "legacy");
    next.projects.retain(|p| p.name != "legacy");
    a.apply(Msg::Loaded(Box::new(Ok(next))));
    assert_eq!(a.tab, 0);
}

#[test]
fn selection_follows_the_same_card_across_refreshes() {
    let mut a = app();
    press(&mut a, KeyCode::Down);
    let id = a.selected_card().unwrap().id.clone();
    a.apply(Msg::Loaded(Box::new(Ok(fixture_board()))));
    assert_eq!(a.selected_card().unwrap().id, id);
}

#[test]
fn failed_refresh_keeps_the_board_and_says_so() {
    let mut a = app();
    a.apply(Msg::Loaded(Box::new(Err("snapshot exited 1".into()))));
    assert!(a.board.is_some());
    let screen = draw(&mut a, 200, 40);
    assert!(
        screen.contains("refresh failed: snapshot exited 1"),
        "{screen}"
    );
}

#[test]
fn first_load_failure_is_shown_not_an_empty_board() {
    let mut a = App::new("/fm".into());
    a.apply(Msg::Loaded(Box::new(Err(
        "bin/fm-fleet-snapshot.sh is missing".into(),
    ))));
    let screen = draw(&mut a, 120, 30);
    assert!(screen.contains("could not read Firstmate"), "{screen}");
    assert!(screen.contains("kanbr doctor"), "{screen}");
}

#[test]
fn tiny_terminal_does_not_panic() {
    let mut a = app();
    draw(&mut a, 20, 6);
    draw(&mut a, 1, 1);
}

#[test]
fn truncation_is_width_aware() {
    assert_eq!(truncate("hello world", 5), "hell…");
    assert_eq!(truncate("short", 10), "short");
    assert_eq!(truncate("日本語", 4), "日…");
    assert_eq!(truncate("日本語", 2), "…");
    assert_eq!(truncate("anything", 0), "");
}

fn release_app() -> App {
    App::with_board(crate::model::tests::release_board(), "/fm".into(), now())
}

#[test]
fn release_headers_sit_under_staging_and_live() {
    let mut a = release_app();
    let shop = tab_index(&a, "Shop");
    a.select_tab(shop);
    let screen = draw(&mut a, 252, 40);
    assert!(
        screen.contains("25 Sep · #150 · v1.2.1 · 4 changes"),
        "{screen}"
    );
    assert!(screen.contains("1 to promote · 1 db change"), "{screen}");
    let narrow = draw(&mut a, 120, 40);
    assert!(
        narrow.contains(" 25 Sep #150 v1.2.1"),
        "shorter form\n{narrow}"
    );
    assert!(narrow.contains(" 1 ready · 1 db"), "{narrow}");
    let site = tab_index(&a, "Site");
    a.select_tab(site);
    let screen = draw(&mut a, 252, 40);
    assert!(screen.contains("not used"), "Site has no staging\n{screen}");
}

#[test]
fn i_opens_release_details_and_capital_r_the_notes() {
    let mut a = release_app();
    let shop = tab_index(&a, "Shop");
    a.select_tab(shop);
    press(&mut a, KeyCode::Char('i'));
    assert_eq!(a.modal, Modal::Release);
    let screen = draw(&mut a, 160, 40);
    assert!(
        screen.contains("https://github.com/acme/shop/pull/150"),
        "{screen}"
    );
    assert!(
        screen.contains("supabase/migrations/20260926_fee.sql"),
        "{screen}"
    );
    press(&mut a, KeyCode::Esc);
    assert_eq!(a.modal, Modal::None);

    press(&mut a, KeyCode::Char('R'));
    assert_eq!(a.modal, Modal::Notes);
    assert!(
        !a.wants_mouse(),
        "the terminal selects text while notes are open"
    );
    let screen = draw(&mut a, 160, 40);
    assert!(
        screen.contains("Shop 1.2.1 — released 25 Sep 2026"),
        "{screen}"
    );
    assert!(
        screen.contains("- Persistent delivery fee per customer"),
        "{screen}"
    );
    press(&mut a, KeyCode::Char('c'));
    let copied = a.copy.take().expect("c asks the run loop to copy");
    assert!(copied.starts_with("Shop 1.2.1"), "{copied}");
    assert_eq!(a.modal, Modal::Notes, "copying keeps the notes open");
    press(&mut a, KeyCode::Esc);
    assert!(a.wants_mouse());
}

#[test]
fn clicking_a_release_header_opens_the_release_details() {
    let mut a = release_app();
    draw(&mut a, 200, 40);
    let (rect, col) = *a.hits.headers.iter().find(|(_, c)| *c == 5).unwrap();
    click(&mut a, rect.x + 1, rect.y);
    assert_eq!(a.modal, Modal::Release);
    assert_eq!(a.col, col);
    assert!(
        a.hits.headers.iter().all(|(_, c)| *c >= 3),
        "only Dev, Staging, and Live headers open it"
    );
}

#[test]
fn cards_without_a_firstmate_card_are_grey() {
    let b = crate::model::tests::release_board();
    let outside = b.cards.iter().find(|c| c.id == "Shop#141").unwrap();
    let lines = card_lines(outside, 30, false, now(), None);
    let title = lines[1].spans.last().unwrap();
    assert_eq!(title.style.fg, Some(Color::Gray));
    let tracked = b.cards.iter().find(|c| c.id == "shop-deploy-hook").unwrap();
    let lines = card_lines(tracked, 30, false, now(), None);
    assert_eq!(lines[1].spans.last().unwrap().style.fg, Some(Color::White));
}

#[test]
fn work_that_is_not_a_release_is_a_dim_labelled_group_in_live() {
    let mut a = release_app();
    let shop = tab_index(&a, "Shop");
    a.select_tab(shop);
    let screen = draw(&mut a, 252, 40);
    assert!(screen.contains(" Live 4 + 1 done"), "{screen}");
    let rows: Vec<&str> = screen.lines().collect();
    let label = rows
        .iter()
        .position(|r| r.contains("─ Done · not a release"))
        .unwrap_or_else(|| panic!("no Done label\n{screen}"));
    let report = rows
        .iter()
        .position(|r| r.contains("Second opinion on two bugs"))
        .unwrap();
    assert_eq!(
        report,
        label + 2,
        "the label sits right above the group\n{screen}"
    );

    let b = crate::model::tests::release_board();
    let done = b
        .cards
        .iter()
        .find(|c| c.id == "shop-worth-fixing")
        .unwrap();
    let lines = card_lines(done, 30, false, now(), None);
    assert_eq!(
        lines[1].spans.last().unwrap().style.fg,
        Some(Color::DarkGray)
    );

    // A Live column holding only such work labels it in the header rule.
    let mut b = crate::model::tests::release_board();
    b.cards
        .retain(|c| c.column != Column::Live || c.not_release);
    let mut a = App::with_board(b, "/fm".into(), now());
    let shop = tab_index(&a, "Shop");
    a.select_tab(shop);
    let screen = draw(&mut a, 252, 40);
    assert!(screen.contains(" Live 0 + 1 done"), "{screen}");
    assert!(screen.contains("─ Done · not a release"), "{screen}");
}

#[test]
fn promotion_cards_carry_a_promotion_badge() {
    let mut b = crate::model::tests::release_board();
    let c = b
        .cards
        .iter_mut()
        .find(|c| c.id == "shop-deploy-hook")
        .unwrap();
    c.promotion = true;
    let lines = card_lines(c, 30, false, now(), None);
    assert_eq!(lines[1].spans[1].content, "⇡ ");
}

#[test]
fn base64_matches_the_standard_alphabet() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64("Shop — 1".as_bytes()), "U2hvcCDigJQgMQ==");
}

// ------------------------------------------------------------ acting

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    app.handle_mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn typing(app: &mut App, text: &str) {
    for ch in text.chars() {
        press(app, KeyCode::Char(ch));
    }
}

/// Drags card `id` onto column `to` with the mouse.
fn drag(app: &mut App, id: &str, to: usize) {
    draw(app, 252, 40);
    let &(rect, col, _) = app
        .hits
        .cards
        .iter()
        .find(|&&(_, col, idx)| app.column_cards(col)[idx].id == id)
        .unwrap_or_else(|| panic!("{id} not on screen"));
    let (target, _) = app.hits.columns[to];
    mouse(
        app,
        MouseEventKind::Down(MouseButton::Left),
        rect.x + 2,
        rect.y,
    );
    assert_eq!(app.col, col);
    let (tx, ty) = (target.x + 2, target.y + target.height / 2);
    mouse(app, MouseEventKind::Drag(MouseButton::Left), tx, ty);
    mouse(app, MouseEventKind::Up(MouseButton::Left), tx, ty);
}

fn shop_app() -> App {
    let mut a = release_app();
    let shop = tab_index(&a, "Shop");
    a.select_tab(shop);
    a
}

fn moved(board: &Board, id: &str, to: Column) -> Board {
    let mut b = board.clone();
    b.cards.iter_mut().find(|c| c.id == id).unwrap().column = to;
    b
}

#[test]
fn a_drag_is_a_request_with_a_masked_passphrase() {
    let mut a = shop_app();
    drag(&mut a, "Shop#144", Column::Live.index());
    assert_eq!(a.modal, Modal::Confirm);
    let plan = &a.confirm.as_ref().unwrap().plan;
    assert_eq!(plan.action, actions::Action::Promote);
    assert_eq!(plan.passphrase.as_deref(), Some("main"));
    let screen = draw(&mut a, 252, 40);
    assert!(
        screen.contains("Ask Firstmate to promote Shop to Live"),
        "{screen}"
    );
    assert!(screen.contains("main passphrase:"), "{screen}");
    // Sending without the passphrase is refused in place.
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.modal, Modal::Confirm);
    assert!(a.jobs.is_empty());
    // Every key is passphrase text now, including the board's own keys.
    typing(&mut a, "qmx7");
    assert!(!a.quit);
    let screen = draw(&mut a, 252, 40);
    assert!(screen.contains("main passphrase: ••••"), "{screen}");
    assert!(!screen.contains("qmx7"), "{screen}");
    // A stray click does not throw the prompt away.
    click(&mut a, 0, 0);
    assert_eq!(a.modal, Modal::Confirm);
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.modal, Modal::None);
    assert!(a.confirm.is_none());
    let Some(Job::Request {
        plan,
        request_id,
        passphrase: Some(word),
    }) = a.jobs.pop()
    else {
        panic!("no request job");
    };
    assert_eq!(word.expose(), "qmx7");
    assert!(request_id.starts_with("kanbr-promote-Shop-144-"));
    assert_eq!(plan.card_id, "Shop#144");
    assert!(matches!(a.tracker.mark("Shop#144"), Some(Mark::Open(t)) if t.contains("sending")));

    a.apply(Msg::Action(Outcome::Requested {
        card_id: "Shop#144".into(),
        request_id: request_id.clone(),
        note_id: "n1".into(),
        warning: None,
    }));
    let screen = draw(&mut a, 252, 40);
    assert!(screen.contains("⇢ requested: promote"), "{screen}");
    // Still in Staging until the board shows it in Live.
    let board = a.board.clone().unwrap();
    a.apply(Msg::Loaded(Box::new(Ok(board.clone()))));
    assert!(a.tracker.get("Shop#144").is_some());
    a.apply(Msg::Loaded(Box::new(Ok(moved(
        &board,
        "Shop#144",
        Column::Live,
    )))));
    assert!(a.tracker.get("Shop#144").is_none());
    let screen = draw(&mut a, 252, 40);
    assert!(screen.contains("✓ moved to Live"), "{screen}");
}

#[test]
fn a_backward_or_skipping_drag_snaps_back_with_the_reason() {
    let mut a = shop_app();
    drag(&mut a, "Shop#144", Column::Dev.index());
    assert_eq!(a.modal, Modal::None);
    assert!(a.jobs.is_empty());
    let Some(Mark::Failed(why)) = a.tracker.mark("Shop#144") else {
        panic!("no snap-back");
    };
    assert!(why.contains("revert"), "{why}");
    let screen = draw(&mut a, 252, 40);
    assert!(
        screen.contains("↩ Kanbr only moves work forward"),
        "{screen}"
    );
    drag(&mut a, "shop-held-work", Column::Building.index());
    let Some(Mark::Failed(why)) = a.tracker.mark("shop-held-work") else {
        panic!("no snap-back");
    };
    assert!(why.contains("one lane at a time"), "{why}");
}

#[test]
fn m_asks_for_the_next_lane_and_esc_cancels() {
    let mut a = release_app();
    assert!(a.select_card("site-launch-post"));
    press(&mut a, KeyCode::Char('m'));
    assert_eq!(a.modal, Modal::Confirm);
    assert_eq!(
        a.confirm.as_ref().unwrap().plan.action,
        actions::Action::Ready
    );
    press(&mut a, KeyCode::Esc);
    assert_eq!(a.modal, Modal::None);
    assert!(a.jobs.is_empty() && a.tracker.get("site-launch-post").is_none());
    press(&mut a, KeyCode::Char('>'));
    press(&mut a, KeyCode::Char('y'));
    assert!(matches!(
        a.jobs.as_slice(),
        [Job::Request {
            passphrase: None,
            ..
        }]
    ));
    // A second request for the same card waits for the first.
    press(&mut a, KeyCode::Char('m'));
    assert_eq!(a.modal, Modal::None);
    assert_eq!(a.jobs.len(), 1);
    // Live has no next lane.
    assert!(a.select_card("Shop#141"));
    press(&mut a, KeyCode::Char('m'));
    assert!(matches!(a.tracker.mark("Shop#141"), Some(Mark::Failed(t)) if t.contains("last lane")));
}

#[test]
fn enter_on_a_waiting_card_answers_it_in_place() {
    let mut a = release_app();
    assert!(a.select_card("shop-promote-main"));
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.modal, Modal::Decide("shop-promote-main".into()));
    assert!(
        a.decide.as_ref().unwrap().release,
        "held work resumes by default"
    );
    let screen = draw(&mut a, 200, 50);
    assert!(screen.contains("keyed-answer intake"), "{screen}");
    typing(&mut a, "reconcile");
    press(&mut a, KeyCode::Enter);
    assert!(
        a.decide
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("reserved")
    );
    press(&mut a, KeyCode::Char('u'));
    a.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    typing(&mut a, "wait for q4 numbers");
    press(&mut a, KeyCode::Tab);
    assert!(!a.decide.as_ref().unwrap().release);
    assert!(!a.quit, "q is text in the answer");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.modal, Modal::None);
    let Some(Job::Answer(job)) = a.jobs.pop() else {
        panic!("no answer job")
    };
    assert_eq!(job.answer, "wait for q4 numbers");
    assert!(!job.release);
    assert_eq!(
        job.ask,
        Ask::Hold {
            home: None,
            work_item: true
        }
    );
    assert!(matches!(
        a.tracker.mark("shop-promote-main"),
        Some(Mark::Open(_))
    ));
    // A question-only call closes by default; a card with no decision shows details.
    assert!(a.select_card("shop-print-queue"));
    press(&mut a, KeyCode::Enter);
    assert!(!a.decide.as_ref().unwrap().release);
    press(&mut a, KeyCode::Esc);
    assert!(a.select_card("site-launch-post"));
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Modal::Details(_)));
}

#[test]
fn the_worker_pick_is_answered_in_place_while_the_request_stays_open() {
    let mut a = release_app();
    a.request_move("site-dark-mode", Column::Building);
    assert_eq!(a.modal, Modal::Confirm);
    press(&mut a, KeyCode::Enter);
    let Some(Job::Request { request_id, .. }) = a.jobs.pop() else {
        panic!("no request job")
    };
    a.apply(Msg::Action(Outcome::Requested {
        card_id: "site-dark-mode".into(),
        request_id: request_id.clone(),
        note_id: "n3".into(),
        warning: None,
    }));
    // Firstmate holds the task for the captain's model pick.
    let mut board = a.board.clone().unwrap();
    let card = board
        .cards
        .iter_mut()
        .find(|c| c.id == "site-dark-mode")
        .unwrap();
    card.decision = true;
    card.ask = Some(Ask::Hold {
        home: Some(PathBuf::from("/mates/alpha")),
        work_item: true,
    });
    a.apply(Msg::Loaded(Box::new(Ok(board.clone()))));
    assert!(a.select_card("site-dark-mode"));
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.modal, Modal::Decide("site-dark-mode".into()));
    typing(&mut a, "gpt-5.5");
    press(&mut a, KeyCode::Enter);
    let Some(Job::Answer(job)) = a.jobs.pop() else {
        panic!("no answer job")
    };
    assert_eq!(job.answer, "gpt-5.5");
    assert!(job.release, "held work resumes");
    let p = a.tracker.get("site-dark-mode").unwrap();
    assert_eq!(p.request_id, request_id, "the move stays open");
    assert!(matches!(
        p.kind,
        Kind::Move {
            to: Column::Building,
            ..
        }
    ));
    assert!(a.tracker.sending());
    // One answer at a time.
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Modal::Details(_)));
    press(&mut a, KeyCode::Esc);

    // An answer the intake refused is reported; the move stays open.
    a.apply(Msg::Action(Outcome::NotSent {
        card_id: "site-dark-mode".into(),
        request_id: job.request_id.clone(),
        reason: "the keyed-answer intake did not take it: busy".into(),
    }));
    let p = a.tracker.get("site-dark-mode").unwrap();
    assert_eq!(
        (p.request_id.as_str(), p.answering.as_deref()),
        (request_id.as_str(), None)
    );
    let (flash, ok, _) = a.tracker.flash.clone().unwrap();
    assert!(!ok && flash.contains("busy"), "{flash}");

    press(&mut a, KeyCode::Enter);
    typing(&mut a, "gpt-5.5");
    press(&mut a, KeyCode::Enter);
    let Some(Job::Answer(job)) = a.jobs.pop() else {
        panic!("no answer job")
    };
    a.apply(Msg::Action(Outcome::Answered {
        card_id: "site-dark-mode".into(),
        request_id: job.request_id,
        detail: "released".into(),
        note_id: None,
        warning: None,
    }));
    assert!(!a.tracker.sending());
    let (flash, ok, _) = a.tracker.flash.clone().unwrap();
    assert!(ok && flash.contains("answered (released)"), "{flash}");
    assert!(matches!(
        a.tracker.mark("site-dark-mode"),
        Some(Mark::Open(t)) if t.contains("requested: worker")
    ));
    // The recorded pick is not answered again before the board shows it.
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Modal::Details(_)));
    press(&mut a, KeyCode::Esc);
    a.apply(Msg::Loaded(Box::new(Ok(board.clone()))));
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Modal::Details(_)));
    press(&mut a, KeyCode::Esc);
    assert!(a.jobs.iter().all(|j| !matches!(j, Job::Answer(_))));
    let mut released = board.clone();
    let card = released
        .cards
        .iter_mut()
        .find(|c| c.id == "site-dark-mode")
        .unwrap();
    card.decision = false;
    card.ask = None;
    a.apply(Msg::Loaded(Box::new(Ok(released))));
    let p = a.tracker.get("site-dark-mode").unwrap();
    assert!(!p.answered, "the board shows the pick taken");
    assert_eq!(p.request_id, request_id, "the move stays open");
    // The card moves only once the board shows the worker.
    let card = board
        .cards
        .iter_mut()
        .find(|c| c.id == "site-dark-mode")
        .unwrap();
    card.decision = false;
    card.ask = None;
    card.column = Column::Building;
    a.apply(Msg::Loaded(Box::new(Ok(board))));
    assert!(a.tracker.get("site-dark-mode").is_none());
    assert_eq!(
        a.tracker.mark("site-dark-mode"),
        Some(Mark::Done("✓ moved to Building".into()))
    );
}

#[test]
fn only_a_worker_request_can_be_answered_alongside() {
    for (id, to, ask) in [
        (
            "site-launch-post",
            Column::Ready,
            Ask::Hold {
                home: None,
                work_item: false,
            },
        ),
        (
            "shop-force-update",
            Column::Dev,
            Ask::Worker {
                questions: vec!["which retry limit?".into()],
            },
        ),
    ] {
        let mut a = release_app();
        let mut board = a.board.clone().unwrap();
        let card = board.cards.iter_mut().find(|c| c.id == id).unwrap();
        card.decision = true;
        card.ask = Some(ask);
        if to == Column::Dev {
            card.pr_url = Some("https://github.com/acme/shop/pull/160".into());
        }
        a.apply(Msg::Loaded(Box::new(Ok(board))));
        a.request_move(id, to);
        assert_eq!(a.modal, Modal::Confirm, "{id}");
        press(&mut a, KeyCode::Enter);
        a.jobs.clear();
        assert!(a.select_card(id));
        press(&mut a, KeyCode::Enter);
        assert!(matches!(a.modal, Modal::Details(_)), "{id}");
        assert!(a.decide.is_none() && a.jobs.is_empty(), "{id}");
    }
}

#[test]
fn requests_time_out_and_replies_are_polled() {
    let mut a = release_app();
    assert!(a.select_card("site-launch-post"));
    press(&mut a, KeyCode::Char('m'));
    press(&mut a, KeyCode::Enter);
    a.jobs.clear();
    let request_id = a
        .tracker
        .get("site-launch-post")
        .unwrap()
        .request_id
        .clone();
    a.apply(Msg::Action(Outcome::Requested {
        card_id: "site-launch-post".into(),
        request_id,
        note_id: "n7".into(),
        warning: None,
    }));
    a.tick();
    assert!(matches!(a.jobs.as_slice(), [Job::Poll]));
    a.jobs.clear();
    a.tick();
    assert!(a.jobs.is_empty(), "polls are spaced out");
    a.set_now(now() + a.config.request_timeout_mins * 60);
    a.tick();
    let Some(Mark::Failed(why)) = a.tracker.mark("site-launch-post") else {
        panic!("no timeout")
    };
    assert!(why.contains("has not picked up note n7"), "{why}");
}
