//! `kanbr print`: the board once, as plain text, for scripts and quick looks.

use std::fmt::Write;

use crate::config::Config;
use crate::model::{Board, Card, Column};

fn card_line(card: &Card, now: i64) -> String {
    let mut s = String::from("  ");
    if card.decision {
        s.push_str("⚑ ");
    }
    if card.grill {
        s.push_str("[grill] ");
    }
    if card.paused {
        s.push_str("[paused] ");
    }
    if !card.blocked_by.is_empty() {
        let _ = write!(s, "[after {}] ", card.blocked_by.join(","));
    }
    let _ = write!(s, "{}", card.project);
    if card.is_secondmate() {
        s.push_str(" 2nd");
    }
    if let Some(n) = card.pr_number {
        let _ = write!(s, " #{n}");
    }
    let _ = write!(s, " · {} ({})", card.title, card.id);
    let status: Vec<String> = [
        card.model.clone(),
        card.elapsed(now),
        Some(card.state.clone()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let _ = write!(s, " — {}", status.join(" · "));
    s
}

/// Renders the board, optionally limited to one project tab.
pub fn render_text(board: &Board, config: &Config, tab: Option<&str>, now: i64) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Kanbr · {} · generated {}",
        board.home.as_deref().unwrap_or("?"),
        board.generated.as_deref().unwrap_or("?")
    );
    let mut tabs = vec![format!("All ({})", board.cards.len())];
    tabs.extend(board.projects.iter().map(|p| {
        if p.halted {
            format!("{} ({}, halted)", p.name, p.count)
        } else {
            format!("{} ({})", p.name, p.count)
        }
    }));
    let _ = writeln!(out, "Tabs: {}", tabs.join(" · "));
    let waiting: Vec<&str> = board
        .waiting()
        .into_iter()
        .filter(|c| tab.is_none_or(|t| c.project == t))
        .map(|c| c.id.as_str())
        .collect();
    if waiting.is_empty() {
        let _ = writeln!(out, "Nothing waiting on you");
    } else {
        let _ = writeln!(
            out,
            "⚑ {} waiting on you: {}",
            waiting.len(),
            waiting.join(", ")
        );
    }
    for n in &board.notices {
        let _ = writeln!(out, "Notice: {n}");
    }
    for column in Column::ALL {
        let cards = board.column_cards(column, tab);
        let _ = writeln!(out, "\n{} ({})", config.label(column), cards.len());
        for c in cards {
            let _ = writeln!(out, "{}", card_line(c, now));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{fixture_board, now};

    #[test]
    fn prints_every_column_with_labels() {
        let b = fixture_board();
        let cfg = Config {
            labels: [
                "Backlog",
                "Ready",
                "Building",
                "Dev",
                "Staging",
                "Production",
            ]
            .map(String::from),
            ..Config::default()
        };
        let text = render_text(&b, &cfg, None, now());
        for heading in [
            "Backlog (",
            "Ready (",
            "Building (",
            "Dev (",
            "Staging (",
            "Production (",
        ] {
            assert!(text.contains(heading), "{heading}\n{text}");
        }
        assert!(text.contains("⚑ 3 waiting on you"), "{text}");
        assert!(text.contains("legacy (2, halted)"), "{text}");
        assert!(text.contains("[after kanbr-1-board]"), "{text}");
        assert!(text.contains("Site 2nd #15"), "{text}");
    }

    #[test]
    fn tab_filter_limits_cards() {
        let b = fixture_board();
        let text = render_text(&b, &Config::default(), Some("firstmate"), now());
        assert!(text.contains("Nothing waiting on you"), "{text}");
        assert!(!text.contains("Shop ·"), "{text}");
        assert!(text.contains("firstmate"), "{text}");
    }
}
