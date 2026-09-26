//! The board model: Firstmate's fleet snapshot mapped onto six columns.
//!
//! Mapping (design decisions 3, 4, 5, 17, 22; slice 1 of decision 18):
//! - **Booked**: held backlog items (grill or decision, or any other hold). A
//!   grill tag marks items whose hold reason or title mentions a grill; items of
//!   a halted project are greyed and paused.
//! - **Ready**: queued and not held. A queued item blocked by another task stays
//!   here with a dependency badge.
//! - **Building**: work in flight (every live worker).
//! - **Dev / Staging / Live** (decisions 4, 6, 7, 8, 19; slice 2): derived
//!   from git by [`crate::releases`]. A project uses a lane when it has the
//!   branch configured to back it (default `dev`, `staging`, `main`); a project
//!   missing a branch never shows cards in that column, so the All tab lines
//!   up. Each merged card keeps its PR and sits in the furthest lane its change
//!   has reached; Live holds only the latest production release. Merged
//!   changes with no Firstmate card (hand-opened PRs, direct commits) are plain
//!   grey cards titled from the PR or commit. Finished work with no PR
//!   (reports, local tasks) is shown in the project's last lane until its next
//!   release. Where the repository cannot be read, or a merged PR is not in
//!   it yet, merged work falls back to the first lane for `finished_days`.
//! - A captain decision is a red badge on the card, which stays in its real column.
//! - Registered second mates' work is mixed into the same projects and columns
//!   with a `2nd` tag.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::config::Config;
use crate::dates::{DAY, format_age_days, format_elapsed, parse_date, parse_generation};
use crate::firstmate::{Lanes, Meta};
use crate::json::{arr_at, bool_at, get, i64_at, str_at, string_at, strings_at};
use crate::releases::{Change, ProjectGit, Role};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Column {
    Booked,
    Ready,
    Building,
    Dev,
    Staging,
    Live,
}

impl Column {
    pub const ALL: [Column; 6] = [
        Column::Booked,
        Column::Ready,
        Column::Building,
        Column::Dev,
        Column::Staging,
        Column::Live,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Column::Booked => "Booked",
            Column::Ready => "Ready",
            Column::Building => "Building",
            Column::Dev => "Dev",
            Column::Staging => "Staging",
            Column::Live => "Live",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// The column merged work lands in: the first release lane the project uses.
pub fn landing_column(lanes: Lanes) -> Option<Column> {
    [
        (lanes.dev, Column::Dev),
        (lanes.staging, Column::Staging),
        (lanes.live, Column::Live),
    ]
    .into_iter()
    .find_map(|(used, c)| used.then_some(c))
}

/// The last release lane the project uses, where finished work that was never
/// merged (reports, local tasks) is shown.
pub fn final_column(lanes: Lanes) -> Option<Column> {
    [
        (lanes.live, Column::Live),
        (lanes.staging, Column::Staging),
        (lanes.dev, Column::Dev),
    ]
    .into_iter()
    .find_map(|(used, c)| used.then_some(c))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    Main,
    Secondmate(String),
}

/// Automated test result for a card. Kanbr only displays it; the results come
/// from Firstmate's test lanes, which publish nothing yet, so the slot stays
/// empty until they do.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestBadge {
    Passed,
    Failed,
    Running,
}

/// How a card's state should read at a glance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Active,
    Attention,
    Problem,
    Finished,
    Muted,
}

#[derive(Clone, Debug)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub project: String,
    pub owner: Owner,
    pub column: Column,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    /// Waiting on the captain: the red badge and the waiting-on-you strip.
    pub decision: bool,
    pub grill: bool,
    /// Greyed: the project is halted or the item is parked.
    pub paused: bool,
    /// Unfinished tasks this item waits for (the dependency badge).
    pub blocked_by: Vec<String>,
    pub test: Option<TestBadge>,
    pub model: Option<String>,
    /// Epoch the elapsed time counts from.
    pub since: Option<i64>,
    /// `since` is a calendar date, so elapsed reads in whole days.
    pub since_is_date: bool,
    pub state: String,
    pub tone: Tone,
    /// Label/value pairs for the details view.
    pub details: Vec<(&'static str, String)>,
    /// A merged change with no Firstmate card (a plain grey card).
    pub outside: bool,
    /// Part of its project's latest Live release.
    pub in_release: bool,
    /// The PR title or commit subject from git, for release notes.
    pub change_title: Option<String>,
    hold_reason: Option<String>,
    hold_kind: Option<String>,
    order: usize,
}

impl Card {
    pub fn is_secondmate(&self) -> bool {
        matches!(self.owner, Owner::Secondmate(_))
    }

    pub fn elapsed(&self, now: i64) -> Option<String> {
        let since = self.since?;
        Some(if self.since_is_date {
            format_age_days(since, now)
        } else {
            format_elapsed(now - since)
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectTab {
    pub name: String,
    pub count: usize,
    pub halted: bool,
}

/// A project's latest production release: the last landing on its Live branch.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveRelease {
    pub time: i64,
    pub pr: Option<u64>,
    pub pr_url: Option<String>,
    pub title: String,
    /// The app version on the Live branch, where the project has one.
    pub version: Option<String>,
    /// The Live branch was fast-forwarded to a lower lane's commit.
    pub fast_forward: bool,
    /// Where the release starts is known, so its changes are.
    pub known: bool,
}

/// Release state for a project on the board.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectRelease {
    pub project: String,
    pub lanes: Lanes,
    /// The ref each used lane is read from, such as `origin/main`.
    pub refs: Vec<(Column, String)>,
    pub live: Option<LiveRelease>,
    /// Database migrations in Staging that Live does not have yet.
    pub migrations: Vec<String>,
    /// When the clone last fetched: the board reflects that moment.
    pub fetched: Option<i64>,
}

impl ProjectRelease {
    pub fn uses(&self, column: Column) -> bool {
        match column {
            Column::Dev => self.lanes.dev,
            Column::Staging => self.lanes.staging,
            Column::Live => self.lanes.live,
            _ => true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Board {
    pub cards: Vec<Card>,
    pub projects: Vec<ProjectTab>,
    pub generated: Option<String>,
    pub home: Option<String>,
    /// Things the board could not show, stated so an absence is never silent.
    pub notices: Vec<String>,
    /// Release state of each project on the board whose repository was read.
    pub releases: Vec<ProjectRelease>,
}

impl Board {
    /// Cards of one column, optionally limited to one project, in display order.
    pub fn column_cards(&self, column: Column, project: Option<&str>) -> Vec<&Card> {
        self.cards
            .iter()
            .filter(|c| c.column == column && project.is_none_or(|p| c.project == p))
            .collect()
    }

    /// Cards waiting on the captain, in board order (column, then position).
    pub fn waiting(&self) -> Vec<&Card> {
        self.cards.iter().filter(|c| c.decision).collect()
    }

    pub fn release(&self, project: &str) -> Option<&ProjectRelease> {
        self.releases.iter().find(|r| r.project == project)
    }

    /// Release state for the whole board, or one project.
    pub fn releases_in(&self, project: Option<&str>) -> Vec<&ProjectRelease> {
        self.releases
            .iter()
            .filter(|r| project.is_none_or(|p| r.project == p))
            .collect()
    }

    /// The cards of a project's latest Live release (or of every project's).
    pub fn release_cards(&self, project: Option<&str>) -> Vec<&Card> {
        self.column_cards(Column::Live, project)
            .into_iter()
            .filter(|c| c.in_release)
            .collect()
    }
}

/// File and git access the mapping needs; faked in tests.
pub trait Env {
    fn meta(&self, path: &Path) -> Option<Meta>;
    /// A project's release state from its repository: `None` when `repo` is
    /// not a git checkout, an error when its history cannot be read.
    fn git(&self, repo: &Path) -> Option<Result<Arc<ProjectGit>, String>>;
    /// The commit a merged PR landed as, from the forge, for a PR its
    /// project's history does not name.
    fn merged_commit(&self, _pr_url: &str) -> Option<String> {
        None
    }
}

pub struct Context<'a> {
    pub registry: &'a [String],
    pub config: &'a Config,
    pub now: i64,
    pub env: &'a dyn Env,
}

/// Maps a `fm-fleet-snapshot.v1` object to the board.
pub fn build_board(snapshot: &Value, ctx: &Context) -> Board {
    let mut b = Builder::new(snapshot, ctx);
    b.main_home();
    b.secondmates();
    b.outside_cards();
    b.finish()
}

fn contains_word(text: Option<&str>, words: &[String]) -> bool {
    let Some(text) = text else { return false };
    let text = text.to_lowercase();
    words
        .iter()
        .any(|w| !w.is_empty() && text.contains(w.as_str()))
}

fn pr_number(url: &str) -> Option<u64> {
    let (_, rest) = url.split_once("/pull/")?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// A compact model label: `claude-opus-5-5` reads `opus-5-5`; a `default`
/// model reads as its harness.
pub fn short_model(model: Option<&str>, harness: Option<&str>) -> Option<String> {
    match model
        .map(str::trim)
        .filter(|m| !m.is_empty() && *m != "default")
    {
        Some(m) => {
            let m = m.rsplit('/').next().unwrap_or(m);
            Some(m.strip_prefix("claude-").unwrap_or(m).to_owned())
        }
        None => harness.map(str::to_owned),
    }
}

/// `refs/remotes/origin/main` reads `origin/main`; `refs/heads/main` reads `main`.
fn short_ref(refname: &str) -> String {
    refname
        .strip_prefix("refs/remotes/")
        .or_else(|| refname.strip_prefix("refs/heads/"))
        .unwrap_or(refname)
        .to_owned()
}

/// `abc1234 on dev`: where a change first landed.
fn landed(g: &ProjectGit, c: &Change) -> String {
    let branch = g
        .refs
        .iter()
        .find(|r| r.column == c.home)
        .map_or("?", |r| r.branch.as_str());
    format!("{} on {branch}", c.commit.get(..7).unwrap_or(&c.commit))
}

/// A plain grey card for a merged change with no Firstmate card.
fn outside_card(project: &str, g: &ProjectGit, c: &Change) -> Card {
    let short = c.commit.get(..7).unwrap_or(&c.commit);
    let mut details = vec![
        ("Source", "git history: no Firstmate card".to_owned()),
        ("Landed", landed(g, c)),
    ];
    if c.in_release {
        details.push(("Release", "in the latest Live release".to_owned()));
    }
    Card {
        id: match c.pr {
            Some(n) => format!("{project}#{n}"),
            None => format!("{project}@{short}"),
        },
        title: c.title.clone(),
        project: project.to_owned(),
        owner: Owner::Main,
        column: c.column,
        pr_number: c.pr,
        pr_url: c.pr.and_then(|n| g.pr_url(n)),
        decision: false,
        grill: false,
        paused: false,
        blocked_by: Vec::new(),
        test: None,
        model: None,
        since: Some(c.time),
        since_is_date: true,
        state: if c.pr.is_some() {
            "merged"
        } else {
            "committed"
        }
        .to_owned(),
        tone: Tone::Muted,
        details,
        outside: true,
        in_release: c.in_release,
        change_title: Some(c.title.clone()),
        hold_reason: None,
        hold_kind: None,
        order: 0,
    }
}

struct Builder<'a> {
    snap: &'a Value,
    ctx: &'a Context<'a>,
    today: i64,
    fm_root: Option<PathBuf>,
    projects_dir: Option<PathBuf>,
    cards: Vec<Card>,
    notices: Vec<String>,
    /// Release state per project, read once per build.
    gits: HashMap<String, Option<Arc<ProjectGit>>>,
    /// Changes shown as Firstmate cards, per project (by index into its changes).
    claimed: HashMap<String, HashSet<usize>>,
    /// Second-mate homes, for projects cloned only there.
    mate_homes: Vec<PathBuf>,
    /// Projects whose finished cards were hidden because they use no lane.
    laneless: Vec<String>,
    /// Merged cards whose PR is not in their project's history yet.
    unfetched: Vec<String>,
    releases: Vec<ProjectRelease>,
}

/// Where a merged card's PR sits in its project's history.
enum Found {
    Change(usize),
    /// A promotion or back-merge PR: shown through the release headers.
    Promotion,
    Missing,
}

impl<'a> Builder<'a> {
    fn new(snap: &'a Value, ctx: &'a Context<'a>) -> Self {
        let fm_root = str_at(snap, "roots.fm_root")
            .or_else(|| str_at(snap, "fm_home"))
            .map(PathBuf::from);
        let projects_dir = str_at(snap, "roots.projects")
            .map(PathBuf::from)
            .or_else(|| fm_root.as_ref().map(|r| r.join("projects")));
        Builder {
            snap,
            ctx,
            today: ctx.now.div_euclid(DAY),
            fm_root,
            projects_dir,
            cards: Vec::new(),
            notices: Vec::new(),
            gits: HashMap::new(),
            claimed: HashMap::new(),
            mate_homes: arr_at(snap, "secondmate_current.records")
                .iter()
                .filter_map(|m| str_at(m, "home").map(PathBuf::from))
                .collect(),
            laneless: Vec::new(),
            unfetched: Vec::new(),
            releases: Vec::new(),
        }
    }

    fn project(&self, repo: Option<&str>, id: &str) -> String {
        if let Some(repo) = repo {
            let trimmed = repo.trim_end_matches('/');
            if self
                .fm_root
                .as_deref()
                .is_some_and(|r| Path::new(trimmed) == r)
            {
                return "firstmate".to_owned();
            }
            let last = trimmed.rsplit('/').find(|s| !s.is_empty());
            if let Some(name) = last.map(|n| n.strip_suffix(".git").unwrap_or(n)) {
                if name.eq_ignore_ascii_case("firstmate") {
                    return "firstmate".to_owned();
                }
                return self
                    .ctx
                    .registry
                    .iter()
                    .find(|r| r.eq_ignore_ascii_case(name))
                    .cloned()
                    .unwrap_or_else(|| name.to_owned());
            }
        }
        let id = id.to_lowercase();
        let mut best: Option<(usize, String)> = None;
        let aliases = [("fm", "firstmate"), ("firstmate", "firstmate")];
        let candidates = self
            .ctx
            .registry
            .iter()
            .map(|r| (r.to_lowercase().replace(' ', "-"), r.clone()))
            .chain(
                aliases
                    .iter()
                    .map(|(a, n)| ((*a).to_owned(), (*n).to_owned())),
            );
        for (prefix, name) in candidates {
            if id.starts_with(&format!("{prefix}-"))
                && best.as_ref().is_none_or(|(l, _)| prefix.len() > *l)
            {
                best = Some((prefix.len(), name));
            }
        }
        best.map(|(_, n)| n).unwrap_or_else(|| "other".to_owned())
    }

    /// A project's release state, from the first readable clone: the
    /// Firstmate home for firstmate, then the projects directory, then the
    /// owning and other second mates' homes.
    fn git_for(&mut self, project: &str, owner_home: Option<&Path>) -> Option<Arc<ProjectGit>> {
        if let Some(g) = self.gits.get(project) {
            return g.clone();
        }
        let mut candidates = Vec::new();
        if project == "firstmate" {
            candidates.extend(self.fm_root.clone());
        }
        if let Some(dir) = &self.projects_dir {
            candidates.push(dir.join(project));
        }
        candidates.extend(owner_home.map(|h| h.join("projects").join(project)));
        candidates.extend(
            self.mate_homes
                .iter()
                .map(|h| h.join("projects").join(project)),
        );
        let mut found = None;
        for path in candidates {
            match self.ctx.env.git(&path) {
                None => continue,
                Some(Ok(g)) => found = Some(g),
                Some(Err(e)) => self.notices.push(format!(
                    "{project}: git history unreadable, lanes assumed: {e}"
                )),
            }
            break;
        }
        self.gits.insert(project.to_owned(), found.clone());
        found
    }

    /// The lanes assumed when no repository can be read: only the last
    /// configured lane.
    fn assumed_lanes(&self) -> Lanes {
        let [dev, staging, live] = self.ctx.config.branches().map(|b| !b.is_empty());
        Lanes {
            live,
            staging: staging && !live,
            dev: dev && !staging && !live,
        }
    }

    /// Finds a merged card's PR in its project's history: by the PR number in
    /// the merge message, else by the commit the forge says it landed as.
    fn find_change(&self, g: &ProjectGit, pr_url: &str) -> Found {
        let role = |r: &Role| match r {
            Role::Change(i) => Found::Change(*i),
            Role::Promotion | Role::BackMerge => Found::Promotion,
        };
        // A PR in another repository (an upstream contribution) is not one of
        // this repository's numbered PRs.
        let same_repo = g.web.as_ref().is_none_or(|w| {
            pr_url
                .to_lowercase()
                .starts_with(&format!("{}/pull/", w.to_lowercase()))
        });
        if same_repo && let Some(r) = pr_number(pr_url).and_then(|n| g.by_pr.get(&n)) {
            return role(r);
        }
        match self
            .ctx
            .env
            .merged_commit(pr_url)
            .and_then(|sha| g.by_commit.get(&sha))
        {
            Some(r) => role(r),
            None => Found::Missing,
        }
    }

    fn is_held(&self, rec: &Value) -> bool {
        if str_at(rec, "hold_reason").is_none() {
            return false;
        }
        if str_at(rec, "hold_kind") == Some("captain") {
            return true;
        }
        match str_at(rec, "hold_until").and_then(parse_date) {
            Some(until) => until.div_euclid(DAY) > self.today,
            None => true,
        }
    }

    fn push(&mut self, mut card: Card) {
        card.order = self.cards.len();
        self.cards.push(card);
    }

    /// A card for a queued or held backlog row (main home or second mate).
    fn planned_card(&self, rec: &Value, owner: Owner) -> Option<Card> {
        let id = string_at(rec, "id")?;
        let title = string_at(rec, "title").unwrap_or_else(|| id.clone());
        let project = self.project(str_at(rec, "repo"), &id);
        let held = self.is_held(rec);
        let hold_reason = string_at(rec, "hold_reason");
        let hold_kind = string_at(rec, "hold_kind");
        let decision = bool_at(rec, "captain_actionable");
        let blocked_by = strings_at(rec, "unresolved_blocker_ids");
        let words = &self.ctx.config.grill_words;
        let grill = held
            && (contains_word(hold_reason.as_deref(), words) || contains_word(Some(&title), words));
        let until = str_at(rec, "hold_until")
            .filter(|u| parse_date(u).is_some_and(|e| e.div_euclid(DAY) > self.today));
        let (column, state, tone) = if held {
            let state = if decision {
                "your call".to_owned()
            } else if grill {
                "needs grill".to_owned()
            } else if let Some(u) = until {
                format!("until {}", u.get(5..10).unwrap_or(u))
            } else if let Some(k) = &hold_kind {
                format!("{k} hold")
            } else {
                "held".to_owned()
            };
            let tone = if decision {
                Tone::Attention
            } else {
                Tone::Normal
            };
            (Column::Booked, state, tone)
        } else if blocked_by.is_empty() {
            (Column::Ready, "queued".to_owned(), Tone::Normal)
        } else {
            (
                Column::Ready,
                format!("after {}", blocked_by.join(", ")),
                Tone::Muted,
            )
        };
        let since_stamp = if held {
            str_at(rec, "hold_set").or_else(|| str_at(rec, "since"))
        } else {
            str_at(rec, "since")
        };
        let pr_url = string_at(rec, "pr_url");
        let mut details = vec![("ID", id.clone())];
        details.push((
            "Kind",
            string_at(rec, "kind").unwrap_or_else(|| "-".to_owned()),
        ));
        if let Some(r) = &hold_reason {
            details.push(("Hold", r.clone()));
        }
        if let Some(k) = &hold_kind {
            details.push(("Hold kind", k.clone()));
        }
        if let Some(u) = str_at(rec, "hold_until") {
            details.push(("Hold until", u.to_owned()));
        }
        if let Some(b) = str_at(rec, "hold_bucket") {
            details.push(("Decision", b.to_owned()));
        }
        if !blocked_by.is_empty() {
            details.push(("Waits for", blocked_by.join(", ")));
        }
        if let Some(r) = str_at(rec, "blocked_reason") {
            details.push(("Blocked", r.to_owned()));
        }
        if let Some(s) = str_at(rec, "since") {
            details.push(("Filed", s.to_owned()));
        }
        if let Some(b) = str_at(rec, "body_excerpt") {
            details.push(("Notes", b.to_owned()));
        }
        Some(Card {
            pr_number: pr_url.as_deref().and_then(pr_number),
            pr_url,
            id,
            title,
            project,
            owner,
            column,
            decision,
            grill,
            paused: hold_kind.as_deref() == Some("parked"),
            blocked_by,
            test: None,
            model: None,
            since: since_stamp.and_then(parse_date),
            since_is_date: true,
            state,
            tone,
            details,
            outside: false,
            in_release: false,
            change_title: None,
            hold_reason,
            hold_kind,
            order: 0,
        })
    }

    /// A Building card for a live worker, from its task row and/or backlog row.
    fn building_card(
        &self,
        id: &str,
        task: Option<&Value>,
        rec: Option<&Value>,
        owner: Owner,
        meta_path: Option<PathBuf>,
    ) -> Card {
        let null = Value::Null;
        let t = task.unwrap_or(&null);
        let r = rec.unwrap_or(&null);
        let title = str_at(r, "title")
            .or_else(|| str_at(t, "backlog.title"))
            .unwrap_or(id)
            .to_owned();
        let project = self.project(str_at(r, "repo").or_else(|| str_at(t, "project")), id);
        let meta = meta_path
            .or_else(|| str_at(t, "paths.meta.path").map(PathBuf::from))
            .and_then(|p| self.ctx.env.meta(&p))
            .unwrap_or_default();
        let harness = str_at(t, "harness").or_else(|| meta.get("harness").map(String::as_str));
        let model = short_model(meta.get("model").map(String::as_str), harness);
        let run_state = str_at(t, "current_state.state");
        let pending = bool_at(t, "hints.pending_decision");
        let (state, tone) = match (task, run_state) {
            (None, _) => ("no worker".to_owned(), Tone::Problem),
            _ if pending => ("needs decision".to_owned(), Tone::Attention),
            (_, Some(s)) => {
                let tone = match s {
                    "working" => Tone::Active,
                    "done" => Tone::Finished,
                    "blocked" | "failed" => Tone::Problem,
                    "parked" | "paused" => Tone::Muted,
                    "unknown" => Tone::Muted,
                    _ => Tone::Normal,
                };
                (s.to_owned(), tone)
            }
            (_, None) => ("unknown".to_owned(), Tone::Muted),
        };
        let spawn = str_at(t, "spawn_gen")
            .or_else(|| meta.get("spawn_gen").map(String::as_str))
            .and_then(parse_generation);
        let (since, since_is_date) = match spawn {
            Some(e) => (Some(e), false),
            None => (str_at(r, "since").and_then(parse_date), true),
        };
        let pr_url = string_at(t, "pr.url").or_else(|| string_at(r, "pr_url"));
        let mut details = vec![("ID", id.to_owned())];
        let mut add = |label: &'static str, v: Option<&str>| {
            if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
                details.push((label, v.to_owned()));
            }
        };
        add("Kind", str_at(t, "kind").or_else(|| str_at(r, "kind")));
        add("State", run_state);
        add("Detail", str_at(t, "current_state.detail"));
        add("Harness", harness);
        add("Model", meta.get("model").map(String::as_str));
        add("Effort", meta.get("effort").map(String::as_str));
        add(
            "Mode",
            str_at(t, "mode").or_else(|| meta.get("mode").map(String::as_str)),
        );
        add(
            "Branch",
            str_at(t, "branch").or_else(|| meta.get("branch").map(String::as_str)),
        );
        add(
            "Worktree",
            str_at(t, "paths.worktree.path").or_else(|| meta.get("worktree").map(String::as_str)),
        );
        add("Pane", str_at(t, "endpoint.target"));
        add("Last event", str_at(t, "paths.status_log.last_event.raw"));
        add("Filed", str_at(r, "since"));
        add("Hold", str_at(r, "hold_reason"));
        add("Notes", str_at(r, "body_excerpt"));
        for d in arr_at(t, "hints.open_decisions") {
            add(
                "Open decision",
                str_at(d, "summary").or_else(|| str_at(d, "key")),
            );
        }
        Card {
            pr_number: pr_url.as_deref().and_then(pr_number),
            pr_url,
            id: id.to_owned(),
            title,
            project,
            owner,
            column: Column::Building,
            decision: pending || bool_at(r, "captain_actionable"),
            grill: false,
            paused: false,
            blocked_by: strings_at(r, "unresolved_blocker_ids"),
            test: None,
            model,
            since,
            since_is_date,
            state,
            tone,
            details,
            outside: false,
            in_release: false,
            change_title: None,
            hold_reason: string_at(r, "hold_reason"),
            hold_kind: string_at(r, "hold_kind"),
            order: 0,
        }
    }

    /// A Dev/Staging/Live card for finished work, or `None` when it is not on
    /// the board: a change released before the project's latest release, a
    /// promotion PR (shown through the release headers), or unplaced work
    /// that finished longer ago than the configured window.
    fn finished_card(
        &mut self,
        rec: &Value,
        owner: Owner,
        owner_home: Option<&Path>,
    ) -> Option<Card> {
        let id = string_at(rec, "id")?;
        let verb = str_at(rec, "completion.verb");
        let date = str_at(rec, "completion.date")
            .or_else(|| verb.and_then(|v| str_at(rec, v)))
            .or_else(|| str_at(rec, "merged"))
            .or_else(|| str_at(rec, "done"))
            .or_else(|| str_at(rec, "reported"));
        let finished = date.and_then(parse_date);
        let recent = finished
            .is_none_or(|f| self.today - f.div_euclid(DAY) <= self.ctx.config.finished_days);
        let title = string_at(rec, "title").unwrap_or_else(|| id.clone());
        let project = self.project(str_at(rec, "repo"), &id);
        let pr_url = string_at(rec, "pr_url");
        let merged = pr_url.is_some() || verb == Some("merged");
        let git = self.git_for(&project, owner_home);
        let lanes = git
            .as_ref()
            .map_or_else(|| self.assumed_lanes(), |g| g.lanes);

        let mut change: Option<Change> = None;
        let mut unfetched = false;
        if let (Some(g), Some(url)) = (git.as_deref(), pr_url.as_deref())
            && g.top().is_some()
        {
            match self.find_change(g, url) {
                Found::Change(i) => {
                    let c = &g.changes[i];
                    if !g.visible(c, recent) {
                        return None;
                    }
                    self.claimed.entry(project.clone()).or_default().insert(i);
                    change = Some(c.clone());
                }
                Found::Promotion => return None,
                Found::Missing => unfetched = true,
            }
        }
        let column = match &change {
            Some(c) => c.column,
            None => {
                if !recent {
                    return None;
                }
                let column = if merged {
                    landing_column(lanes)
                } else {
                    final_column(lanes)
                };
                let Some(column) = column else {
                    self.laneless.push(project);
                    return None;
                };
                // Unmerged finished work in Live stays until the next release.
                if !merged
                    && column == Column::Live
                    && let (Some(f), Some(r)) =
                        (finished, git.as_ref().and_then(|g| g.release.as_ref()))
                    && f.div_euclid(DAY) < r.time.div_euclid(DAY)
                {
                    return None;
                }
                column
            }
        };
        let mut details = vec![("ID", id.clone())];
        for (label, key) in [
            ("Kind", "kind"),
            ("Finished", "completion.date"),
            ("Report", "report_path"),
            ("Note", "local_note"),
            ("Notes", "body_excerpt"),
        ] {
            if let Some(v) = str_at(rec, key) {
                details.push((label, v.to_owned()));
            }
        }
        if let (Some(c), Some(g)) = (&change, &git) {
            details.push(("Landed", landed(g, c)));
            if c.in_release {
                details.push(("Release", "in the latest Live release".to_owned()));
            }
        }
        if unfetched {
            self.unfetched.push(id.clone());
            details.push((
                "Git",
                "PR not in the project's history yet (the board reads the clone as of its last fetch)"
                    .to_owned(),
            ));
        }
        Some(Card {
            pr_number: pr_url.as_deref().and_then(pr_number),
            pr_url,
            id,
            title,
            project,
            owner,
            column,
            decision: false,
            grill: false,
            paused: false,
            blocked_by: Vec::new(),
            test: None,
            model: None,
            since: change.as_ref().map_or(finished, |c| Some(c.time)),
            since_is_date: true,
            state: verb.unwrap_or("done").to_owned(),
            tone: Tone::Finished,
            details,
            outside: false,
            in_release: change.as_ref().is_some_and(|c| c.in_release),
            change_title: change.map(|c| c.title),
            hold_reason: None,
            hold_kind: None,
            order: 0,
        })
    }

    /// Plain grey cards for the changes on the board that no Firstmate card
    /// tracks (hand-opened PRs, direct commits), and each project's release
    /// state. Only projects already on the board and not halted are read, so
    /// dormant repositories never add tabs.
    fn outside_cards(&mut self) {
        let halted = self.halted_projects();
        let mut projects: Vec<String> = self.cards.iter().map(|c| c.project.clone()).collect();
        projects.sort_by_key(|p| p.to_lowercase());
        projects.dedup();
        for project in projects {
            if halted.contains(&project) {
                continue;
            }
            let Some(g) = self.git_for(&project, None) else {
                continue;
            };
            let claimed = self.claimed.get(&project).cloned().unwrap_or_default();
            for (i, c) in g.changes.iter().enumerate() {
                let recent = self.today - c.time.div_euclid(DAY) <= self.ctx.config.finished_days;
                if claimed.contains(&i) || !g.visible(c, recent) {
                    continue;
                }
                let card = outside_card(&project, &g, c);
                self.push(card);
            }
            if g.top().is_some() {
                self.releases.push(ProjectRelease {
                    project: project.clone(),
                    lanes: g.lanes,
                    refs: g
                        .refs
                        .iter()
                        .map(|r| (r.column, short_ref(&r.refname)))
                        .collect(),
                    live: g.release.as_ref().map(|r| LiveRelease {
                        time: r.time,
                        pr: r.pr,
                        pr_url: r.pr.and_then(|n| g.pr_url(n)),
                        title: r.title.clone(),
                        version: r.version.clone(),
                        fast_forward: r.fast_forward,
                        known: r.known,
                    }),
                    migrations: g.migrations.clone(),
                    fetched: g.fetched,
                });
            }
        }
    }

    /// Projects halted by a hold reason on one of their Booked or Ready cards.
    fn halted_projects(&self) -> HashSet<String> {
        let words = &self.ctx.config.halted_words;
        self.cards
            .iter()
            .filter(|c| matches!(c.column, Column::Booked | Column::Ready))
            .filter(|c| contains_word(c.hold_reason.as_deref(), words))
            .map(|c| c.project.clone())
            .collect()
    }

    fn main_home(&mut self) {
        let snap = self.snap;
        let tasks: HashMap<&str, &Value> = arr_at(snap, "tasks")
            .iter()
            .filter(|t| str_at(t, "kind") != Some("secondmate"))
            .filter_map(|t| str_at(t, "id").map(|id| (id, t)))
            .collect();
        let mut record_ids = HashSet::new();
        let mut free_form = 0;
        for rec in arr_at(snap, "backlog.records") {
            let state = str_at(rec, "state");
            if !bool_at(rec, "structured") {
                if matches!(state, Some("queued" | "in_flight")) {
                    free_form += 1;
                }
                continue;
            }
            let Some(id) = str_at(rec, "id") else {
                continue;
            };
            record_ids.insert(id);
            match state {
                Some("queued") => {
                    if let Some(c) = self.planned_card(rec, Owner::Main) {
                        self.push(c);
                    }
                }
                Some("in_flight") => {
                    let role = str_at(rec, "current_role");
                    if role == Some("program") {
                        continue;
                    }
                    let task = tasks.get(id).copied();
                    let working =
                        task.and_then(|t| str_at(t, "current_state.state")) == Some("working");
                    if role == Some("held") && !working {
                        if let Some(c) = self.planned_card(rec, Owner::Main) {
                            self.push(c);
                        }
                    } else {
                        let c = self.building_card(id, task, Some(rec), Owner::Main, None);
                        self.push(c);
                    }
                }
                Some("done") => {
                    if let Some(c) = self.finished_card(rec, Owner::Main, None) {
                        self.push(c);
                    }
                }
                _ => {}
            }
        }
        let mut orphans: Vec<(&str, &Value)> = tasks
            .iter()
            .filter(|(id, _)| !record_ids.contains(*id))
            .map(|(id, t)| (*id, *t))
            .collect();
        orphans.sort_by_key(|(id, _)| *id);
        for (id, task) in orphans {
            let c = self.building_card(id, Some(task), None, Owner::Main, None);
            self.push(c);
        }
        if free_form > 0 {
            self.notices.push(format!(
                "{free_form} free-form backlog row(s) are not structured and are not shown"
            ));
        }
        if get(snap, "main_inventory.valid").as_bool() == Some(false) {
            self.notices.push(format!(
                "main backlog inventory invalid: {}",
                str_at(snap, "main_inventory.reason").unwrap_or("reason not given")
            ));
        }
    }

    fn secondmates(&mut self) {
        let snap = self.snap;
        let current = get(snap, "secondmate_current");
        if get(current, "registry.available").as_bool() == Some(false) {
            self.notices.push(format!(
                "second-mate registry unavailable: {}",
                str_at(current, "registry.reason").unwrap_or("read failed")
            ));
        }
        if let Some(n) = i64_at(current, "truncated").filter(|n| *n > 0) {
            self.notices.push(format!(
                "{n} registered second mate(s) omitted by the snapshot bound"
            ));
        }
        for m in arr_at(current, "records") {
            let Some(mate) = str_at(m, "id") else {
                continue;
            };
            let owner = Owner::Secondmate(mate.to_owned());
            let home = str_at(m, "home").map(PathBuf::from);
            if str_at(m, "provenance.selected") != Some("structured-home") {
                self.notices.push(format!(
                    "second mate {mate}: home unreadable ({})",
                    str_at(m, "current.reason").unwrap_or("no structured state")
                ));
            } else if let Some(reason) = str_at(m, "current.reason") {
                self.notices.push(format!("second mate {mate}: {reason}"));
            }
            for o in arr_at(m, "omitted") {
                if let (Some(s), Some(n)) = (str_at(o, "surface"), i64_at(o, "count")) {
                    self.notices.push(format!(
                        "second mate {mate}: {n} {s} omitted by its summary bound"
                    ));
                }
            }
            let child_decisions: HashMap<&str, &str> = arr_at(m, "decisions_open")
                .iter()
                .filter(|d| str_at(d, "source") == Some("status"))
                .filter_map(|d| {
                    Some((
                        str_at(d, "id")?,
                        str_at(d, "summary").unwrap_or("decision pending"),
                    ))
                })
                .collect();
            for child in arr_at(m, "active_children") {
                let Some(id) = str_at(child, "id") else {
                    continue;
                };
                let meta_path = home
                    .as_ref()
                    .map(|h| h.join("state").join(format!("{id}.meta")));
                let meta = meta_path
                    .as_deref()
                    .and_then(|p| self.ctx.env.meta(p))
                    .unwrap_or_default();
                let task = serde_json::json!({
                    "id": id,
                    "kind": str_at(child, "kind"),
                    "harness": meta.get("harness"),
                    "project": str_at(child, "repo"),
                    "current_state": {
                        "state": str_at(child, "state").unwrap_or("working"),
                        "detail": str_at(child, "doing"),
                    },
                    "hints": {"pending_decision": child_decisions.contains_key(id)},
                    "backlog": {"title": str_at(child, "name")},
                });
                let mut c = self.building_card(id, Some(&task), None, owner.clone(), meta_path);
                c.details.push(("Second mate", mate.to_owned()));
                if let Some(d) = child_decisions.get(id) {
                    c.details.push(("Open decision", (*d).to_owned()));
                }
                self.push(c);
            }
            let mut queued_ids = HashSet::new();
            for q in arr_at(m, "queued") {
                if let Some(mut c) = self.planned_card(q, owner.clone()) {
                    queued_ids.insert(c.id.clone());
                    c.details.push(("Second mate", mate.to_owned()));
                    self.push(c);
                }
            }
            for d in arr_at(m, "decisions_open") {
                if str_at(d, "source") != Some("backlog") {
                    continue;
                }
                let Some(id) = str_at(d, "id") else { continue };
                if queued_ids.contains(id) {
                    continue;
                }
                let rec = serde_json::json!({
                    "id": id,
                    "title": str_at(d, "summary").unwrap_or(id),
                    "hold_reason": str_at(d, "reason").unwrap_or("captain decision pending"),
                    "hold_kind": "captain",
                    "hold_until": str_at(d, "hold_until"),
                    "captain_actionable": str_at(d, "hold_bucket").is_none_or(|b| b == "live"),
                });
                if let Some(mut c) = self.planned_card(&rec, owner.clone()) {
                    c.details.push(("Second mate", mate.to_owned()));
                    self.push(c);
                }
            }
            for l in arr_at(m, "landed") {
                if let Some(mut c) = self.finished_card(l, owner.clone(), home.as_deref()) {
                    c.details.push(("Second mate", mate.to_owned()));
                    self.push(c);
                }
            }
        }
    }

    fn finish(mut self) -> Board {
        if !self.laneless.is_empty() {
            let n = self.laneless.len();
            self.laneless.sort();
            self.laneless.dedup();
            let [d, st, l] = self.ctx.config.branches();
            self.notices.push(format!(
                "{n} finished card(s) not shown: {} has none of the configured branches (dev={d}, staging={st}, live={l})",
                self.laneless.join(", ")
            ));
        }
        if !self.unfetched.is_empty() {
            self.notices.push(format!(
                "{} merged card(s) not found in their project's git history yet, placed by merge date (the board reads each clone as of its last fetch): {}",
                self.unfetched.len(),
                self.unfetched.join(", ")
            ));
        }
        let halted = self.halted_projects();
        for c in &mut self.cards {
            if matches!(c.column, Column::Booked | Column::Ready)
                && (halted.contains(&c.project) || c.hold_kind.as_deref() == Some("parked"))
            {
                c.paused = true;
                if !c.decision {
                    c.state = "paused".to_owned();
                    c.tone = Tone::Muted;
                }
            }
        }
        self.cards.sort_by(|a, b| {
            a.column.cmp(&b.column).then_with(|| match a.column {
                Column::Booked | Column::Ready => b
                    .decision
                    .cmp(&a.decision)
                    .then(a.paused.cmp(&b.paused))
                    .then(a.order.cmp(&b.order)),
                Column::Building => a.order.cmp(&b.order),
                Column::Dev | Column::Staging | Column::Live => b
                    .in_release
                    .cmp(&a.in_release)
                    .then(b.since.cmp(&a.since))
                    .then(a.order.cmp(&b.order)),
            })
        });
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for c in &self.cards {
            *counts.entry(c.project.as_str()).or_default() += 1;
        }
        let mut projects: Vec<ProjectTab> = counts
            .into_iter()
            .map(|(name, count)| ProjectTab {
                halted: halted.contains(name),
                name: name.to_owned(),
                count,
            })
            .collect();
        projects.sort_by(|a, b| match a.halted.cmp(&b.halted) {
            Ordering::Equal => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            o => o,
        });
        Board {
            cards: self.cards,
            projects,
            generated: string_at(self.snap, "generated"),
            home: string_at(self.snap, "fm_home"),
            notices: self.notices,
            releases: self.releases,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
