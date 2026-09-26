//! `kanbr open`: the Herdr action. Opens the board in its own "Kanbr"
//! workspace, or focuses it when it is already running there, and restarts it
//! in place when the pane is left at a shell prompt. Everything goes through
//! the Herdr CLI (`$HERDR_BIN_PATH`), so it works from the plugin action, a
//! keybinding, or a shell inside Herdr.

use std::env;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::firstmate::run_bounded;
use crate::json::{arr_at, str_at};

pub const WORKSPACE_LABEL: &str = "Kanbr";
pub const PANE_LABEL: &str = "Kanbr";

fn normalize(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The id of the workspace labelled `label` (case- and space-insensitive).
pub fn find_workspace(list: &Value, label: &str) -> Option<String> {
    let want = normalize(label);
    arr_at(list, "result.workspaces")
        .iter()
        .find(|w| str_at(w, "label").is_some_and(|l| normalize(l) == want))
        .and_then(|w| str_at(w, "workspace_id"))
        .map(str::to_owned)
}

/// `(pane_id, tab_id)` of the pane labelled `label`.
pub fn find_pane(list: &Value, label: &str) -> Option<(String, Option<String>)> {
    let want = normalize(label);
    arr_at(list, "result.panes")
        .iter()
        .find(|p| str_at(p, "label").is_some_and(|l| normalize(l) == want))
        .and_then(|p| {
            let id = str_at(p, "pane_id")?.to_owned();
            Some((id, str_at(p, "tab_id").map(str::to_owned)))
        })
}

/// Whether a pane's foreground process is a running board.
pub fn board_running(process_info: &Value) -> bool {
    arr_at(process_info, "result.process_info.foreground_processes")
        .iter()
        .any(|p| {
            let argv0 = str_at(p, "argv0")
                .or_else(|| arr_at(p, "argv").first().and_then(Value::as_str))
                .or_else(|| str_at(p, "name"))
                .unwrap_or("");
            Path::new(argv0).file_name().and_then(|n| n.to_str()) == Some("kanbr")
        })
}

/// A shell-quoted command line that starts the board for `home`, passing the
/// config file along so the board sees the same settings as the action.
/// Without a home, the board resolves it itself, and shows why it cannot.
pub fn board_command(
    exe: &Path,
    home: Option<&Path>,
    config: Option<&Path>,
) -> Result<String, String> {
    let quote = |p: &Path| -> Result<String, String> {
        let s = p
            .to_str()
            .ok_or_else(|| format!("path is not UTF-8: {}", p.display()))?;
        if s.contains('\'') || s.contains('\n') {
            return Err(format!("path contains a quote or newline: {s}"));
        }
        Ok(format!("'{s}'"))
    };
    let mut cmd = quote(exe)?;
    if let Some(h) = home {
        cmd.push_str(&format!(" --home {}", quote(h)?));
    }
    if let Some(c) = config {
        cmd.push_str(&format!(" --config {}", quote(c)?));
    }
    Ok(cmd)
}

struct Herdr {
    bin: OsString,
}

impl Herdr {
    fn from_env() -> Self {
        let bin = env::var_os("HERDR_BIN_PATH")
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| OsString::from("herdr"));
        Herdr { bin }
    }

    fn raw(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let what = format!("herdr {}", args.join(" "));
        match run_bounded(cmd, Duration::from_secs(15)) {
            Ok((Some(status), out, _)) if status.success() => Ok(out),
            Ok((Some(_), out, err)) => {
                let msg = if err.trim().is_empty() { out } else { err };
                Err(format!("{what} failed: {}", msg.trim()))
            }
            Ok((None, _, _)) => Err(format!("{what} timed out")),
            Err(e) => Err(format!("cannot run {}: {e}", self.bin.to_string_lossy())),
        }
    }

    fn json(&self, args: &[&str]) -> Result<Value, String> {
        let out = self.raw(args)?;
        serde_json::from_str(&out)
            .map_err(|e| format!("herdr {} printed invalid JSON: {e}", args.join(" ")))
    }

    fn focus(&self, workspace: &str, tab: Option<&str>) -> Result<(), String> {
        self.raw(&["workspace", "focus", workspace])?;
        if let Some(tab) = tab {
            self.raw(&["tab", "focus", tab])?;
        }
        Ok(())
    }

    fn start_board(&self, pane: &str, command: &str) -> Result<(), String> {
        self.raw(&["pane", "rename", pane, PANE_LABEL])?;
        self.raw(&["pane", "run", pane, command])?;
        Ok(())
    }
}

/// Opens or focuses the board; returns a one-line description of what it did.
/// `home` is `None` when it could not be resolved: the board then opens anyway
/// and states the problem in its pane, where the captain can see it.
pub fn open(home: Option<&Path>, config: Option<&Path>) -> Result<String, String> {
    let exe = env::current_exe().map_err(|e| format!("cannot locate the kanbr binary: {e}"))?;
    let command = board_command(&exe, home, config)?;
    let herdr = Herdr::from_env();
    let cwd = match home {
        Some(h) => h.to_path_buf(),
        None => env::var_os("HOME")
            .map(Into::into)
            .unwrap_or_else(|| "/".into()),
    };
    let home_str = cwd.to_str().ok_or("Firstmate home path is not UTF-8")?;

    let workspaces = herdr.json(&["workspace", "list"])?;
    let Some(ws) = find_workspace(&workspaces, WORKSPACE_LABEL) else {
        let created = herdr.json(&[
            "workspace",
            "create",
            "--label",
            WORKSPACE_LABEL,
            "--cwd",
            home_str,
            "--focus",
        ])?;
        let ws = str_at(&created, "result.workspace.workspace_id")
            .ok_or("herdr workspace create returned no workspace id")?;
        let pane = str_at(&created, "result.root_pane.pane_id")
            .ok_or("herdr workspace create returned no root pane")?;
        herdr.start_board(pane, &command)?;
        return Ok(format!("opened the board in new workspace {ws}"));
    };

    let panes = herdr.json(&["pane", "list", "--workspace", &ws])?;
    match find_pane(&panes, PANE_LABEL) {
        Some((pane, tab)) => {
            let info = herdr.json(&["pane", "process-info", "--pane", &pane])?;
            herdr.focus(&ws, tab.as_deref())?;
            if board_running(&info) {
                Ok(format!("focused the running board in workspace {ws}"))
            } else {
                herdr.raw(&["pane", "run", &pane, &command])?;
                Ok(format!("restarted the board in workspace {ws}"))
            }
        }
        None => {
            let created = herdr.json(&[
                "tab",
                "create",
                "--workspace",
                &ws,
                "--label",
                WORKSPACE_LABEL,
                "--cwd",
                home_str,
                "--focus",
            ])?;
            let pane = str_at(&created, "result.root_pane.pane_id")
                .ok_or("herdr tab create returned no root pane")?;
            herdr.start_board(pane, &command)?;
            herdr.focus(&ws, str_at(&created, "result.tab.tab_id"))?;
            Ok(format!("opened the board in a new tab of workspace {ws}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_the_kanbr_workspace() {
        let list = json!({"result": {"type": "workspace_list", "workspaces": [
            {"workspace_id": "w1", "label": "api"},
            {"workspace_id": "w7", "label": "  kanbr "},
            {"workspace_id": "w9", "label": null}
        ]}});
        assert_eq!(
            find_workspace(&list, WORKSPACE_LABEL).as_deref(),
            Some("w7")
        );
        assert_eq!(
            find_workspace(&json!({"result": {"workspaces": []}}), "Kanbr"),
            None
        );
        assert_eq!(find_workspace(&json!({}), "Kanbr"), None);
    }

    #[test]
    fn finds_the_board_pane() {
        let list = json!({"result": {"panes": [
            {"pane_id": "w7:p1", "tab_id": "w7:t1"},
            {"pane_id": "w7:p2", "tab_id": "w7:t2", "label": "Kanbr"}
        ]}});
        assert_eq!(
            find_pane(&list, PANE_LABEL),
            Some(("w7:p2".to_owned(), Some("w7:t2".to_owned())))
        );
        assert_eq!(
            find_pane(&json!({"result": {"panes": null}}), PANE_LABEL),
            None
        );
    }

    #[test]
    fn detects_a_running_board() {
        let running = json!({"result": {"process_info": {"foreground_processes": [
            {"argv": ["/opt/kanbr/target/release/kanbr", "--home", "/fm"], "argv0": "/opt/kanbr/target/release/kanbr", "name": "kanbr"}
        ]}}});
        assert!(board_running(&running));
        let shell = json!({"result": {"process_info": {"foreground_processes": [
            {"argv": ["-zsh"], "argv0": "-zsh", "name": "zsh"}
        ]}}});
        assert!(!board_running(&shell));
        assert!(!board_running(
            &json!({"result": {"process_info": {"foreground_processes": null}}})
        ));
    }

    #[test]
    fn quotes_the_board_command() {
        let cmd = board_command(Path::new("/a b/kanbr"), Some(Path::new("/fm home")), None);
        assert_eq!(cmd.unwrap(), "'/a b/kanbr' --home '/fm home'");
        let with_config = board_command(
            Path::new("/k"),
            Some(Path::new("/fm")),
            Some(Path::new("/c/config")),
        );
        assert_eq!(
            with_config.unwrap(),
            "'/k' --home '/fm' --config '/c/config'"
        );
        assert_eq!(board_command(Path::new("/k"), None, None).unwrap(), "'/k'");
        assert!(board_command(Path::new("/it's/kanbr"), Some(Path::new("/fm")), None).is_err());
    }
}
