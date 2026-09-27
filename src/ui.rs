//! The interactive board: tabs, the waiting-on-you strip, six columns of
//! compact cards with release headers over Staging and Live, a details view,
//! release details, and copyable release notes.
//!
//! Acting from the board (slice 3): Enter on a card waiting on the captain
//! opens its decision; dragging a card to the next column (or `m`) asks
//! Firstmate for that move. Both are requests Firstmate carries out; the card
//! shows "requested" until Firstmate's state shows the outcome (see
//! [`crate::actions`] and [`crate::requests`]).

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::actions::{self, AnswerJob, Job, Outcome, Plan, check_answer, next_column, request_id};
use crate::config::Config;
use crate::dates::now_epoch;
use crate::firstmate::fingerprint;
use crate::loader::Loader;
use crate::model::{Ask, Board, Card, Column, Owner, TestBadge, Tone, assumed_lanes};
use crate::notes::{column_header, details, release_notes};
use crate::requests::{FLASH_SECS, Kind, Mark, Pending, Tracker};
use crate::secret::{self, Secret};

const CARD_ROWS: u16 = 3;
/// Rows above a column's cards: the title, the release line, and a rule.
const HEADER_ROWS: u16 = 3;
const CARD_SLOT: u16 = CARD_ROWS + 1;
const DOUBLE_CLICK: Duration = Duration::from_millis(450);

pub enum Msg {
    Loading,
    Loaded(Box<Result<Board, String>>),
    /// What came of a request sent to Firstmate.
    Action(Outcome),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    None,
    Details(String),
    Help,
    Notices,
    /// Release details for the tab: the Live release and the next release.
    Release,
    /// Plain-language release notes for the tab's Live release, to copy.
    Notes,
    /// Answering a card's decision in place.
    Decide(String),
    /// Confirming a drag request, and typing its passphrase when it needs one.
    Confirm,
}

/// A drag request waiting for the captain's confirmation.
pub struct Confirm {
    pub plan: Plan,
    /// Typed into a masked prompt; never shown, stored, or logged.
    pub passphrase: Secret,
    pub error: Option<String>,
}

/// A decision being answered.
pub struct Decide {
    pub card_id: String,
    pub input: String,
    /// For a held task: lift the hold so the work resumes, instead of closing the call.
    pub release: bool,
    pub error: Option<String>,
}

/// A card being dragged with the mouse.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Drag {
    card_id: String,
    from: usize,
    over: usize,
    moved: bool,
}

#[derive(Default)]
struct Hits {
    tabs: Vec<(Rect, usize)>,
    strip: Option<Rect>,
    columns: Vec<(Rect, usize)>,
    /// Release-column headers, which open the release details.
    headers: Vec<(Rect, usize)>,
    cards: Vec<(Rect, usize, usize)>,
}

pub struct App {
    pub board: Option<Board>,
    pub error: Option<String>,
    pub loading: bool,
    loaded_at: Option<Instant>,
    pub tab: usize,
    tab_name: Option<String>,
    pub col: usize,
    sel: [usize; 6],
    sel_id: [Option<String>; 6],
    scroll: [usize; 6],
    pub modal: Modal,
    modal_scroll: u16,
    home_label: String,
    hits: Hits,
    last_click: Option<(Instant, String)>,
    pub quit: bool,
    force: bool,
    now: Option<i64>,
    /// Column labels from the config, in board order.
    pub labels: [String; 6],
    /// Text waiting to be copied to the clipboard by the run loop.
    pub copy: Option<String>,
    /// The outcome of the last copy, shown in the notes view.
    pub flash: Option<String>,
    pub config: Config,
    /// Open requests and their outcomes.
    pub tracker: Tracker,
    /// Work for the thread that talks to Firstmate, taken by the run loop.
    pub jobs: Vec<Job>,
    pub confirm: Option<Confirm>,
    pub decide: Option<Decide>,
    drag: Option<Drag>,
    /// `q` was pressed once while a request was still being sent.
    quit_armed: bool,
}

impl App {
    pub fn new(home_label: String) -> Self {
        App {
            board: None,
            error: None,
            loading: true,
            loaded_at: None,
            tab: 0,
            tab_name: None,
            col: 0,
            sel: [0; 6],
            sel_id: Default::default(),
            scroll: [0; 6],
            modal: Modal::None,
            modal_scroll: 0,
            home_label,
            hits: Hits::default(),
            last_click: None,
            quit: false,
            force: false,
            now: None,
            labels: Column::ALL.map(|c| c.title().to_owned()),
            copy: None,
            flash: None,
            config: Config::default(),
            tracker: Tracker::default(),
            jobs: Vec::new(),
            confirm: None,
            decide: None,
            drag: None,
            quit_armed: false,
        }
    }

    /// An app showing `board`, with a fixed clock.
    #[cfg(test)]
    pub fn with_board(board: Board, home_label: String, now: i64) -> Self {
        let mut app = App::new(home_label);
        app.now = Some(now);
        app.apply(Msg::Loaded(Box::new(Ok(board))));
        app
    }

    fn now(&self) -> i64 {
        self.now.unwrap_or_else(now_epoch)
    }

    /// Moves the test clock.
    #[cfg(test)]
    pub fn set_now(&mut self, now: i64) {
        self.now = Some(now);
    }

    pub fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Loading => self.loading = true,
            Msg::Loaded(res) => {
                self.loading = false;
                match *res {
                    Ok(board) => {
                        let now = self.now();
                        self.tracker.on_board(&board, now);
                        self.board = Some(board);
                        self.error = None;
                        self.loaded_at = Some(Instant::now());
                        self.resync();
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Action(outcome) => {
                let now = self.now();
                // A recorded answer changes the backlog: read it back now.
                self.force |= matches!(outcome, Outcome::Answered { .. });
                self.tracker.on_outcome(outcome, now);
            }
        }
    }

    /// Times out requests that saw no outcome and schedules reads of
    /// Firstmate's replies. Called by the run loop every turn.
    pub fn tick(&mut self) {
        let now = self.now();
        self.tracker
            .expire(now, self.config.request_timeout_mins.saturating_mul(60));
        if self.tracker.wants_poll(now) {
            self.tracker.polled(now);
            self.jobs.push(Job::Poll);
        }
    }

    /// Tab labels: All first, then each project with open work.
    pub fn tabs(&self) -> Vec<(String, usize, bool)> {
        let mut tabs = vec![("All".to_owned(), 0, false)];
        if let Some(b) = &self.board {
            tabs[0].1 = b.cards.len();
            tabs.extend(
                b.projects
                    .iter()
                    .map(|p| (p.name.clone(), p.count, p.halted)),
            );
        }
        tabs
    }

    pub fn tab_project(&self) -> Option<&str> {
        if self.tab == 0 {
            return None;
        }
        self.board
            .as_ref()
            .and_then(|b| b.projects.get(self.tab - 1))
            .map(|p| p.name.as_str())
    }

    fn column_cards(&self, col: usize) -> Vec<&Card> {
        match &self.board {
            Some(b) => b.column_cards(Column::ALL[col], self.tab_project()),
            None => Vec::new(),
        }
    }

    pub fn selected_card(&self) -> Option<&Card> {
        self.column_cards(self.col).get(self.sel[self.col]).copied()
    }

    fn remember(&mut self, col: usize) {
        let id = self
            .column_cards(col)
            .get(self.sel[col])
            .map(|c| c.id.clone());
        self.sel_id[col] = id;
    }

    /// Keeps the tab and selections on the same projects and cards across refreshes.
    fn resync(&mut self) {
        let tab_count = self.tabs().len();
        self.tab = match &self.tab_name {
            None => 0,
            Some(name) => self
                .board
                .as_ref()
                .and_then(|b| b.projects.iter().position(|p| &p.name == name))
                .map_or(0, |i| i + 1),
        }
        .min(tab_count.saturating_sub(1));
        if self.tab == 0 {
            self.tab_name = None;
        }
        for col in 0..6 {
            let cards = self.column_cards(col);
            let found = self.sel_id[col]
                .as_ref()
                .and_then(|id| cards.iter().position(|c| &c.id == id));
            self.sel[col] = found
                .unwrap_or(self.sel[col])
                .min(cards.len().saturating_sub(1));
            self.remember(col);
        }
        if let Modal::Details(id) = &self.modal
            && !self
                .board
                .as_ref()
                .is_some_and(|b| b.cards.iter().any(|c| &c.id == id))
        {
            self.modal = Modal::None;
        }
        // A decision that is no longer open (answered elsewhere) closes.
        if let Modal::Decide(id) = &self.modal
            && !self
                .board
                .as_ref()
                .is_some_and(|b| b.cards.iter().any(|c| &c.id == id && c.decision))
        {
            let id = id.clone();
            self.modal = Modal::None;
            self.decide = None;
            let now = self.now();
            self.tracker.say(
                format!("{id}: the decision closed before you answered"),
                false,
                now,
            );
        }
    }

    pub fn select_tab(&mut self, tab: usize) {
        if tab >= self.tabs().len() {
            return;
        }
        self.tab = tab;
        self.tab_name = self.tab_project().map(str::to_owned);
        self.sel = [0; 6];
        self.sel_id = Default::default();
        self.scroll = [0; 6];
        self.resync();
    }

    fn move_col(&mut self, delta: isize) {
        self.col = (self.col as isize + delta).clamp(0, 5) as usize;
    }

    fn move_sel(&mut self, delta: isize) {
        let len = self.column_cards(self.col).len();
        if len == 0 {
            return;
        }
        let next = (self.sel[self.col] as isize + delta).clamp(0, len as isize - 1);
        self.sel[self.col] = next as usize;
        self.remember(self.col);
    }

    fn select_card(&mut self, id: &str) -> bool {
        for col in 0..6 {
            if let Some(i) = self.column_cards(col).iter().position(|c| c.id == id) {
                self.col = col;
                self.sel[col] = i;
                self.remember(col);
                return true;
            }
        }
        false
    }

    /// Jumps to the next card waiting on the captain, switching to All when
    /// the current tab has none.
    pub fn jump_waiting(&mut self) {
        let Some(board) = &self.board else { return };
        let project = self.tab_project().map(str::to_owned);
        let mut waiting: Vec<String> = board
            .waiting()
            .iter()
            .filter(|c| project.as_deref().is_none_or(|p| c.project == p))
            .map(|c| c.id.clone())
            .collect();
        if waiting.is_empty() {
            if project.is_none() || board.waiting().is_empty() {
                return;
            }
            self.select_tab(0);
            waiting = self
                .board
                .as_ref()
                .map(|b| b.waiting().iter().map(|c| c.id.clone()).collect())
                .unwrap_or_default();
        }
        let current = self.selected_card().map(|c| c.id.clone());
        let next = match current.and_then(|id| waiting.iter().position(|w| *w == id)) {
            Some(i) => (i + 1) % waiting.len(),
            None => 0,
        };
        let id = waiting[next].clone();
        self.select_card(&id);
    }

    fn open_details(&mut self) {
        if let Some(id) = self.selected_card().map(|c| c.id.clone()) {
            self.modal = Modal::Details(id);
            self.modal_scroll = 0;
        }
    }

    /// Enter on a card: its decision when it waits on the captain and can be
    /// answered in place, else its details.
    fn open_selected(&mut self) {
        let Some(card) = self.selected_card() else {
            return;
        };
        // A move stays open while its decision is answered (a worker's model
        // pick); an answer already on its way blocks another.
        let answerable = card.decision
            && card.ask.is_some()
            && self
                .tracker
                .get(&card.id)
                .is_none_or(|p| matches!(p.kind, Kind::Move { .. }) && p.answering.is_none());
        if !answerable {
            return self.open_details();
        }
        let release = matches!(
            card.ask,
            Some(Ask::Hold {
                work_item: true,
                ..
            })
        );
        let id = card.id.clone();
        self.decide = Some(Decide {
            card_id: id.clone(),
            input: String::new(),
            release,
            error: None,
        });
        self.modal = Modal::Decide(id);
        self.modal_scroll = 0;
    }

    fn lanes_of(&self, project: &str) -> crate::firstmate::Lanes {
        self.board
            .as_ref()
            .and_then(|b| b.release(project))
            .map_or_else(|| assumed_lanes(&self.config), |r| r.lanes)
    }

    /// `m`: asks to move the selected card to its next lane.
    fn request_next(&mut self) {
        let Some(card) = self.selected_card() else {
            return;
        };
        let (id, column, project) = (card.id.clone(), card.column, card.project.clone());
        match next_column(column, self.lanes_of(&project)) {
            Some(to) => self.request_move(&id, to),
            None => {
                let now = self.now();
                let reason = format!(
                    "{} is the last lane {project} uses",
                    self.labels[column.index()]
                );
                self.tracker.refuse(&id, &reason, now);
            }
        }
    }

    /// A drag (or `m`) of card `id` to column `to`: checks it and opens the
    /// confirmation, or snaps the card back with the reason.
    pub fn request_move(&mut self, id: &str, to: Column) {
        let now = self.now();
        let Some(board) = &self.board else { return };
        let Some(card) = board.cards.iter().find(|c| c.id == id) else {
            return;
        };
        if self.tracker.get(id).is_some() {
            self.tracker.say(
                format!("{id}: already requested; waiting for Firstmate"),
                false,
                now,
            );
            return;
        }
        match actions::plan(board, &self.config, card, to) {
            Ok(plan) => {
                self.confirm = Some(Confirm {
                    plan,
                    passphrase: Secret::new(),
                    error: None,
                });
                self.modal = Modal::Confirm;
                self.modal_scroll = 0;
            }
            Err(reason) => self.tracker.refuse(id, &reason, now),
        }
    }

    fn cancel_input(&mut self) {
        // Dropping the confirmation wipes a typed passphrase.
        self.confirm = None;
        self.decide = None;
        self.modal = Modal::None;
    }

    fn send_confirm(&mut self) {
        let Some(mut c) = self.confirm.take() else {
            return;
        };
        if let Some(branch) = &c.plan.passphrase
            && c.passphrase.is_empty()
        {
            c.error = Some(format!("type the {branch} passphrase first"));
            self.confirm = Some(c);
            return;
        }
        let now = self.now();
        let Confirm {
            plan, passphrase, ..
        } = c;
        let rid = request_id(plan.action.name(), &plan.card_id, now);
        self.tracker.start(Pending::new(
            &plan.card_id,
            Kind::Move {
                action: plan.action,
                to: plan.to,
                to_label: plan.to_label.clone(),
            },
            &rid,
            now,
        ));
        let passphrase = plan.passphrase.is_some().then_some(passphrase);
        self.jobs.push(Job::Request {
            plan,
            request_id: rid,
            passphrase,
        });
        self.modal = Modal::None;
    }

    fn send_decide(&mut self) {
        let Some(mut d) = self.decide.take() else {
            return;
        };
        if let Err(e) = check_answer(&d.input) {
            d.error = Some(e);
            self.decide = Some(d);
            return;
        }
        self.modal = Modal::None;
        let now = self.now();
        let Some(card) = self
            .board
            .as_ref()
            .and_then(|b| b.cards.iter().find(|c| c.id == d.card_id))
        else {
            return;
        };
        let Some(ask) = card.ask.clone() else { return };
        let rid = request_id("answer", &card.id, now);
        let job = AnswerJob {
            card_id: card.id.clone(),
            title: card.title.clone(),
            project: card.project.clone(),
            owner: card.owner.clone(),
            release: d.release && matches!(ask, Ask::Hold { .. }),
            ask,
            answer: d.input.trim().to_owned(),
            request_id: rid.clone(),
        };
        let hold = matches!(job.ask, Ask::Hold { .. });
        self.tracker.start_answer(&job.card_id, hold, &rid, now);
        self.jobs.push(Job::Answer(job));
    }

    fn confirm_key(&mut self, key: KeyEvent) {
        let Some(c) = self.confirm.as_mut() else {
            self.modal = Modal::None;
            return;
        };
        let typing = c.plan.passphrase.is_some();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.cancel_input(),
            KeyCode::Enter => self.send_confirm(),
            KeyCode::Char('u') if ctrl && typing => c.passphrase = Secret::new(),
            KeyCode::Backspace if typing => c.passphrase.pop(),
            KeyCode::Char(ch) if typing && !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                c.passphrase.push(ch);
                c.error = None;
            }
            KeyCode::Char('y') if !typing => self.send_confirm(),
            KeyCode::Char('n' | 'q') if !typing => self.cancel_input(),
            KeyCode::Up if !typing => self.modal_scroll = self.modal_scroll.saturating_sub(1),
            KeyCode::Down if !typing => self.modal_scroll = self.modal_scroll.saturating_add(1),
            _ => {}
        }
    }

    fn decide_key(&mut self, key: KeyEvent) {
        let Some(d) = self.decide.as_mut() else {
            self.modal = Modal::None;
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.cancel_input(),
            KeyCode::Enter => self.send_decide(),
            KeyCode::Tab | KeyCode::BackTab => d.release = !d.release,
            KeyCode::Char('u') if ctrl => d.input.clear(),
            KeyCode::Backspace => {
                d.input.pop();
            }
            KeyCode::Up => self.modal_scroll = self.modal_scroll.saturating_sub(1),
            KeyCode::Down => self.modal_scroll = self.modal_scroll.saturating_add(1),
            KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if d.input.len() + ch.len_utf8() <= actions::ANSWER_LIMIT {
                    d.input.push(ch);
                }
                d.error = None;
            }
            _ => {}
        }
    }

    /// Pasted text goes into the open prompt, as one line. The pasted text is
    /// wiped afterwards, since it may be a passphrase.
    pub fn handle_paste(&mut self, text: String) {
        let flat = |ch: char| {
            if matches!(ch, '\r' | '\n' | '\t') {
                ' '
            } else {
                ch
            }
        };
        match self.modal {
            Modal::Confirm => {
                if let Some(c) = self.confirm.as_mut()
                    && c.plan.passphrase.is_some()
                {
                    for ch in text.trim().chars() {
                        c.passphrase.push(flat(ch));
                    }
                    c.error = None;
                }
            }
            Modal::Decide(_) => {
                if let Some(d) = self.decide.as_mut() {
                    for ch in text.chars().map(flat) {
                        if d.input.len() + ch.len_utf8() > actions::ANSWER_LIMIT {
                            break;
                        }
                        d.input.push(ch);
                    }
                    d.error = None;
                }
            }
            _ => {}
        }
        secret::wipe(text);
    }

    fn open_modal(&mut self, modal: Modal) {
        if self.board.is_some() {
            self.modal = modal;
            self.modal_scroll = 0;
            self.flash = None;
        }
    }

    /// Whether the terminal should report mouse events: not while the release
    /// notes are open, so the terminal's own selection can copy them.
    pub fn wants_mouse(&self) -> bool {
        self.modal != Modal::Notes
    }

    /// Whether a refresh was requested since the last call.
    pub fn take_force(&mut self) -> bool {
        std::mem::take(&mut self.force)
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.cancel_input();
            self.quit = true;
            return;
        }
        match self.modal {
            Modal::Confirm => return self.confirm_key(key),
            Modal::Decide(_) => return self.decide_key(key),
            _ => {}
        }
        if self.modal != Modal::None {
            if self.modal == Modal::Notes && key.code == KeyCode::Char('c') {
                self.copy = self
                    .board
                    .as_ref()
                    .map(|b| release_notes(b, self.tab_project()));
                return;
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal_scroll = self.modal_scroll.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.modal_scroll = self.modal_scroll.saturating_add(1)
                }
                KeyCode::PageUp => self.modal_scroll = self.modal_scroll.saturating_sub(10),
                KeyCode::PageDown => self.modal_scroll = self.modal_scroll.saturating_add(10),
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | '?' | 'n' | 'i' | 'R') => {
                    self.modal = Modal::None;
                    self.flash = None;
                }
                _ => {}
            }
            return;
        }
        if key.code != KeyCode::Char('q') {
            self.quit_armed = false;
        }
        match key.code {
            KeyCode::Char('q') => {
                if self.tracker.sending() && !self.quit_armed {
                    self.quit_armed = true;
                    let now = self.now();
                    self.tracker.say(
                        "a request is still being sent to Firstmate; press q again to quit anyway"
                            .to_owned(),
                        false,
                        now,
                    );
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Left | KeyCode::Char('h') => self.move_col(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_col(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-5),
            KeyCode::PageDown => self.move_sel(5),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(isize::MAX / 2),
            KeyCode::Tab => self.select_tab((self.tab + 1) % self.tabs().len()),
            KeyCode::BackTab => {
                let n = self.tabs().len();
                self.select_tab((self.tab + n - 1) % n)
            }
            KeyCode::Char(d @ '0'..='9') => {
                let n = d.to_digit(10).unwrap_or(1) as usize;
                self.select_tab(if n == 0 { 9 } else { n - 1 });
            }
            KeyCode::Enter => self.open_selected(),
            KeyCode::Char('d') => self.open_details(),
            KeyCode::Char('m' | '>') => self.request_next(),
            KeyCode::Char('w') => self.jump_waiting(),
            KeyCode::Char('r') => self.force = true,
            KeyCode::Char('?') => self.modal = Modal::Help,
            KeyCode::Char('i') => self.open_modal(Modal::Release),
            KeyCode::Char('R') => self.open_modal(Modal::Notes),
            KeyCode::Char('n') => {
                let has_notices = self.error.is_some()
                    || self.board.as_ref().is_some_and(|b| !b.notices.is_empty());
                if has_notices {
                    self.modal = Modal::Notices;
                    self.modal_scroll = 0;
                }
            }
            _ => {}
        }
    }

    pub fn handle_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        if matches!(self.modal, Modal::Confirm | Modal::Decide(_)) {
            // A stray click must not throw away a typed answer or passphrase.
            return;
        }
        match m.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                let over = self
                    .hits
                    .columns
                    .iter()
                    .find(|(r, _)| r.contains(pos))
                    .map(|&(_, c)| c);
                if let Some(d) = self.drag.as_mut() {
                    d.moved = true;
                    if let Some(c) = over {
                        d.over = c;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(d) = self.drag.take()
                    && d.moved
                    && d.over != d.from
                {
                    self.last_click = None;
                    self.request_move(&d.card_id, Column::ALL[d.over]);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.drag = None;
                if self.modal != Modal::None {
                    self.modal = Modal::None;
                    return;
                }
                if let Some(&(_, tab)) = self.hits.tabs.iter().find(|(r, _)| r.contains(pos)) {
                    self.select_tab(tab);
                    return;
                }
                if self.hits.strip.is_some_and(|r| r.contains(pos)) {
                    self.jump_waiting();
                    return;
                }
                if let Some(&(_, col)) = self.hits.headers.iter().find(|(r, _)| r.contains(pos)) {
                    self.col = col;
                    self.open_modal(Modal::Release);
                    return;
                }
                if let Some(&(_, col, idx)) =
                    self.hits.cards.iter().find(|(r, _, _)| r.contains(pos))
                {
                    self.col = col;
                    self.sel[col] = idx;
                    self.remember(col);
                    let id = self.sel_id[col].clone().unwrap_or_default();
                    self.drag = Some(Drag {
                        card_id: id.clone(),
                        from: col,
                        over: col,
                        moved: false,
                    });
                    let double = self
                        .last_click
                        .as_ref()
                        .is_some_and(|(at, last)| *last == id && at.elapsed() < DOUBLE_CLICK);
                    if double {
                        self.last_click = None;
                        self.drag = None;
                        self.open_selected();
                    } else {
                        self.last_click = Some((Instant::now(), id));
                    }
                } else if let Some(&(_, col)) =
                    self.hits.columns.iter().find(|(r, _)| r.contains(pos))
                {
                    self.col = col;
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let delta = if m.kind == MouseEventKind::ScrollDown {
                    1
                } else {
                    -1
                };
                if self.modal != Modal::None {
                    self.modal_scroll = (self.modal_scroll as isize + delta).max(0) as u16;
                } else if let Some(&(_, col)) =
                    self.hits.columns.iter().find(|(r, _)| r.contains(pos))
                {
                    self.col = col;
                    self.move_sel(delta);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- rendering

fn text_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncates to `max` display cells, ending in `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if text_width(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max - 1 {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

const PALETTE: [Color; 8] = [
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::LightBlue,
    Color::LightGreen,
    Color::LightMagenta,
    Color::LightCyan,
];

fn project_color(name: &str) -> Color {
    let hash = name.bytes().fold(2_166_136_261_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    });
    PALETTE[hash as usize % PALETTE.len()]
}

fn tone_color(tone: Tone) -> Color {
    match tone {
        Tone::Normal => Color::Gray,
        Tone::Active => Color::Green,
        Tone::Attention => Color::Yellow,
        Tone::Problem => Color::Red,
        Tone::Finished => Color::Blue,
        Tone::Muted => Color::DarkGray,
    }
}

const DIM: Style = Style::new().fg(Color::DarkGray);
const SELECTED_BG: Color = Color::Indexed(237);

/// The three lines of a compact card, fitted to `width` cells. A request
/// `mark` replaces the state on the third line.
pub fn card_lines(
    card: &Card,
    width: usize,
    selected: bool,
    now: i64,
    mark: Option<&Mark>,
) -> Vec<Line<'static>> {
    let greyed = card.paused;
    let plain = card.outside;
    let fg = |c: Color| {
        if greyed {
            Color::DarkGray
        } else if plain {
            Color::Gray
        } else {
            c
        }
    };
    let marker_color = if selected {
        Color::White
    } else if card.decision {
        Color::Red
    } else if plain {
        Color::DarkGray
    } else {
        fg(project_color(&card.project))
    };
    let marker = || Span::styled("▌", Style::new().fg(marker_color));
    let inner = width.saturating_sub(1);

    // Line 1: project, PR number, second-mate tag.
    let mut right = Vec::new();
    if let Some(n) = card.pr_number {
        right.push(Span::styled(
            format!("#{n}"),
            Style::new().fg(fg(Color::Gray)),
        ));
    }
    if card.is_secondmate() {
        right.push(Span::raw(" "));
        right.push(Span::styled(
            "2nd",
            Style::new().fg(Color::Black).bg(fg(Color::LightBlue)),
        ));
    }
    let right_w: usize = right.iter().map(|s| text_width(&s.content)).sum();
    let project = truncate(&card.project, inner.saturating_sub(right_w + 1).max(1));
    let gap = inner.saturating_sub(text_width(&project) + right_w);
    let mut l1 = vec![
        marker(),
        Span::styled(
            project,
            Style::new()
                .fg(fg(project_color(&card.project)))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(gap)),
    ];
    l1.extend(right);

    // Line 2: decision badge, test badge slot, grill tag, dependency badge, title.
    let mut l2 = vec![marker()];
    let mut used = 0;
    let mut badge = |spans: &mut Vec<Span<'static>>, text: &str, style: Style| {
        used += text_width(text);
        spans.push(Span::styled(text.to_owned(), style));
    };
    if card.decision {
        badge(
            &mut l2,
            "⚑ ",
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        );
    }
    match card.test {
        Some(TestBadge::Passed) => badge(&mut l2, "✓ ", Style::new().fg(fg(Color::Green))),
        Some(TestBadge::Failed) => badge(&mut l2, "✗ ", Style::new().fg(fg(Color::Red))),
        Some(TestBadge::Running) => badge(&mut l2, "◐ ", Style::new().fg(fg(Color::Yellow))),
        None => {}
    }
    if card.grill {
        badge(
            &mut l2,
            "grill ",
            Style::new()
                .fg(fg(Color::Magenta))
                .add_modifier(Modifier::BOLD),
        );
    }
    if !card.blocked_by.is_empty() {
        badge(&mut l2, "⧗ ", Style::new().fg(fg(Color::Yellow)));
    }
    let title_style = if greyed {
        DIM
    } else if selected {
        Style::new()
            .fg(if plain { Color::Gray } else { Color::White })
            .add_modifier(Modifier::BOLD)
    } else if plain {
        Style::new().fg(Color::Gray)
    } else {
        Style::new().fg(Color::White)
    };
    l2.push(Span::styled(
        truncate(&card.title, inner.saturating_sub(used)),
        title_style,
    ));

    // Line 3: worker model, elapsed time, state.
    let mut parts: Vec<Span<'static>> = Vec::new();
    for text in [card.model.clone(), card.elapsed(now)]
        .into_iter()
        .flatten()
    {
        parts.push(Span::styled(text, DIM));
        parts.push(Span::styled(" · ", DIM));
    }
    let lead: usize = parts.iter().map(|s| text_width(&s.content)).sum();
    let (state, state_style) = match mark {
        Some(Mark::Open(t)) => (t.as_str(), Style::new().fg(Color::Cyan)),
        Some(Mark::Failed(t)) => (t.as_str(), Style::new().fg(Color::Red)),
        Some(Mark::Done(t)) => (t.as_str(), Style::new().fg(Color::Green)),
        None => (
            card.state.as_str(),
            Style::new().fg(fg(tone_color(card.tone))),
        ),
    };
    if mark.is_some() {
        // The request is what matters now: drop the model and age first.
        parts.clear();
    }
    let lead = if mark.is_some() { 0 } else { lead };
    parts.push(Span::styled(
        truncate(state, inner.saturating_sub(lead)),
        state_style,
    ));
    let mut l3 = vec![marker()];
    let mut w = 0;
    for span in parts {
        let sw = text_width(&span.content);
        if w + sw > inner {
            let rest = truncate(&span.content, inner.saturating_sub(w));
            l3.push(Span::styled(rest, span.style));
            break;
        }
        w += sw;
        l3.push(span);
    }

    let mut lines = vec![Line::from(l1), Line::from(l2), Line::from(l3)];
    if selected {
        for l in &mut lines {
            l.style = Style::new().bg(SELECTED_BG);
        }
    }
    lines
}

fn centered(area: Rect, pct_w: u16, pct_h: u16, min_w: u16, min_h: u16) -> Rect {
    let w = (area.width * pct_w / 100).max(min_w).min(area.width);
    let h = (area.height * pct_h / 100).max(min_h).min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

pub fn render(app: &mut App, f: &mut Frame) {
    let area = f.area();
    app.hits = Hits::default();
    let has_notice =
        app.error.is_some() || app.board.as_ref().is_some_and(|b| !b.notices.is_empty());
    let [tabs_area, strip_area, notice_area, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(u16::from(has_notice)),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_tabs(app, f, tabs_area);
    render_strip(app, f, strip_area);
    if has_notice {
        render_notice(app, f, notice_area);
    }
    if app.board.is_some() {
        render_columns(app, f, body);
    } else {
        let text = match &app.error {
            Some(e) => vec![
                Line::styled(
                    "Kanbr could not read Firstmate",
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                Line::raw(e.clone()),
                Line::raw(""),
                Line::styled(
                    "Run `kanbr doctor` to check every surface Kanbr reads.",
                    DIM,
                ),
            ],
            None => vec![Line::styled(
                format!("Reading Firstmate at {} …", app.home_label),
                DIM,
            )],
        };
        let r = centered(body, 80, 40, 20, 5);
        f.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).centered(),
            r,
        );
    }
    render_footer(app, f, footer);

    match app.modal.clone() {
        Modal::None => {}
        Modal::Details(id) => render_details(app, f, area, &id),
        Modal::Help => render_help(f, area),
        Modal::Notices => render_notices(app, f, area),
        Modal::Release => render_release(app, f, area),
        Modal::Notes => render_notes(app, f, area),
        Modal::Decide(id) => render_decide(app, f, area, &id),
        Modal::Confirm => render_confirm(app, f, area),
    }
}

fn render_tabs(app: &mut App, f: &mut Frame, area: Rect) {
    let brand = " Kanbr ";
    let mut spans = vec![Span::styled(
        brand,
        Style::new()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    let end = area.x + area.width;
    let mut x = area.x + text_width(brand) as u16;
    for (i, (name, count, halted)) in app.tabs().into_iter().enumerate() {
        let key = match i {
            0..=8 => format!("{}", i + 1),
            9 => "0".to_owned(),
            _ => "·".to_owned(),
        };
        let label = format!(" {key} {name} {count} ");
        let w = text_width(&label) as u16;
        if x + 1 + w > end {
            spans.push(Span::styled(" …", DIM));
            break;
        }
        let style = if i == app.tab {
            Style::new()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else if halted {
            DIM
        } else {
            Style::new().fg(Color::Gray)
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label, style));
        app.hits.tabs.push((Rect::new(x + 1, area.y, w, 1), i));
        x += 1 + w;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_strip(app: &mut App, f: &mut Frame, area: Rect) {
    let Some(board) = &app.board else { return };
    let waiting = board.waiting().len();
    let line = if waiting > 0 {
        let here = app
            .tab_project()
            .map(|p| board.waiting().iter().filter(|c| c.project == p).count());
        let mut text = format!(" ⚑ {waiting} waiting on you");
        if let Some(n) = here {
            text.push_str(&format!(" ({n} on this tab)"));
        }
        text.push_str("  ·  w or click to jump ");
        app.hits.strip = Some(area);
        Line::styled(
            text,
            Style::new()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::new().bg(Color::Red))
    } else {
        Line::styled(" ✓ nothing waiting on you", Style::new().fg(Color::Green))
    };
    f.render_widget(Paragraph::new(line), area);
}

fn render_notice(app: &App, f: &mut Frame, area: Rect) {
    let mut text = String::from(" ⚠ ");
    let mut total = 0;
    if let Some(e) = &app.error {
        let age = app
            .loaded_at
            .map(|t| format!(" (showing data from {}s ago)", t.elapsed().as_secs()));
        text.push_str(&format!("refresh failed: {e}{}", age.unwrap_or_default()));
        total += 1;
    }
    if let Some(b) = &app.board {
        if app.error.is_none()
            && let Some(first) = b.notices.first()
        {
            text.push_str(first);
        }
        total += b.notices.len();
    }
    if total > 1 {
        text.push_str(&format!("  (+{} more, n to list)", total - 1));
    }
    f.render_widget(
        Paragraph::new(Line::styled(
            truncate(&text, area.width as usize),
            Style::new().fg(Color::Yellow),
        )),
        area,
    );
}

fn render_columns(app: &mut App, f: &mut Frame, area: Rect) {
    let now = app.now();
    let cols = Layout::horizontal([Constraint::Ratio(1, 6); 6]).split(area);
    for (ci, rect) in cols.iter().enumerate() {
        let last = ci == 5;
        let block = if last {
            Block::new()
        } else {
            Block::new().borders(Borders::RIGHT).border_style(DIM)
        };
        let inner = block.inner(*rect);
        f.render_widget(block, *rect);
        app.hits.columns.push((*rect, ci));
        if inner.height < HEADER_ROWS + 1 || inner.width < 4 {
            continue;
        }
        let cards: Vec<Card> = app.column_cards(ci).into_iter().cloned().collect();
        let focused = ci == app.col;
        let title = app.labels[ci].clone();
        let drop_target = app
            .drag
            .as_ref()
            .is_some_and(|d| d.moved && d.over == ci && d.over != d.from);
        let header_style = if drop_target {
            Style::new()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if focused {
            Style::new()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD)
        };
        let header = Line::from(vec![
            Span::styled(format!(" {title}"), header_style),
            Span::styled(format!(" {}", cards.len()), DIM),
        ]);
        f.render_widget(
            Paragraph::new(header),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let column = Column::ALL[ci];
        let forms = app
            .board
            .as_ref()
            .map(|b| column_header(b, column, app.tab_project()))
            .unwrap_or_default();
        let room = (inner.width as usize).saturating_sub(1);
        if let Some(meta) = forms
            .iter()
            .find(|m| text_width(m) <= room)
            .or(forms.last())
        {
            let style = if meta == "not used" {
                DIM
            } else {
                Style::new().fg(Color::Cyan)
            };
            f.render_widget(
                Paragraph::new(Line::styled(
                    truncate(&format!(" {meta}"), inner.width as usize),
                    style,
                )),
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
            );
        }
        if matches!(column, Column::Dev | Column::Staging | Column::Live) {
            app.hits
                .headers
                .push((Rect::new(inner.x, inner.y, inner.width, 2), ci));
        }
        f.render_widget(
            Paragraph::new(Line::styled("─".repeat(inner.width as usize), DIM)),
            Rect::new(inner.x, inner.y + 2, inner.width, 1),
        );
        let list = Rect::new(
            inner.x,
            inner.y + HEADER_ROWS,
            inner.width,
            inner.height - HEADER_ROWS,
        );
        let fits_all = (list.height + 1) / CARD_SLOT;
        let visible = if cards.len() as u16 > fits_all {
            (list.height / CARD_SLOT).max(1)
        } else {
            fits_all.max(1)
        } as usize;
        let sel = app.sel[ci].min(cards.len().saturating_sub(1));
        let mut scroll = app.scroll[ci].min(cards.len().saturating_sub(visible));
        if sel < scroll {
            scroll = sel;
        } else if sel >= scroll + visible {
            scroll = sel + 1 - visible;
        }
        app.scroll[ci] = scroll;
        for (slot, idx) in (scroll..cards.len().min(scroll + visible)).enumerate() {
            let y = list.y + slot as u16 * CARD_SLOT;
            let h = CARD_ROWS.min(list.y + list.height - y);
            let r = Rect::new(list.x, y, list.width, h);
            let selected = focused && idx == sel;
            let mark = app.tracker.mark(&cards[idx].id);
            let lines = card_lines(
                &cards[idx],
                list.width as usize,
                selected,
                now,
                mark.as_ref(),
            );
            f.render_widget(Paragraph::new(lines), r);
            app.hits.cards.push((r, ci, idx));
        }
        let below = cards.len().saturating_sub(scroll + visible);
        if below > 0 || scroll > 0 {
            let mut t = String::new();
            if scroll > 0 {
                t.push_str(&format!(" ▲{scroll}"));
            }
            if below > 0 {
                t.push_str(&format!(" ▼{below} more"));
            }
            let y = list.y + list.height - 1;
            f.render_widget(
                Paragraph::new(Line::styled(t, DIM)),
                Rect::new(list.x, y, list.width, 1),
            );
        }
    }
}

fn render_footer(app: &App, f: &mut Frame, area: Rect) {
    let now = app.now();
    let dragging = app.drag.as_ref().filter(|d| d.moved);
    let flash = app
        .tracker
        .flash
        .as_ref()
        .filter(|(_, _, at)| now - at < FLASH_SECS);
    let (keys, keys_style) = if let Some(d) = dragging {
        let text = if d.over == d.from {
            " drag to the next column and release to ask Firstmate for the move".to_owned()
        } else {
            format!(
                " release to ask Firstmate to move it to {}",
                app.labels[d.over]
            )
        };
        (text, Style::new().fg(Color::Cyan))
    } else if let Some((text, ok, _)) = flash {
        (
            format!(" {text}"),
            Style::new().fg(if *ok { Color::Green } else { Color::Red }),
        )
    } else {
        (
            " ←→ column  ↑↓ card  enter details/answer  m move  w waiting  1-9 tab  i release  R notes  r refresh  ? help  q quit".to_owned(),
            DIM,
        )
    };
    let status = if app.loading {
        "refreshing…".to_owned()
    } else if let Some(t) = app.loaded_at {
        format!("updated {}s ago", t.elapsed().as_secs())
    } else {
        String::new()
    };
    let status = format!("{status} ");
    let room = (area.width as usize).saturating_sub(text_width(&status) + 1);
    let keys = truncate(&keys, room);
    let gap = (area.width as usize).saturating_sub(text_width(&keys) + text_width(&status));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(keys, keys_style),
            Span::raw(" ".repeat(gap)),
            Span::styled(status, DIM),
        ])),
        area,
    );
}

fn modal_block(title: String) -> Block<'static> {
    Block::bordered()
        .title(Span::styled(
            title,
            Style::new().add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::new().fg(Color::Cyan))
}

fn render_details(app: &App, f: &mut Frame, area: Rect, id: &str) {
    let Some(card) = app
        .board
        .as_ref()
        .and_then(|b| b.cards.iter().find(|c| c.id == id))
    else {
        return;
    };
    let now = app.now();
    let label = Style::new().fg(Color::Cyan);
    let mut lines = vec![
        Line::styled(
            card.title.clone(),
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    let mut row = |k: &str, v: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("{k:<13} "), label),
            Span::raw(v),
        ]));
    };
    row("Project", card.project.clone());
    row("Column", app.labels[card.column.index()].clone());
    row("State", card.state.clone());
    if let Owner::Secondmate(m) = &card.owner {
        row("Owner", format!("second mate {m}"));
    }
    if card.decision {
        row("Waiting", "on you: a captain decision".to_owned());
    }
    if card.grill {
        row("Grill", "needs a grill".to_owned());
    }
    if card.paused {
        row("Paused", "project halted or item parked".to_owned());
    }
    if let Some(e) = card.elapsed(now) {
        row(
            if card.column == Column::Building {
                "Running"
            } else {
                "Age"
            },
            e,
        );
    }
    if let Some(url) = &card.pr_url {
        row("PR", url.clone());
    }
    for (k, v) in &card.details {
        row(k, v.clone());
    }
    for (k, v) in app.tracker.detail_rows(&card.id, now) {
        row(k, v);
    }
    let r = centered(area, 80, 80, 40, 10);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .block(modal_block(format!(" {} · esc to close ", card.id)))
            .wrap(Wrap { trim: false })
            .scroll((app.modal_scroll, 0)),
        r,
    );
}

fn render_help(f: &mut Frame, area: Rect) {
    let rows = [
        ("← → / h l", "move between columns"),
        ("↑ ↓ / j k", "move between cards"),
        ("pgup pgdn", "move five cards"),
        ("g G / home end", "first or last card"),
        (
            "enter / double-click",
            "answer a ⚑ decision in place, else card details",
        ),
        ("d", "card details"),
        (
            "drag / m / >",
            "ask Firstmate to move the card to the next column",
        ),
        ("1-9, 0 / tab / click", "switch project tab (1 is All)"),
        (
            "w / click the strip",
            "jump to the next card waiting on you",
        ),
        ("n", "list notices (what the board could not show)"),
        (
            "i / click a lane header",
            "release details: Live release, next release, database changes",
        ),
        ("R", "release notes for the Live release (c copies them)"),
        ("r", "refresh now"),
        ("q", "quit"),
    ];
    let mut lines = vec![
        Line::styled(
            "Kanbr never changes Firstmate itself: an answer or a drag is a request Firstmate carries out through its own guarded scripts. The card shows \"requested\" and moves only when that succeeds; otherwise it snaps back with the reason.",
            DIM,
        ),
        Line::raw(""),
    ];
    for (k, v) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("{k:<22}"), Style::new().fg(Color::Cyan)),
            Span::raw(v),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("⚑ ", Style::new().fg(Color::Red)),
        Span::raw("waiting on you   "),
        Span::styled("grill ", Style::new().fg(Color::Magenta)),
        Span::raw("needs a grill   "),
        Span::styled("⧗ ", Style::new().fg(Color::Yellow)),
        Span::raw("waits for another task   "),
        Span::styled("2nd", Style::new().fg(Color::Black).bg(Color::LightBlue)),
        Span::raw(" second mate's work"),
    ]));
    lines.push(Line::from(vec![
        Span::styled("grey card", Style::new().fg(Color::Gray)),
        Span::raw(" a merged change with no Firstmate card (hand-opened PR or direct commit)"),
    ]));
    lines.push(Line::styled(
        "Dev, Staging, and Live come from git: each change sits in the furthest branch it has reached, and Live shows only the latest release. The board reads each clone as of its last fetch.",
        DIM,
    ));
    let r = centered(area, 70, 80, 50, 28);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .block(modal_block(" Kanbr help · esc to close ".to_owned()))
            .wrap(Wrap { trim: false }),
        r,
    );
}

fn render_notices(app: &App, f: &mut Frame, area: Rect) {
    let mut lines = Vec::new();
    if let Some(e) = &app.error {
        lines.push(Line::styled(
            format!("• refresh failed: {e}"),
            Style::new().fg(Color::Red),
        ));
    }
    if let Some(b) = &app.board {
        lines.extend(b.notices.iter().map(|n| Line::raw(format!("• {n}"))));
    }
    let r = centered(area, 80, 60, 40, 8);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .block(modal_block(" Notices · esc to close ".to_owned()))
            .wrap(Wrap { trim: false })
            .scroll((app.modal_scroll, 0)),
        r,
    );
}

fn render_release(app: &App, f: &mut Frame, area: Rect) {
    let Some(board) = &app.board else { return };
    let label = Style::new().fg(Color::Cyan);
    let mut lines = Vec::new();
    for d in details(board, app.tab_project(), app.now()) {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            d.project,
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ));
        for (k, v) in d.rows {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<13} "), label),
                Span::raw(v),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::raw(
            "No release information: Kanbr found no readable repository with a Dev, Staging, or Live branch for the projects on this tab.",
        ));
    }
    let r = centered(area, 80, 70, 40, 10);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .block(modal_block(
                " Releases · R release notes · esc to close ".to_owned(),
            ))
            .wrap(Wrap { trim: false })
            .scroll((app.modal_scroll, 0)),
        r,
    );
}

fn render_notes(app: &App, f: &mut Frame, area: Rect) {
    let Some(board) = &app.board else { return };
    let text = release_notes(board, app.tab_project());
    let lines: Vec<Line> = text.lines().map(|l| Line::raw(l.to_owned())).collect();
    // Full width, with top and bottom rules only, so selecting the text copies
    // no borders and no board beside it.
    let r = centered(area, 100, 80, 40, 10);
    let mut block = Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .padding(Padding::horizontal(2))
        .border_style(Style::new().fg(Color::Cyan))
        .title(Span::styled(
            " Release notes · c copy · select with the mouse · esc to close ",
            Style::new().add_modifier(Modifier::BOLD),
        ));
    if let Some(flash) = &app.flash {
        block = block.title_bottom(Span::styled(
            format!(" {flash} "),
            Style::new().fg(Color::Green),
        ));
    }
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((app.modal_scroll, 0)),
        r,
    );
}

fn render_confirm(app: &App, f: &mut Frame, area: Rect) {
    let Some(c) = &app.confirm else { return };
    let p = &c.plan;
    let label = Style::new().fg(Color::Cyan);
    let mut lines = vec![
        Line::styled(
            format!("Ask Firstmate to {}", p.headline()),
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    let mut row = |k: &str, v: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("{k:<11} "), label),
            Span::raw(v),
        ]));
    };
    row("Card", format!("{} · {}", p.card_id, p.title));
    row("Move", format!("{} → {}", p.from_label, p.to_label));
    if let Owner::Secondmate(m) = &p.owner {
        row("Owner", format!("second mate {m}"));
    }
    if !p.blocked_by.is_empty() {
        row("Waits for", p.blocked_by.join(", "));
    }
    match p.action {
        actions::Action::Ready => row(
            "Asks",
            "mark it talked through and ready: Firstmate lifts its hold".to_owned(),
        ),
        actions::Action::Worker => row(
            "Asks",
            "a worker: Firstmate recommends two models and puts the pick to you as a decision on this card".to_owned(),
        ),
        actions::Action::Merge => {
            row("PR", p.pr_url.clone().unwrap_or_default());
            row(
                "Asks",
                "your merge word for this PR. Kanbr reads its checks first and sends the word only when every check is green".to_owned(),
            );
        }
        actions::Action::Promote => {
            row(
                "Branches",
                format!(
                    "{} → {}",
                    p.source_branch.as_deref().unwrap_or("?"),
                    p.branch.as_deref().unwrap_or("?")
                ),
            );
            row(
                "Moves",
                format!(
                    "{} change(s): the promotion takes everything in {}",
                    p.moves.len(),
                    p.from_label
                ),
            );
            for m in p.moves.iter().take(8) {
                row("", format!("· {m}"));
            }
            if p.moves.len() > 8 {
                row("", format!("… and {} more", p.moves.len() - 8));
            }
            if !p.migrations.is_empty() {
                row(
                    "Migrations",
                    format!(
                        "{} database migration(s) go live with it: {}",
                        p.migrations.len(),
                        p.migrations.join(", ")
                    ),
                );
            }
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Firstmate does the real work through its guarded scripts. The card shows \"requested\" and moves only when that succeeds; otherwise it snaps back with the reason.",
        DIM,
    ));
    if let Some(branch) = &p.passphrase {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{branch} passphrase: "),
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "•".repeat(c.passphrase.chars().min(64)),
                Style::new().fg(Color::White),
            ),
            Span::styled("█", DIM),
        ]));
        lines.push(Line::styled(
            "Masked. It travels only inside this one request to Firstmate; Kanbr never stores, logs, or shows it.",
            DIM,
        ));
    }
    if let Some(e) = &c.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(e.clone(), Style::new().fg(Color::Red)));
    }
    let title = if p.passphrase.is_some() {
        " Request · type the passphrase · enter to send · esc to cancel "
    } else {
        " Request · enter to send · esc to cancel "
    };
    let r = centered(area, 80, 70, 40, 14);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .block(modal_block(title.to_owned()))
            .wrap(Wrap { trim: false })
            .scroll((app.modal_scroll, 0)),
        r,
    );
}

fn render_decide(app: &App, f: &mut Frame, area: Rect, id: &str) {
    let (Some(d), Some(card)) = (
        &app.decide,
        app.board
            .as_ref()
            .and_then(|b| b.cards.iter().find(|c| c.id == id)),
    ) else {
        return;
    };
    let label = Style::new().fg(Color::Cyan);
    let mut lines = vec![
        Line::styled(
            card.title.clone(),
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    let mut row = |k: &str, v: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("{k:<11} "), label),
            Span::raw(v),
        ]));
    };
    row("Project", card.project.clone());
    if let Owner::Secondmate(m) = &card.owner {
        row("Owner", format!("second mate {m}"));
    }
    let detail = |key: &str| {
        card.details
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    };
    let hold = matches!(card.ask, Some(Ask::Hold { .. }));
    match &card.ask {
        Some(Ask::Hold { .. }) => {
            row(
                "Question",
                detail("Hold").unwrap_or_else(|| "held for your call".to_owned()),
            );
        }
        Some(Ask::Worker { questions }) if !questions.is_empty() => {
            for q in questions {
                row("Question", q.clone());
            }
        }
        _ => row(
            "Question",
            "the worker is waiting on your decision".to_owned(),
        ),
    }
    if let Some(n) = detail("Notes") {
        row("Notes", n);
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Your answer, in your own words:",
        Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::from(vec![
        Span::styled("> ", label),
        Span::raw(d.input.clone()),
        Span::styled("█", DIM),
    ]));
    lines.push(Line::raw(""));
    let dot = |on: bool| if on { "● " } else { "○ " };
    if hold {
        lines.push(Line::from(vec![
            Span::styled("Tab        ", label),
            Span::styled(
                format!("{}resume the work (release)", dot(d.release)),
                if d.release {
                    Style::new().fg(Color::White)
                } else {
                    DIM
                },
            ),
            Span::raw("   "),
            Span::styled(
                format!("{}close the call (done)", dot(!d.release)),
                if d.release {
                    DIM
                } else {
                    Style::new().fg(Color::White)
                },
            ),
        ]));
        let home = match &card.ask {
            Some(Ask::Hold { home: Some(h), .. }) => h.display().to_string(),
            _ => app.home_label.clone(),
        };
        lines.push(Line::styled(
            format!(
                "Goes to Firstmate's keyed-answer intake (bin/fm-captain-hold.sh answers) in {home}, and Firstmate is woken to act on it."
            ),
            DIM,
        ));
    } else {
        lines.push(Line::styled(
            "Goes to Firstmate as a request note; Firstmate relays it to the worker, and the badge clears when the worker's decision is resolved.",
            DIM,
        ));
    }
    if let Some(e) = &d.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(e.clone(), Style::new().fg(Color::Red)));
    }
    let r = centered(area, 80, 70, 40, 14);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .block(modal_block(format!(
                " ⚑ {} · enter to send · esc to cancel ",
                card.id
            )))
            .wrap(Wrap { trim: false })
            .scroll((app.modal_scroll, 0)),
        r,
    );
}

// ---------------------------------------------------------------- clipboard

/// Copies text with the platform clipboard tool, falling back to the OSC 52
/// terminal escape (which Herdr and most terminals pass to the clipboard).
fn copy_to_clipboard(text: &str) -> String {
    let tools: [(&str, &[&str]); 4] = [
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (tool, args) in tools {
        let Ok(mut child) = Command::new(tool)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut i| i.write_all(text.as_bytes()).is_ok());
        let started = Instant::now();
        let ok = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if started.elapsed() < Duration::from_secs(2) => {
                    thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        if wrote && ok {
            return format!("copied with {tool}");
        }
    }
    let mut out = io::stdout();
    let sent = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes())).and_then(|()| out.flush());
    match sent {
        Ok(()) => "sent to the terminal clipboard (OSC 52)".to_owned(),
        Err(e) => format!("copy failed: {e}"),
    }
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---------------------------------------------------------------- run loop

/// Polls the Firstmate home cheaply every `interval`, and re-reads the full
/// snapshot when a source file changed, when `refresh` elapsed, or on demand.
fn spawn_refresher(loader: Loader, tx: Sender<Msg>, force: Receiver<()>) {
    let interval = Duration::from_secs(loader.config.interval_secs);
    let refresh = Duration::from_secs(loader.config.refresh_secs);
    thread::spawn(move || {
        let mut last: Option<(u64, Instant)> = None;
        loop {
            let fp = fingerprint(&loader.home);
            let due = last.is_none_or(|(f, at)| f != fp || at.elapsed() >= refresh);
            if due {
                if tx.send(Msg::Loading).is_err() {
                    return;
                }
                let res = loader.load();
                last = Some((fp, Instant::now()));
                if tx.send(Msg::Loaded(Box::new(res))).is_err() {
                    return;
                }
            }
            match force.recv_timeout(interval) {
                Ok(()) => {
                    while force.try_recv().is_ok() {}
                    last = None;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
}

/// The thread that talks to Firstmate: sends requests and answers, reads
/// replies, and reports each outcome back to the board.
fn spawn_actions(home: PathBuf, tx: Sender<Msg>) -> Sender<Job> {
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    thread::spawn(move || {
        let transport = actions::Firstmate { home };
        for job in job_rx {
            let outcome = actions::run_job(&transport, job);
            if tx.send(Msg::Action(outcome)).is_err() {
                return;
            }
        }
    });
    job_tx
}

pub fn run(loader: Loader) -> io::Result<()> {
    let home_label = loader.home.display().to_string();
    let labels = loader.config.labels.clone();
    let config = loader.config.clone();
    let (tx, rx) = mpsc::channel();
    let (force_tx, force_rx) = mpsc::channel();
    let jobs = spawn_actions(loader.home.clone(), tx.clone());
    spawn_refresher(loader, tx, force_rx);

    let mut terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let restore_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
        restore_hook(info);
    }));

    let mut app = App::new(home_label);
    app.labels = labels;
    app.config = config;
    let mut mouse = true;
    let result = (|| -> io::Result<()> {
        loop {
            while let Ok(msg) = rx.try_recv() {
                app.apply(msg);
            }
            app.tick();
            for job in app.jobs.drain(..) {
                let _ = jobs.send(job);
            }
            if app.wants_mouse() != mouse {
                mouse = app.wants_mouse();
                if mouse {
                    execute!(io::stdout(), EnableMouseCapture)?;
                } else {
                    execute!(io::stdout(), DisableMouseCapture)?;
                }
            }
            if let Some(text) = app.copy.take() {
                app.flash = Some(copy_to_clipboard(&text));
            }
            terminal.draw(|f| render(&mut app, f))?;
            if event::poll(Duration::from_millis(250))? {
                match event::read()? {
                    Event::Key(k) => app.handle_key(k),
                    Event::Mouse(m) => app.handle_mouse(m),
                    Event::Paste(text) => app.handle_paste(text),
                    _ => {}
                }
            }
            if app.take_force() {
                let _ = force_tx.send(());
            }
            if app.quit {
                return Ok(());
            }
        }
    })();
    let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests;
