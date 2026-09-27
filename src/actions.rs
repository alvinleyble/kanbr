//! Acting from the board (decision 14; slice 3 of decision 18).
//!
//! Kanbr never merges, promotes, spawns, or edits the backlog itself. Every
//! action is a request delivered to Firstmate, and Firstmate's own guarded
//! scripts do the real work:
//!
//! - **A drag** to the next column becomes a typed request (`kanbr-request.v1`)
//!   saved as a captain inbox note with `bin/fm-inbox.sh note --request-id ID
//!   --json -`. The note is durable, a retry with the same request id replays
//!   it instead of filing a second one, and saving it wakes Firstmate.
//!   Firstmate answers with `bin/fm-inbox.sh reply`, read back from
//!   `bin/fm-inbox.sh receipts`.
//! - **A decision** held for the captain is answered through Firstmate's one
//!   keyed-answer intake, `bin/fm-captain-hold.sh answers`, in the home that
//!   owns the task (as Captain's Deck does), followed by a note that wakes
//!   Firstmate to act on it. A worker that stopped to ask has no held task, so
//!   its answer is a request note Firstmate relays.
//!
//! What each drag asks for:
//!
//! | Drag | Request |
//! | --- | --- |
//! | Booked to Ready | mark it talked through and ready (lift the hold) |
//! | Ready to Building | a worker; Firstmate recommends two models for the captain to pick |
//! | Building to Dev | the captain's merge word for the card's PR, sent only when its checks are green |
//! | Dev to Staging | promote the project's dev branch to staging |
//! | Staging to Live | promote the project's staging branch to main |
//!
//! A card moves one lane at a time, to the next lane its project uses, and
//! never backwards (that would be a revert). A merge or promotion that lands
//! on a lane listed in `passphrase_lanes` carries that branch's passphrase,
//! typed into a masked prompt; it travels only inside that one request note
//! and is wiped from Kanbr's memory once written (see [`crate::secret`]).
//!
//! Whether a request succeeded is read from Firstmate's state, never assumed:
//! see [`crate::requests`].

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::config::Config;
use crate::dates::{format_utc, now_epoch};
use crate::firstmate::{HOLD_SCRIPT, INBOX_SCRIPT, last_line, run_bounded_input};
use crate::forge::{self, GhError};
use crate::json::{arr_at, bool_at, get, str_at};
use crate::model::{Ask, Board, Card, Column, Owner, assumed_lanes, landing_column};
use crate::secret::Secret;

pub const REQUEST_SCHEMA: &str = "kanbr-request.v1";
pub const NOTE_SCHEMA: &str = "fm-inbox-note.v1";
pub const RECEIPTS_SCHEMA: &str = "fm-inbox-receipts.v1";
/// Provenance recorded with every keyed answer Kanbr feeds the intake.
pub const ANSWER_SOURCE: &str = "kanbr board";
/// The intake refuses this exact answer: it asks for a re-check, it is not an answer.
pub const RESERVED_ANSWER: &str = "reconcile";
/// The most bytes of a decision answer Kanbr sends.
pub const ANSWER_LIMIT: usize = 2000;
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(90);
/// The wake-up line Firstmate's inbox queues shows this many characters of a
/// note, so a passphrase must sit well past it.
const WAKE_SUMMARY_CHARS: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Ready,
    Worker,
    Merge,
    Promote,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::Ready => "ready",
            Action::Worker => "worker",
            Action::Merge => "merge",
            Action::Promote => "promote",
        }
    }

    /// What the card shows while the request is open: `requested: <verb>`.
    pub fn verb(self) -> &'static str {
        match self {
            Action::Ready => "mark ready",
            Action::Worker => "worker",
            Action::Merge => "merge",
            Action::Promote => "promote",
        }
    }
}

/// One drag, checked and described, before it is sent.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub action: Action,
    pub card_id: String,
    pub title: String,
    pub project: String,
    pub owner: Owner,
    pub from: Column,
    pub to: Column,
    pub from_label: String,
    pub to_label: String,
    pub pr_url: Option<String>,
    /// For a merge or promotion: the branch the change lands on.
    pub branch: Option<String>,
    /// For a promotion: the branch it promotes from.
    pub source_branch: Option<String>,
    /// For a promotion: the changes it moves, as shown on the board.
    pub moves: Vec<String>,
    /// For a promotion to Live: database migrations that go live with it.
    pub migrations: Vec<String>,
    /// Unfinished tasks the card waits for.
    pub blocked_by: Vec<String>,
    /// The branch whose passphrase this request carries, when it needs one.
    pub passphrase: Option<String>,
}

impl Plan {
    /// One line naming the request, such as
    /// `promote Leyble-Hub to Staging (dev -> staging)`.
    pub fn headline(&self) -> String {
        match self.action {
            Action::Ready => format!("mark {} talked through and ready", self.card_id),
            Action::Worker => format!("start a worker on {}", self.card_id),
            Action::Merge => format!(
                "merge {} into {}",
                self.pr_url.as_deref().unwrap_or(&self.card_id),
                self.branch.as_deref().unwrap_or("its base")
            ),
            Action::Promote => format!(
                "promote {} to {} ({} -> {})",
                self.project,
                self.to_label,
                self.source_branch.as_deref().unwrap_or("?"),
                self.branch.as_deref().unwrap_or("?")
            ),
        }
    }
}

/// The next lane a card moves to, for its project's lanes.
pub fn next_column(column: Column, lanes: crate::firstmate::Lanes) -> Option<Column> {
    match column {
        Column::Booked => Some(Column::Ready),
        Column::Ready => Some(Column::Building),
        Column::Building => landing_column(lanes),
        Column::Dev if lanes.staging => Some(Column::Staging),
        Column::Dev | Column::Staging => lanes.live.then_some(Column::Live),
        Column::Live => None,
    }
}

fn lane_used(column: Column, lanes: crate::firstmate::Lanes) -> bool {
    match column {
        Column::Dev => lanes.dev,
        Column::Staging => lanes.staging,
        Column::Live => lanes.live,
        _ => true,
    }
}

/// Checks a drag of `card` to `to` and describes the request it makes, or
/// says why Kanbr will not ask for it (the card snaps back with that reason).
pub fn plan(board: &Board, config: &Config, card: &Card, to: Column) -> Result<Plan, String> {
    let label = |c: Column| config.label(c).to_owned();
    let from = card.column;
    if to == from {
        return Err(format!("{} is already in {}", card.id, label(from)));
    }
    if to < from {
        return Err(format!(
            "Kanbr only moves work forward: moving it back from {} to {} would mean a revert. Ask Firstmate if that is what you want",
            label(from),
            label(to)
        ));
    }
    let release = board.release(&card.project);
    let lanes = release.map_or_else(|| assumed_lanes(config), |r| r.lanes);
    let Some(next) = next_column(from, lanes) else {
        return Err(format!(
            "{} is the last lane {} uses",
            label(from),
            card.project
        ));
    };
    if to != next {
        return Err(if lane_used(to, lanes) {
            format!(
                "one lane at a time: {} to {} first",
                label(from),
                label(next)
            )
        } else {
            format!("{} does not use {}", card.project, label(to))
        });
    }
    if card.paused {
        return Err(format!(
            "{} is halted or this item is parked: ask Firstmate to reopen it first",
            card.project
        ));
    }
    let action = match from {
        Column::Booked => Action::Ready,
        Column::Ready => Action::Worker,
        Column::Building => Action::Merge,
        _ => Action::Promote,
    };
    if action == Action::Merge && card.pr_url.is_none() {
        return Err(format!(
            "{} has no PR to merge yet: the merge word is for a PR with green checks",
            card.id
        ));
    }
    if action == Action::Promote && release.is_none() {
        return Err(format!(
            "Kanbr cannot read {}'s branches, so it cannot tell what a promotion would move",
            card.project
        ));
    }
    let lands = matches!(action, Action::Merge | Action::Promote);
    let branch = lands
        .then(|| config.branch(to).to_owned())
        .filter(|b| !b.is_empty());
    let moves = if action == Action::Promote {
        board
            .changes_in(from, Some(&card.project))
            .iter()
            .map(|c| match c.pr_number {
                Some(n) => format!("#{n} {}", c.title),
                None => c.title.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let migrations = match (action, release) {
        (Action::Promote, Some(r)) if to == Column::Live => r.migrations.clone(),
        _ => Vec::new(),
    };
    Ok(Plan {
        action,
        card_id: card.id.clone(),
        title: card.title.clone(),
        project: card.project.clone(),
        owner: card.owner.clone(),
        from,
        to,
        from_label: label(from),
        to_label: label(to),
        pr_url: card.pr_url.clone(),
        passphrase: branch
            .clone()
            .filter(|_| config.passphrase_lanes.contains(&to)),
        branch,
        source_branch: (action == Action::Promote)
            .then(|| config.branch(from).to_owned())
            .filter(|b| !b.is_empty()),
        moves,
        migrations,
        blocked_by: card.blocked_by.clone(),
    })
}

/// A request id Firstmate's inbox accepts (1-128 of `A-Za-z0-9._:-`), unique
/// per request and stable across retries of it.
pub fn request_id(kind: &str, card_id: &str, now: i64) -> String {
    let mut slug: String = card_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let tail = format!("-{now}");
    let room = 128 - "kanbr-".len() - kind.len() - 1 - tail.len();
    slug.truncate(room);
    let slug = slug.trim_matches(|c| c == '-' || c == '.');
    let slug = if slug.is_empty() { "card" } else { slug };
    format!("kanbr-{kind}-{slug}{tail}")
}

/// Text from the board or the captain as one safe line of a request.
fn one_line(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

fn owner_text(owner: &Owner) -> String {
    match owner {
        Owner::Main => "main".to_owned(),
        Owner::Secondmate(m) => format!("second mate {m}"),
    }
}

/// The reply contract every request states, so Firstmate's answer reads back.
fn reply_line(outcome: &str) -> String {
    format!(
        "reply: answer this note with `bin/fm-inbox.sh reply <this note's id> \"done: <what happened>\"` once it has happened, or `\"refused: <reason>\"` if you will not do it; any other reply is shown to the captain while the request stays open. Kanbr shows your reply on the card and {outcome}"
    )
}

/// The request note for a drag. A passphrase, when there is one, is the last
/// line, far past the part of the note Firstmate's wake-up line quotes.
pub fn request_body(
    plan: &Plan,
    request_id: &str,
    checks: Option<&str>,
    passphrase: Option<&Secret>,
) -> Secret {
    let mut t = String::new();
    let _ = writeln!(
        t,
        "Kanbr request ({REQUEST_SCHEMA}): {}",
        one_line(&plan.headline(), 200)
    );
    let _ = writeln!(t, "schema: {REQUEST_SCHEMA}");
    let _ = writeln!(t, "request: {request_id}");
    let _ = writeln!(t, "action: {}", plan.action.name());
    let _ = writeln!(t, "project: {}", one_line(&plan.project, 100));
    let _ = writeln!(t, "card: {}", one_line(&plan.card_id, 200));
    let _ = writeln!(t, "title: {}", one_line(&plan.title, 300));
    let _ = writeln!(t, "owner: {}", owner_text(&plan.owner));
    let _ = writeln!(t, "from: {}", plan.from.title());
    let _ = writeln!(t, "to: {}", plan.to.title());
    if let Some(pr) = &plan.pr_url {
        let _ = writeln!(t, "pr: {}", one_line(pr, 300));
    }
    if let (Some(from), Some(to)) = (&plan.source_branch, &plan.branch) {
        let _ = writeln!(t, "branches: {from} -> {to}");
    } else if let Some(to) = &plan.branch {
        let _ = writeln!(t, "branch: {to}");
    }
    if !plan.blocked_by.is_empty() {
        let _ = writeln!(
            t,
            "waits for: {}",
            one_line(&plan.blocked_by.join(", "), 300)
        );
    }
    if !plan.moves.is_empty() {
        let list: Vec<String> = plan.moves.iter().map(|m| one_line(m, 120)).collect();
        let _ = writeln!(
            t,
            "moves: {} change(s): {}",
            plan.moves.len(),
            one_line(&list.join("; "), 1500)
        );
    }
    if !plan.migrations.is_empty() {
        let _ = writeln!(
            t,
            "migrations: {} database migration(s) go live with it: {}",
            plan.migrations.len(),
            one_line(&plan.migrations.join(", "), 1500)
        );
    }
    if let Some(c) = checks {
        let _ = writeln!(t, "checks: {}", one_line(c, 300));
    }
    let ask = match plan.action {
        Action::Ready => format!(
            "The captain has talked {} through and marks it ready: lift its hold so it is queued ({} -> {}), through your guarded backlog path (for a captain hold, bin/fm-captain-hold.sh answer --release with the captain's words \"talked through; ready\").",
            plan.card_id, plan.from_label, plan.to_label
        ),
        Action::Worker => format!(
            "The captain asks for a worker on {} ({} -> {}). Recommend two models and put the pick to the captain as a captain decision on this task (bin/fm-captain-hold.sh hold), so the captain can answer it in place on the board; then spawn the worker with the model the captain picks.",
            plan.card_id, plan.from_label, plan.to_label
        ),
        Action::Merge => format!(
            "The captain gives the merge word for {} (task {}): merge it now with bin/fm-pr-merge.sh, which re-checks that every check is green ({} -> {}).",
            plan.pr_url.as_deref().unwrap_or("its PR"),
            plan.card_id,
            plan.from_label,
            plan.to_label
        ),
        Action::Promote => format!(
            "The captain asks to promote {} from {} to {} now ({} -> {}) through your guarded promotion path (a promotion PR merged with bin/fm-pr-merge.sh).",
            plan.project,
            plan.source_branch.as_deref().unwrap_or("?"),
            plan.branch.as_deref().unwrap_or("?"),
            plan.from_label,
            plan.to_label
        ),
    };
    let _ = writeln!(t, "ask: {}", one_line(&ask, 1000));
    let _ = writeln!(
        t,
        "{}",
        reply_line(&format!(
            "moves it to {} only when the board sees it there.",
            plan.to_label
        ))
    );
    let mut body = Secret::new();
    body.push_str(&t);
    if let (Some(branch), Some(word)) = (&plan.passphrase, passphrase) {
        // The fields above always run far past the wake-up summary; this
        // keeps it so if they ever shrink.
        while body.chars() < 2 * WAKE_SUMMARY_CHARS {
            body.push_str("-\n");
        }
        body.push_str(&format!(
            "passphrase handling: the last line is the captain's {branch} passphrase for this one merge. Pass it only to bin/fm-pr-merge.sh (--passphrase or FM_MERGE_PASSPHRASE), and never store, log, or repeat it.\n"
        ));
        body.push_str(&format!("passphrase ({branch}): "));
        body.push_str(word.expose());
        body.push_str("\n");
    }
    body
}

/// The keyed-answer line the intake reads: task, answer, label, close mode.
pub fn keyed_line(task: &str, title: &str, answer: &str, release: bool) -> Result<String, String> {
    if task.is_empty()
        || task.len() > 128
        || !task
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "{task} is not a task id the keyed-answer intake accepts; answer it from chat"
        ));
    }
    let answer = one_line(answer, ANSWER_LIMIT);
    let label = one_line(&format!("{title} -> {answer}"), 400);
    Ok(format!(
        "{task}\t{answer}\t{label}\t{}",
        if release { "release" } else { "done" }
    ))
}

/// Checks a decision answer before anything is sent.
pub fn check_answer(answer: &str) -> Result<(), String> {
    let a = answer.trim();
    if a.is_empty() {
        return Err("type an answer first".to_owned());
    }
    if a.eq_ignore_ascii_case(RESERVED_ANSWER) {
        return Err(format!(
            "`{RESERVED_ANSWER}` is reserved: it asks Firstmate to re-check the call, it is not an answer. Ask for that from chat, or answer in other words"
        ));
    }
    if a.len() > ANSWER_LIMIT {
        return Err(format!(
            "keep the answer under {ANSWER_LIMIT} bytes ({} now)",
            a.len()
        ));
    }
    Ok(())
}

/// The note that wakes Firstmate after the intake recorded an answer.
pub fn answered_body(a: &AnswerJob, request_id: &str, detail: &str) -> String {
    let mut t = String::new();
    let _ = writeln!(
        t,
        "Kanbr answer ({REQUEST_SCHEMA}): the captain answered captain call {}",
        a.card_id
    );
    let _ = writeln!(t, "schema: {REQUEST_SCHEMA}");
    let _ = writeln!(t, "request: {request_id}");
    let _ = writeln!(t, "action: answered");
    let _ = writeln!(t, "project: {}", one_line(&a.project, 100));
    let _ = writeln!(t, "card: {}", a.card_id);
    let _ = writeln!(t, "title: {}", one_line(&a.title, 300));
    let _ = writeln!(t, "owner: {}", owner_text(&a.owner));
    let _ = writeln!(
        t,
        "close mode: {}",
        if a.release {
            "release (the held work resumes)"
        } else {
            "done (the call is closed)"
        }
    );
    let _ = writeln!(t, "answer: {}", one_line(&a.answer, ANSWER_LIMIT));
    let _ = writeln!(t, "intake: {}", one_line(detail, 300));
    let _ = writeln!(
        t,
        "ask: The keyed-answer intake (bin/fm-captain-hold.sh answers) already recorded the captain's answer; act on it now{}.",
        match (&a.owner, a.release) {
            (Owner::Secondmate(m), _) => format!(" and relay it to second mate {m}"),
            (Owner::Main, true) => ": the work is released to resume".to_owned(),
            (Owner::Main, false) => String::new(),
        }
    );
    t
}

/// The request note carrying the captain's answer to a worker that stopped to ask.
pub fn worker_answer_body(a: &AnswerJob, questions: &[String], request_id: &str) -> String {
    let mut t = String::new();
    let _ = writeln!(
        t,
        "Kanbr answer ({REQUEST_SCHEMA}): the captain answers {}'s open decision",
        a.card_id
    );
    let _ = writeln!(t, "schema: {REQUEST_SCHEMA}");
    let _ = writeln!(t, "request: {request_id}");
    let _ = writeln!(t, "action: answer");
    let _ = writeln!(t, "project: {}", one_line(&a.project, 100));
    let _ = writeln!(t, "card: {}", a.card_id);
    let _ = writeln!(t, "title: {}", one_line(&a.title, 300));
    let _ = writeln!(t, "owner: {}", owner_text(&a.owner));
    for q in questions {
        let _ = writeln!(t, "question: {}", one_line(q, 300));
    }
    let _ = writeln!(t, "answer: {}", one_line(&a.answer, ANSWER_LIMIT));
    let _ = writeln!(
        t,
        "ask: Relay the captain's answer to the worker and resolve its open decision."
    );
    let _ = writeln!(
        t,
        "{}",
        reply_line("clears the red badge only when the worker's decision is resolved.")
    );
    t
}

// ------------------------------------------------------------- transport

/// A saved request note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub note_id: String,
    /// Firstmate was woken (or had already acknowledged the note).
    pub announced: bool,
}

/// What Firstmate did with one note, from `fm-inbox.sh receipts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub id: String,
    pub acknowledged: bool,
    pub reply: Option<String>,
}

/// A PR's checks, from the forge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Checks {
    pub state: String,
    pub draft: bool,
    pub passed: usize,
    pub pending: Vec<String>,
    pub failed: Vec<String>,
}

impl Checks {
    /// Why the merge word is not given now, or `None` when every check is green.
    pub fn problem(&self) -> Option<String> {
        match self.state.as_str() {
            "OPEN" => {}
            "MERGED" => {
                return Some(
                    "the PR is already merged: the card moves when Firstmate records it".to_owned(),
                );
            }
            other => return Some(format!("the PR is {}", other.to_lowercase())),
        }
        if self.draft {
            return Some("the PR is a draft".to_owned());
        }
        let mut parts = Vec::new();
        if !self.failed.is_empty() {
            parts.push(format!(
                "{} failing ({})",
                self.failed.len(),
                self.failed.join(", ")
            ));
        }
        if !self.pending.is_empty() {
            parts.push(format!(
                "{} still running ({})",
                self.pending.len(),
                self.pending.join(", ")
            ));
        }
        if parts.is_empty() && self.passed == 0 {
            parts.push("no checks reported on the PR yet".to_owned());
        }
        (!parts.is_empty()).then(|| format!("checks are not green: {}", parts.join("; ")))
    }

    pub fn summary(&self) -> String {
        format!("{} check(s) green", self.passed)
    }
}

/// Everything Kanbr asks of Firstmate and the forge, so tests can fake it.
pub trait Transport {
    /// Saves a request note in the board's home and wakes Firstmate.
    fn note(&self, request_id: &str, body: &[u8]) -> Result<Saved, String>;
    /// Reads what Firstmate did with the notes.
    fn receipts(&self) -> Result<Vec<Receipt>, String>;
    /// Feeds one keyed-answer line to the intake of `home` (`None`: the board's home).
    fn answer(&self, home: Option<&Path>, line: &str) -> Result<String, String>;
    /// Reads a PR's checks.
    fn checks(&self, pr_url: &str) -> Result<Checks, String>;
}

/// The real transport: Firstmate's scripts in its home, and `gh`.
pub struct Firstmate {
    pub home: PathBuf,
}

fn script(home: &Path, rel: &str) -> Result<Command, String> {
    let path = home.join(rel);
    if !path.is_file() {
        return Err(format!("{} is missing", path.display()));
    }
    let mut cmd = Command::new(&path);
    cmd.current_dir(home).env("FM_HOME", home);
    Ok(cmd)
}

/// The last line of a script's output, for a one-line reason.
fn script_reason(stdout: &str, stderr: &str) -> String {
    let err = stderr.trim();
    if !err.is_empty() {
        return last_line(err).trim().to_owned();
    }
    let out = stdout.trim();
    if out.is_empty() {
        "no output".to_owned()
    } else {
        last_line(out).trim().to_owned()
    }
}

impl Transport for Firstmate {
    fn note(&self, request_id: &str, body: &[u8]) -> Result<Saved, String> {
        let mut cmd = script(&self.home, INBOX_SCRIPT)?;
        cmd.args(["note", "--request-id", request_id, "--json", "-"]);
        let (status, stdout, stderr) = run_bounded_input(cmd, body, SCRIPT_TIMEOUT)
            .map_err(|e| format!("cannot run {INBOX_SCRIPT}: {e}"))?;
        let Some(status) = status else {
            return Err(format!(
                "{INBOX_SCRIPT} did not finish within {}s",
                SCRIPT_TIMEOUT.as_secs()
            ));
        };
        match status.code() {
            Some(0 | 3) => parse_saved(&stdout),
            _ => Err(format!(
                "{INBOX_SCRIPT} refused the request: {}",
                script_reason(&stdout, &stderr)
            )),
        }
    }

    fn receipts(&self) -> Result<Vec<Receipt>, String> {
        let mut cmd = script(&self.home, INBOX_SCRIPT)?;
        cmd.arg("receipts");
        let (status, stdout, stderr) = run_bounded_input(cmd, b"", SCRIPT_TIMEOUT)
            .map_err(|e| format!("cannot run {INBOX_SCRIPT}: {e}"))?;
        match status {
            Some(s) if s.success() => parse_receipts(&stdout),
            Some(_) => Err(format!(
                "{INBOX_SCRIPT} receipts failed: {}",
                script_reason(&stdout, &stderr)
            )),
            None => Err(format!("{INBOX_SCRIPT} receipts did not finish")),
        }
    }

    fn answer(&self, home: Option<&Path>, line: &str) -> Result<String, String> {
        let home = home.unwrap_or(&self.home);
        let mut cmd = script(home, HOLD_SCRIPT)?;
        cmd.args(["answers", "--source", ANSWER_SOURCE]);
        let input = format!("{line}\n");
        let (status, stdout, stderr) = run_bounded_input(cmd, input.as_bytes(), SCRIPT_TIMEOUT)
            .map_err(|e| format!("cannot run {HOLD_SCRIPT}: {e}"))?;
        let detail = script_reason(&stdout, &stderr);
        match status {
            Some(s) if s.success() => Ok(detail),
            Some(_) => Err(detail),
            None => Err(format!(
                "{HOLD_SCRIPT} answers did not finish within {}s",
                SCRIPT_TIMEOUT.as_secs()
            )),
        }
    }

    fn checks(&self, pr_url: &str) -> Result<Checks, String> {
        match forge::pr_checks(pr_url) {
            Ok(json) => parse_checks(&json),
            Err(GhError::Missing) => Err("gh is not installed".to_owned()),
            Err(GhError::Failed(why)) => Err(format!("gh: {why}")),
        }
    }
}

pub fn parse_saved(stdout: &str) -> Result<Saved, String> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("{INBOX_SCRIPT} printed no receipt ({e})"))?;
    if str_at(&v, "schema") != Some(NOTE_SCHEMA) {
        return Err(format!(
            "{INBOX_SCRIPT} printed an unexpected receipt (want {NOTE_SCHEMA})"
        ));
    }
    let note_id = str_at(&v, "id")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("{INBOX_SCRIPT} receipt names no note id"))?;
    if !bool_at(&v, "saved") {
        return Err(format!("{INBOX_SCRIPT} did not save the note"));
    }
    Ok(Saved {
        note_id: note_id.to_owned(),
        announced: bool_at(&v, "announced") || bool_at(&v, "acknowledged"),
    })
}

/// Each note's id, acknowledgement, and reply. Note bodies are dropped here:
/// Kanbr never keeps or shows them.
pub fn parse_receipts(stdout: &str) -> Result<Vec<Receipt>, String> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("{INBOX_SCRIPT} receipts printed invalid JSON ({e})"))?;
    if str_at(&v, "schema") != Some(RECEIPTS_SCHEMA) {
        return Err(format!(
            "{INBOX_SCRIPT} receipts reports {}; this Kanbr reads {RECEIPTS_SCHEMA}",
            str_at(&v, "schema").unwrap_or("no schema")
        ));
    }
    let mut out = Vec::new();
    for (section, acknowledged) in [("pending", false), ("handled", true)] {
        if !get(&v, section).is_array() {
            return Err(format!("{INBOX_SCRIPT} receipts has no {section} list"));
        }
        for n in arr_at(&v, section) {
            let Some(id) = str_at(n, "id") else { continue };
            out.push(Receipt {
                id: id.to_owned(),
                acknowledged,
                reply: str_at(n, "reply.body").map(str::to_owned),
            });
        }
    }
    Ok(out)
}

pub fn parse_checks(json: &str) -> Result<Checks, String> {
    let v: Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("gh printed invalid JSON ({e})"))?;
    let mut c = Checks {
        state: str_at(&v, "state").unwrap_or("UNKNOWN").to_owned(),
        draft: bool_at(&v, "isDraft"),
        ..Checks::default()
    };
    for item in arr_at(&v, "statusCheckRollup") {
        let name = str_at(item, "name")
            .or_else(|| str_at(item, "context"))
            .unwrap_or("check")
            .to_owned();
        let outcome = match str_at(item, "__typename") {
            Some("StatusContext") => match str_at(item, "state") {
                Some("SUCCESS") => Some(true),
                Some("PENDING" | "EXPECTED") | None => None,
                _ => Some(false),
            },
            _ => match str_at(item, "status") {
                Some("COMPLETED") => Some(matches!(
                    str_at(item, "conclusion"),
                    Some("SUCCESS" | "NEUTRAL" | "SKIPPED")
                )),
                _ => None,
            },
        };
        match outcome {
            Some(true) => c.passed += 1,
            Some(false) => c.failed.push(name),
            None => c.pending.push(name),
        }
    }
    Ok(c)
}

// ------------------------------------------------------------- jobs

/// A decision answer on its way to Firstmate.
#[derive(Clone, Debug, PartialEq)]
pub struct AnswerJob {
    pub card_id: String,
    pub title: String,
    pub project: String,
    pub owner: Owner,
    pub ask: Ask,
    pub answer: String,
    /// For a held task: lift the hold so the work resumes, instead of closing it.
    pub release: bool,
    pub request_id: String,
}

/// Work for the background thread that talks to Firstmate.
#[derive(Debug)]
pub enum Job {
    Request {
        plan: Plan,
        request_id: String,
        passphrase: Option<Secret>,
    },
    Answer(AnswerJob),
    Poll,
}

/// What came of a job.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The request note is saved; `warning` says when Firstmate was not woken.
    Requested {
        card_id: String,
        request_id: String,
        note_id: String,
        warning: Option<String>,
    },
    /// Nothing reached Firstmate, for this reason.
    NotSent {
        card_id: String,
        request_id: String,
        reason: String,
    },
    /// A decision answer: recorded by the intake, or delivered as a request
    /// note (`note_id`) for Firstmate to relay.
    Answered {
        card_id: String,
        request_id: String,
        detail: String,
        note_id: Option<String>,
        warning: Option<String>,
    },
    Receipts(Result<Vec<Receipt>, String>),
}

pub fn run_job(t: &dyn Transport, job: Job) -> Outcome {
    match job {
        Job::Request {
            plan,
            request_id,
            passphrase,
        } => {
            let card_id = plan.card_id.clone();
            let mut checks_line = None;
            if plan.action == Action::Merge {
                let url = plan.pr_url.as_deref().unwrap_or_default();
                match t.checks(url) {
                    Ok(c) => {
                        if let Some(problem) = c.problem() {
                            return Outcome::NotSent {
                                card_id,
                                request_id,
                                reason: format!("{problem}; the merge word waits for green checks"),
                            };
                        }
                        checks_line = Some(format!(
                            "{}, read by Kanbr at {}",
                            c.summary(),
                            format_utc(now_epoch())
                        ));
                    }
                    Err(e) => {
                        return Outcome::NotSent {
                            card_id,
                            request_id,
                            reason: format!(
                                "cannot confirm the PR's checks are green ({e}); Kanbr gives the merge word only on green checks"
                            ),
                        };
                    }
                }
            }
            let body = request_body(
                &plan,
                &request_id,
                checks_line.as_deref(),
                passphrase.as_ref(),
            );
            drop(passphrase);
            let saved = t.note(&request_id, body.bytes());
            drop(body);
            match saved {
                Ok(s) => Outcome::Requested {
                    card_id,
                    request_id,
                    warning: (!s.announced).then(|| {
                        format!(
                            "request saved as note {}, but Firstmate was not woken: it waits for Firstmate's next check",
                            s.note_id
                        )
                    }),
                    note_id: s.note_id,
                },
                Err(reason) => Outcome::NotSent {
                    card_id,
                    request_id,
                    reason,
                },
            }
        }
        Job::Answer(a) => match &a.ask {
            Ask::Hold { home, .. } => {
                let line = match keyed_line(&a.card_id, &a.title, &a.answer, a.release) {
                    Ok(l) => l,
                    Err(reason) => {
                        return Outcome::NotSent {
                            card_id: a.card_id,
                            request_id: a.request_id,
                            reason,
                        };
                    }
                };
                match t.answer(home.as_deref(), &line) {
                    Ok(detail) => {
                        let body = answered_body(&a, &a.request_id, &detail);
                        let warning = match t.note(&a.request_id, body.as_bytes()) {
                            Ok(s) if s.announced => None,
                            Ok(s) => Some(format!(
                                "answer recorded; the wake-up note {} is saved but Firstmate was not woken",
                                s.note_id
                            )),
                            Err(e) => {
                                Some(format!("answer recorded, but Firstmate was not woken: {e}"))
                            }
                        };
                        Outcome::Answered {
                            card_id: a.card_id,
                            request_id: a.request_id,
                            detail,
                            note_id: None,
                            warning,
                        }
                    }
                    Err(reason) => Outcome::NotSent {
                        card_id: a.card_id,
                        request_id: a.request_id,
                        reason: format!("the keyed-answer intake did not take it: {reason}"),
                    },
                }
            }
            Ask::Worker { questions } => {
                let body = worker_answer_body(&a, questions, &a.request_id);
                match t.note(&a.request_id, body.as_bytes()) {
                    Ok(s) => Outcome::Answered {
                        card_id: a.card_id,
                        request_id: a.request_id,
                        detail: format!("sent to Firstmate as note {}", s.note_id),
                        warning: (!s.announced).then(|| {
                            format!("note {} is saved but Firstmate was not woken", s.note_id)
                        }),
                        note_id: Some(s.note_id),
                    },
                    Err(reason) => Outcome::NotSent {
                        card_id: a.card_id,
                        request_id: a.request_id,
                        reason,
                    },
                }
            }
        },
        Job::Poll => Outcome::Receipts(t.receipts()),
    }
}

#[cfg(test)]
mod tests;
