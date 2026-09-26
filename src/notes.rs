//! Release views shared by the board and `kanbr print` / `kanbr notes`: the
//! Live and Staging column headers, the release details, and plain-language
//! release notes built from the Live cards (design decision 8).

use std::fmt::Write;

use crate::dates::{format_ago, format_day, format_long_day};
use crate::model::{Board, Card, Column, ProjectRelease};

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The compact header line under a release column's title, for one project
/// or the whole board. `None` where there is nothing to say.
pub fn column_header(board: &Board, column: Column, project: Option<&str>) -> Option<String> {
    if let Some(p) = project {
        let rel = board.release(p)?;
        if !rel.uses(column) {
            return Some("not used".to_owned());
        }
        return match column {
            Column::Live => Some(live_line(board, rel, false)),
            Column::Staging => Some(staging_line(
                board.column_cards(Column::Staging, Some(p)).len(),
                rel.migrations.len(),
            )),
            _ => None,
        };
    }
    match column {
        Column::Live => {
            let releases = board.releases.iter().filter(|r| r.live.is_some()).count();
            (releases > 0).then(|| {
                format!(
                    "{} in {}",
                    plural(board.release_cards(None).len(), "change", "changes"),
                    plural(releases, "release", "releases")
                )
            })
        }
        Column::Staging => {
            let staged = board.releases.iter().any(|r| r.lanes.staging);
            staged.then(|| {
                staging_line(
                    board.column_cards(Column::Staging, None).len(),
                    board.releases.iter().map(|r| r.migrations.len()).sum(),
                )
            })
        }
        _ => None,
    }
}

/// `10 Sep · #116 · v1.2.1 · 5 changes` (`long` spells out the year).
fn live_line(board: &Board, rel: &ProjectRelease, long: bool) -> String {
    let Some(live) = &rel.live else {
        return "no release yet".to_owned();
    };
    let mut parts = vec![if long {
        format_long_day(live.time)
    } else {
        format_day(live.time)
    }];
    if let Some(n) = live.pr {
        parts.push(format!("#{n}"));
    }
    if let Some(v) = &live.version {
        parts.push(format!("v{}", v.trim_start_matches('v')));
    }
    parts.push(plural(
        board.release_cards(Some(&rel.project)).len(),
        "change",
        "changes",
    ));
    parts.join(" · ")
}

/// `4 to promote · 2 db changes`.
fn staging_line(ready: usize, migrations: usize) -> String {
    let mut s = if ready == 0 {
        "nothing to promote".to_owned()
    } else {
        format!("{ready} to promote")
    };
    if migrations > 0 {
        let _ = write!(s, " · {}", plural(migrations, "db change", "db changes"));
    }
    s
}

/// One project's release details: label/value rows.
pub struct Details {
    pub project: String,
    pub rows: Vec<(&'static str, String)>,
}

/// Release details for one project or every project on the board.
pub fn details(board: &Board, project: Option<&str>, now: i64) -> Vec<Details> {
    board
        .releases_in(project)
        .into_iter()
        .map(|rel| {
            let mut rows = Vec::new();
            if rel.lanes.live {
                rows.push(("Live release", live_line(board, rel, true)));
                if let Some(live) = &rel.live {
                    rows.push(match (&live.pr_url, live.pr) {
                        (Some(url), _) => ("Promotion PR", url.clone()),
                        (None, Some(n)) => ("Promotion PR", format!("#{n}")),
                        (None, None) => ("Landed as", format!("direct commit: {}", live.title)),
                    });
                }
            }
            if rel.lanes.staging {
                let ready = board.column_cards(Column::Staging, Some(&rel.project));
                rows.push((
                    "Next release",
                    if ready.is_empty() {
                        "nothing in Staging to promote".to_owned()
                    } else {
                        format!(
                            "{} ready to promote",
                            plural(ready.len(), "change", "changes")
                        )
                    },
                ));
                if rel.migrations.is_empty() {
                    rows.push(("Database", "no database changes".to_owned()));
                }
                for (i, m) in rel.migrations.iter().enumerate() {
                    rows.push((if i == 0 { "Database" } else { "" }, m.clone()));
                }
            }
            let refs: Vec<&str> = rel.refs.iter().map(|(_, r)| r.as_str()).collect();
            rows.push((
                "Read from",
                format!(
                    "{}, as of the last fetch{}",
                    refs.join(" · "),
                    rel.fetched
                        .map(|f| format!(" ({})", format_ago(f, now)))
                        .unwrap_or_default()
                ),
            ));
            Details {
                project: rel.project.clone(),
                rows,
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    New,
    Improved,
    Fixed,
    Internal,
}

/// A release-note line for a card: the conventional-commit type and scope
/// dropped, first letter capitalised, with the group it belongs to.
fn note(card: &Card) -> (Group, String, Option<String>) {
    let text = card
        .change_title
        .as_deref()
        .filter(|t| !t.starts_with("Merge "))
        .unwrap_or(&card.title)
        .trim();
    let (kind, rest) = match text.split_once(':') {
        Some((head, rest))
            if !head.contains(' ')
                && head.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) =>
        {
            let kind = head
                .split('(')
                .next()
                .unwrap_or(head)
                .trim_end_matches('!')
                .to_lowercase();
            (Some(kind), rest.trim())
        }
        _ => (None, text),
    };
    let group = match kind.as_deref() {
        Some("feat" | "feature") => Group::New,
        Some("fix" | "bugfix" | "hotfix" | "revert") => Group::Fixed,
        Some("perf" | "ux" | "ui" | "style") => Group::Improved,
        Some("docs" | "doc" | "test" | "tests" | "ci" | "chore" | "build" | "refactor") => {
            Group::Internal
        }
        _ => {
            let first = rest.split_whitespace().next().unwrap_or("").to_lowercase();
            match first.as_str() {
                "add" | "adds" | "added" | "new" | "introduce" | "support" | "allow" | "enable"
                | "let" | "show" => Group::New,
                "fix" | "fixes" | "fixed" | "stop" | "prevent" | "correct" | "resolve"
                | "repair" => Group::Fixed,
                _ => Group::Improved,
            }
        }
    };
    let mut line: String = rest.trim_end_matches('.').to_owned();
    if let Some(first) = line.chars().next() {
        line.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
    }
    (group, line, kind)
}

/// Short plain-language release notes for the latest Live release of one
/// project, or of every project on the board, ready to send to users.
pub fn release_notes(board: &Board, project: Option<&str>) -> String {
    let mut out = String::new();
    for rel in board.releases_in(project) {
        let Some(live) = &rel.live else { continue };
        let cards = board.release_cards(Some(&rel.project));
        if cards.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        let version = live
            .version
            .as_deref()
            .map(|v| format!(" {}", v.trim_start_matches('v')))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{}{version} — released {}",
            rel.project,
            format_long_day(live.time)
        );
        let mut notes: Vec<(Group, String, Option<String>)> =
            cards.iter().map(|c| note(c)).collect();
        notes.sort_by_key(|(g, _, _)| *g);
        for (group, heading) in [
            (Group::New, "New"),
            (Group::Improved, "Improved"),
            (Group::Fixed, "Fixed"),
        ] {
            let lines: Vec<&String> = notes
                .iter()
                .filter(|(g, _, _)| *g == group)
                .map(|(_, l, _)| l)
                .collect();
            if lines.is_empty() {
                continue;
            }
            let _ = writeln!(out, "\n{heading}");
            for l in lines {
                let _ = writeln!(out, "- {l}");
            }
        }
        let mut internal: Vec<&str> = notes
            .iter()
            .filter(|(g, _, _)| *g == Group::Internal)
            .filter_map(|(_, _, k)| k.as_deref())
            .collect();
        if !internal.is_empty() {
            let n = internal.len();
            internal.sort_unstable();
            internal.dedup();
            let _ = writeln!(
                out,
                "\nBehind the scenes: {} ({}).",
                plural(n, "change", "changes"),
                internal.join(", ")
            );
        }
    }
    if out.is_empty() {
        out.push_str("No Live release to describe yet.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{now, release_board};

    #[test]
    fn notes_group_changes_in_plain_language() {
        let b = release_board();
        let text = release_notes(&b, Some("Shop"));
        assert!(
            text.starts_with("Shop 1.2.1 — released 25 Sep 2026\n"),
            "{text}"
        );
        assert!(
            text.contains("\nNew\n- Persistent delivery fee per customer\n"),
            "{text}"
        );
        assert!(
            text.contains("\nFixed\n- Stop double saves on slow networks\n"),
            "{text}"
        );
        assert!(
            text.contains("- Receipt printing waits for the printer"),
            "{text}"
        );
        assert!(
            text.contains("Behind the scenes: 1 change (docs)."),
            "{text}"
        );
        assert!(!text.contains("feat("), "{text}");
        assert!(
            !text.contains("Old release"),
            "earlier releases are not in the notes\n{text}"
        );
    }

    #[test]
    fn notes_without_a_release_say_so() {
        let b = release_board();
        assert_eq!(
            release_notes(&b, Some("nothing-here")),
            "No Live release to describe yet.\n"
        );
    }

    #[test]
    fn headers_summarise_release_and_next_release() {
        let b = release_board();
        assert_eq!(
            column_header(&b, Column::Live, Some("Shop")).as_deref(),
            Some("25 Sep · #150 · v1.2.1 · 4 changes")
        );
        assert_eq!(
            column_header(&b, Column::Staging, Some("Shop")).as_deref(),
            Some("1 to promote · 1 db change")
        );
        assert_eq!(
            column_header(&b, Column::Staging, Some("Site")).as_deref(),
            Some("not used")
        );
        assert_eq!(column_header(&b, Column::Dev, Some("Shop")), None);
        assert_eq!(
            column_header(&b, Column::Live, None).as_deref(),
            Some("4 changes in 1 release")
        );
    }

    #[test]
    fn details_name_the_promotion_pr_and_database_changes() {
        let b = release_board();
        let d = details(&b, Some("Shop"), now());
        let rows = &d[0].rows;
        let get = |k: &str| rows.iter().find(|(l, _)| *l == k).map(|(_, v)| v.as_str());
        assert_eq!(
            get("Promotion PR"),
            Some("https://github.com/acme/shop/pull/150")
        );
        assert_eq!(
            get("Database"),
            Some("supabase/migrations/20260926_fee.sql")
        );
        assert_eq!(get("Next release"), Some("1 change ready to promote"));
        assert!(get("Read from").unwrap().contains("origin/main"));
        assert!(get("Read from").unwrap().contains("ago"));
    }
}
