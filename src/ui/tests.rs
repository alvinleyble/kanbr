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
    let (rect, col, idx) = a.hits.cards[0];
    click(&mut a, rect.x + 2, rect.y);
    click(&mut a, rect.x + 2, rect.y);
    assert_eq!((a.col, a.sel[col]), (col, idx));
    assert!(matches!(a.modal, Modal::Details(_)));
}

#[test]
fn cards_fit_their_width() {
    let b = fixture_board();
    for width in [12usize, 24, 40] {
        for c in &b.cards {
            for line in card_lines(c, width, false, now()) {
                assert!(line.width() <= width, "{} at {width}: {:?}", c.id, line);
            }
        }
    }
}

#[test]
fn card_header_has_project_pr_and_second_mate_tag() {
    let b = fixture_board();
    let c = b.cards.iter().find(|c| c.id == "site-footer").unwrap();
    let lines = card_lines(c, 30, false, now());
    let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        header.contains("Site") && header.contains("#15") && header.contains("2nd"),
        "{header}"
    );
    let d = b.cards.iter().find(|c| c.id == "shop-print-queue").unwrap();
    let title: String = card_lines(d, 40, false, now())[1]
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
