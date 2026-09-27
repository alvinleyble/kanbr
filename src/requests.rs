//! Open requests and what came of them.
//!
//! A card shows "requested" from the moment its request is sent until
//! Firstmate's outcome is visible in Firstmate's own state:
//!
//! - **It happened**: the board (backlog, worker state, PR state, or git)
//!   shows the card in the target column or beyond, or, for an answer, the
//!   card no longer waits on the captain. Only then does the card move.
//! - **Refused**: Firstmate replied to the request note with `refused:` (or
//!   `failed:` or `declined:`), or Kanbr could not send it. The card snaps back
//!   with the reason.
//! - **Timed out**: nothing happened within `request_timeout` minutes. The
//!   card snaps back saying whether Firstmate ever picked the note up.
//!
//! A `done:` reply alone does not move a card: it waits until the board sees
//! the change (a promotion shows only once the project's clone has fetched).
//! Requests live only in memory; the notes themselves stay in Firstmate's
//! inbox.

use crate::actions::{Action, Outcome, Receipt};
use crate::dates::format_elapsed;
use crate::model::{Board, Column};

/// Seconds between reads of Firstmate's replies while a request is open.
pub const POLL_SECS: i64 = 10;
/// How long a settled outcome stays on its card.
const DONE_SHOW_SECS: i64 = 60;
const FAILED_SHOW_SECS: i64 = 600;
/// How long an outcome message stays in the footer.
pub const FLASH_SECS: i64 = 12;
/// The most characters of a reply shown on a card.
const REPLY_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Move {
        action: Action,
        to: Column,
        to_label: String,
    },
    Answer {
        /// Recorded by the keyed-answer intake, not relayed by Firstmate.
        hold: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub card_id: String,
    pub kind: Kind,
    pub request_id: String,
    pub note_id: Option<String>,
    pub sent_at: i64,
    /// Still being written to Firstmate.
    pub sending: bool,
    /// Firstmate has picked the note up.
    pub seen: bool,
    /// Firstmate's reply so far.
    pub reply: Option<String>,
    /// Firstmate replied `done:`; the board has not caught up yet.
    pub reported_done: bool,
    pub warning: Option<String>,
    /// The request id of a decision answer sent while this move stays open,
    /// until its outcome is back.
    pub answering: Option<String>,
}

impl Pending {
    pub fn new(card_id: &str, kind: Kind, request_id: &str, now: i64) -> Self {
        Pending {
            card_id: card_id.to_owned(),
            kind,
            request_id: request_id.to_owned(),
            note_id: None,
            sent_at: now,
            sending: true,
            seen: false,
            reply: None,
            reported_done: false,
            warning: None,
            answering: None,
        }
    }

    fn what(&self) -> String {
        match &self.kind {
            Kind::Move { action, .. } => action.verb().to_owned(),
            Kind::Answer { .. } => "answer".to_owned(),
        }
    }
}

/// What a card shows about its request, in place of its state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Open(String),
    Failed(String),
    Done(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyKind {
    Done,
    Refused,
    Info,
}

/// How Kanbr reads a reply: `done:` and `refused:` settle the request (a done
/// still waits for the board); anything else is shown while it stays open.
pub fn reply_kind(reply: &str) -> ReplyKind {
    let r = reply.trim_start().to_lowercase();
    if r.starts_with("done") {
        ReplyKind::Done
    } else if ["refused", "failed", "declined"]
        .iter()
        .any(|p| r.starts_with(p))
    {
        ReplyKind::Refused
    } else {
        ReplyKind::Info
    }
}

fn short_reply(reply: &str) -> String {
    let first = reply.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let first = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if first.chars().count() <= REPLY_CHARS {
        first
    } else {
        let mut s: String = first.chars().take(REPLY_CHARS - 1).collect();
        s.push('…');
        s
    }
}

#[derive(Clone, Debug)]
struct Settled {
    card_id: String,
    mark: Mark,
    at: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Tracker {
    pub pending: Vec<Pending>,
    settled: Vec<Settled>,
    last_poll: Option<i64>,
    /// The last time Firstmate's replies could not be read, and why.
    pub poll_error: Option<String>,
    /// The latest outcome, for the footer: text, whether it went well, when.
    pub flash: Option<(String, bool, i64)>,
}

impl Tracker {
    pub fn get(&self, card_id: &str) -> Option<&Pending> {
        self.pending.iter().find(|p| p.card_id == card_id)
    }

    fn get_mut(&mut self, card_id: &str) -> Option<&mut Pending> {
        self.pending.iter_mut().find(|p| p.card_id == card_id)
    }

    fn by_request_mut(&mut self, request_id: &str) -> Option<&mut Pending> {
        self.pending.iter_mut().find(|p| p.request_id == request_id)
    }

    /// Whether a request is still being written to Firstmate.
    pub fn sending(&self) -> bool {
        self.pending
            .iter()
            .any(|p| p.sending || p.answering.is_some())
    }

    pub fn start(&mut self, p: Pending) {
        self.settled.retain(|s| s.card_id != p.card_id);
        self.pending.retain(|q| q.card_id != p.card_id);
        self.pending.push(p);
    }

    /// Starts an answer to `card_id`'s decision. A move still open for the
    /// card stays open (the answer is its worker's model pick); the answer's
    /// outcome then shows in the footer.
    pub fn start_answer(&mut self, card_id: &str, hold: bool, request_id: &str, now: i64) {
        match self.get_mut(card_id) {
            Some(p) => p.answering = Some(request_id.to_owned()),
            None => self.start(Pending::new(
                card_id,
                Kind::Answer { hold },
                request_id,
                now,
            )),
        }
    }

    fn answered_alongside(&mut self, request_id: &str) {
        for p in &mut self.pending {
            if p.answering.as_deref() == Some(request_id) {
                p.answering = None;
            }
        }
    }

    fn settle(&mut self, card_id: &str, mark: Mark, now: i64) {
        self.pending.retain(|p| p.card_id != card_id);
        self.settled.retain(|s| s.card_id != card_id);
        let (text, ok) = match &mark {
            Mark::Done(t) => (t.clone(), true),
            Mark::Failed(t) => (t.clone(), false),
            Mark::Open(t) => (t.clone(), true),
        };
        self.flash = Some((format!("{card_id}: {text}"), ok, now));
        self.settled.push(Settled {
            card_id: card_id.to_owned(),
            mark,
            at: now,
        });
    }

    pub fn say(&mut self, text: String, ok: bool, now: i64) {
        self.flash = Some((text, ok, now));
    }

    /// Shows a drag Kanbr would not ask for: the card snaps back with the reason.
    pub fn refuse(&mut self, card_id: &str, reason: &str, now: i64) {
        self.settle(card_id, Mark::Failed(format!("↩ {reason}")), now);
    }

    pub fn on_outcome(&mut self, outcome: Outcome, now: i64) {
        match outcome {
            Outcome::Requested {
                card_id,
                request_id,
                note_id,
                warning,
            } => {
                let Some(p) = self.by_request_mut(&request_id) else {
                    return;
                };
                p.sending = false;
                p.note_id = Some(note_id.clone());
                p.warning = warning.clone();
                let what = p.what();
                match warning {
                    Some(w) => self.say(format!("{card_id}: requested {what}; {w}"), false, now),
                    None => self.say(
                        format!(
                            "{card_id}: requested {what} (note {note_id}); Firstmate was woken"
                        ),
                        true,
                        now,
                    ),
                }
            }
            Outcome::NotSent {
                card_id,
                request_id,
                reason,
            } => {
                if self.by_request_mut(&request_id).is_some() {
                    self.refuse(&card_id, &reason, now);
                } else {
                    self.answered_alongside(&request_id);
                    self.say(format!("{card_id}: not sent: {reason}"), false, now);
                }
            }
            Outcome::Answered {
                card_id,
                request_id,
                detail,
                note_id,
                warning,
            } => {
                let text = match &warning {
                    Some(w) => format!("{card_id}: answered; {w}"),
                    None => format!("{card_id}: answered ({detail})"),
                };
                match self.by_request_mut(&request_id) {
                    Some(p) => {
                        p.sending = false;
                        p.note_id = note_id;
                        p.warning = warning;
                    }
                    None => self.answered_alongside(&request_id),
                }
                self.say(text, true, now);
            }
            Outcome::Receipts(Ok(receipts)) => {
                self.poll_error = None;
                self.on_receipts(&receipts, now);
            }
            Outcome::Receipts(Err(e)) => self.poll_error = Some(e),
        }
    }

    pub fn on_receipts(&mut self, receipts: &[Receipt], now: i64) {
        let mut refused = Vec::new();
        for p in &mut self.pending {
            let Some(note) = &p.note_id else { continue };
            let Some(r) = receipts.iter().find(|r| &r.id == note) else {
                continue;
            };
            p.seen |= r.acknowledged;
            let Some(reply) = &r.reply else { continue };
            p.seen = true;
            match reply_kind(reply) {
                ReplyKind::Refused => {
                    refused.push((p.card_id.clone(), short_reply(reply)));
                }
                ReplyKind::Done => {
                    p.reported_done = true;
                    p.reply = Some(short_reply(reply));
                }
                ReplyKind::Info => p.reply = Some(short_reply(reply)),
            }
        }
        for (card, reply) in refused {
            self.settle(&card, Mark::Failed(format!("↩ Firstmate: {reply}")), now);
        }
    }

    /// Settles every request whose outcome the board now shows.
    pub fn on_board(&mut self, board: &Board, now: i64) {
        let mut done = Vec::new();
        for p in self.pending.iter().filter(|p| !p.sending) {
            let card = board.cards.iter().find(|c| c.id == p.card_id);
            match (&p.kind, card) {
                (Kind::Move { to, to_label, .. }, Some(c)) if c.column >= *to => {
                    done.push((p.card_id.clone(), format!("✓ moved to {to_label}")));
                }
                (Kind::Answer { .. }, Some(c)) if !c.decision => {
                    done.push((p.card_id.clone(), "✓ answered".to_owned()));
                }
                (Kind::Move { .. }, None) => {
                    done.push((p.card_id.clone(), "left the board".to_owned()));
                }
                (Kind::Answer { .. }, None) => {
                    done.push((p.card_id.clone(), "✓ answered; left the board".to_owned()));
                }
                _ => {}
            }
        }
        for (card, text) in done {
            self.settle(&card, Mark::Done(text), now);
        }
    }

    /// Snaps back every request that saw no outcome in time, and forgets old
    /// outcomes.
    pub fn expire(&mut self, now: i64, timeout_secs: i64) {
        let late: Vec<(String, String)> = self
            .pending
            .iter()
            .filter(|p| !p.sending && now - p.sent_at >= timeout_secs)
            .map(|p| {
                let after = format_elapsed(timeout_secs);
                let note = p.note_id.as_deref().unwrap_or("?");
                let why = if p.reported_done {
                    format!(
                        "Firstmate reported it done, but the board has not seen it in {after}: {}",
                        p.reply.as_deref().unwrap_or("")
                    )
                } else if let Some(reply) = &p.reply {
                    format!("no outcome after {after}; Firstmate said: {reply}")
                } else if p.seen {
                    format!(
                        "no outcome after {after}; Firstmate has note {note} and may still act on it"
                    )
                } else if matches!(p.kind, Kind::Answer { hold: true }) {
                    format!("the board still shows the call open after {after}")
                } else {
                    format!(
                        "no outcome after {after}: Firstmate has not picked up note {note} yet (it stays in its inbox)"
                    )
                };
                (p.card_id.clone(), why)
            })
            .collect();
        for (card, why) in late {
            self.settle(&card, Mark::Failed(format!("↩ {why}")), now);
        }
        self.settled.retain(|s| {
            let keep = match s.mark {
                Mark::Failed(_) => FAILED_SHOW_SECS,
                _ => DONE_SHOW_SECS,
            };
            now - s.at < keep
        });
    }

    /// Whether Firstmate's replies should be read now.
    pub fn wants_poll(&self, now: i64) -> bool {
        self.pending.iter().any(|p| p.note_id.is_some())
            && self.last_poll.is_none_or(|t| now - t >= POLL_SECS)
    }

    pub fn polled(&mut self, now: i64) {
        self.last_poll = Some(now);
    }

    /// What a card shows about its request, if anything.
    pub fn mark(&self, card_id: &str) -> Option<Mark> {
        if let Some(p) = self.get(card_id) {
            let what = p.what();
            let text = if p.sending {
                format!("⇢ sending: {what}…")
            } else if let Kind::Answer { hold } = p.kind {
                if hold {
                    "⇢ answered · waiting for the board".to_owned()
                } else if let Some(r) = &p.reply {
                    format!("⇢ Firstmate: {r}")
                } else {
                    "⇢ answered · waiting for Firstmate".to_owned()
                }
            } else if p.reported_done {
                "⇢ done per Firstmate · waiting for the board".to_owned()
            } else if let Some(r) = &p.reply {
                format!("⇢ Firstmate: {r}")
            } else if p.seen {
                format!("⇢ requested: {what} · Firstmate on it")
            } else {
                format!("⇢ requested: {what}")
            };
            return Some(Mark::Open(text));
        }
        self.settled
            .iter()
            .find(|s| s.card_id == card_id)
            .map(|s| s.mark.clone())
    }

    /// Rows for the card details view.
    pub fn detail_rows(&self, card_id: &str, now: i64) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        if let Some(p) = self.get(card_id) {
            rows.push((
                "Request",
                match &p.kind {
                    Kind::Move { to_label, .. } => {
                        format!(
                            "{} (to {to_label}), sent {} ago",
                            p.what(),
                            format_elapsed(now - p.sent_at)
                        )
                    }
                    Kind::Answer { .. } => {
                        format!("answer, sent {} ago", format_elapsed(now - p.sent_at))
                    }
                },
            ));
            rows.push(("Request id", p.request_id.clone()));
            if let Some(n) = &p.note_id {
                rows.push((
                    "Inbox note",
                    format!(
                        "{n}{}",
                        if p.seen {
                            " (picked up by Firstmate)"
                        } else {
                            " (waiting for Firstmate)"
                        }
                    ),
                ));
            }
            if let Some(r) = &p.reply {
                rows.push(("Firstmate", r.clone()));
            }
            if let Some(w) = &p.warning {
                rows.push(("Warning", w.clone()));
            }
            if let Some(e) = &self.poll_error {
                rows.push(("Replies", format!("unreadable: {e}")));
            }
        } else if let Some(Mark::Failed(t) | Mark::Done(t)) = self.mark(card_id) {
            rows.push(("Request", t));
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::fixture_board;

    fn mv(to: Column) -> Kind {
        Kind::Move {
            action: Action::Ready,
            to,
            to_label: to.title().to_owned(),
        }
    }

    #[test]
    fn a_request_is_requested_until_the_board_shows_it() {
        let board = fixture_board();
        let booked = board
            .cards
            .iter()
            .find(|c| c.column == Column::Booked)
            .unwrap();
        let mut t = Tracker::default();
        t.start(Pending::new(&booked.id, mv(Column::Ready), "r1", 100));
        assert!(matches!(t.mark(&booked.id), Some(Mark::Open(s)) if s.contains("sending")));
        t.on_outcome(
            Outcome::Requested {
                card_id: booked.id.clone(),
                request_id: "r1".into(),
                note_id: "n1".into(),
                warning: None,
            },
            101,
        );
        assert_eq!(
            t.mark(&booked.id),
            Some(Mark::Open("⇢ requested: mark ready".into()))
        );
        assert!(t.wants_poll(101));
        t.polled(101);
        assert!(!t.wants_poll(105));
        // The board still shows it in Booked: nothing settles.
        t.on_board(&board, 110);
        assert!(t.get(&booked.id).is_some());
        // Firstmate picks it up, then the board shows it in Ready.
        t.on_receipts(
            &[Receipt {
                id: "n1".into(),
                acknowledged: true,
                reply: None,
            }],
            120,
        );
        assert!(matches!(t.mark(&booked.id), Some(Mark::Open(s)) if s.contains("Firstmate on it")));
        let mut moved = board.clone();
        moved
            .cards
            .iter_mut()
            .find(|c| c.id == booked.id)
            .unwrap()
            .column = Column::Ready;
        t.on_board(&moved, 130);
        assert!(t.get(&booked.id).is_none());
        assert_eq!(
            t.mark(&booked.id),
            Some(Mark::Done("✓ moved to Ready".into()))
        );
        t.expire(130 + DONE_SHOW_SECS, 1800);
        assert_eq!(t.mark(&booked.id), None);
    }

    #[test]
    fn a_refusal_snaps_back_with_the_reason_and_done_waits_for_the_board() {
        let mut t = Tracker::default();
        for (card, note) in [("a", "n1"), ("b", "n2"), ("c", "n3")] {
            t.start(Pending::new(card, mv(Column::Staging), card, 0));
            t.on_outcome(
                Outcome::Requested {
                    card_id: card.into(),
                    request_id: card.into(),
                    note_id: note.into(),
                    warning: None,
                },
                0,
            );
        }
        let reply = |id: &str, text: &str| Receipt {
            id: id.into(),
            acknowledged: true,
            reply: Some(text.into()),
        };
        t.on_receipts(
            &[
                reply("n1", "refused: checks went red on the promotion PR"),
                reply("n2", "done: promoted via #150"),
                reply("n3", "waiting on the captain's staging word"),
            ],
            5,
        );
        assert_eq!(
            t.mark("a"),
            Some(Mark::Failed(
                "↩ Firstmate: refused: checks went red on the promotion PR".into()
            ))
        );
        assert_eq!(
            t.mark("b"),
            Some(Mark::Open(
                "⇢ done per Firstmate · waiting for the board".into()
            ))
        );
        assert_eq!(
            t.mark("c"),
            Some(Mark::Open(
                "⇢ Firstmate: waiting on the captain's staging word".into()
            ))
        );
        t.expire(1800, 1800);
        let Some(Mark::Failed(why)) = t.mark("b") else {
            panic!("done without the board times out")
        };
        assert!(why.contains("board has not seen it"), "{why}");
    }

    #[test]
    fn a_silent_request_times_out_saying_so() {
        let mut t = Tracker::default();
        t.start(Pending::new("x", mv(Column::Building), "r", 0));
        t.on_outcome(
            Outcome::Requested {
                card_id: "x".into(),
                request_id: "r".into(),
                note_id: "n9".into(),
                warning: Some("request saved as note n9, but Firstmate was not woken".into()),
            },
            0,
        );
        let (flash, ok, _) = t.flash.clone().unwrap();
        assert!(!ok && flash.contains("not woken"), "{flash}");
        t.expire(1799, 1800);
        assert!(t.get("x").is_some());
        t.expire(1800, 1800);
        let Some(Mark::Failed(why)) = t.mark("x") else {
            panic!()
        };
        assert!(why.contains("has not picked up note n9"), "{why}");
    }

    #[test]
    fn nothing_sent_snaps_back_at_once() {
        let mut t = Tracker::default();
        t.start(Pending::new("x", mv(Column::Dev), "r", 0));
        t.on_outcome(
            Outcome::NotSent {
                card_id: "x".into(),
                request_id: "r".into(),
                reason: "checks are not green: 1 failing (ci)".into(),
            },
            1,
        );
        assert!(t.get("x").is_none());
        assert_eq!(
            t.mark("x"),
            Some(Mark::Failed(
                "↩ checks are not green: 1 failing (ci)".into()
            ))
        );
    }

    #[test]
    fn replies_are_classified() {
        assert_eq!(reply_kind("Done: merged"), ReplyKind::Done);
        assert_eq!(reply_kind("  refused: no"), ReplyKind::Refused);
        assert_eq!(reply_kind("failed: gh down"), ReplyKind::Refused);
        assert_eq!(reply_kind("which model?"), ReplyKind::Info);
    }
}
