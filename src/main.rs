//! Kanbr: a Kanban board for Firstmate work inside Herdr.

mod config;
mod dates;
mod doctor;
mod firstmate;
mod forge;
mod git;
mod herdr;
mod json;
mod loader;
mod model;
mod notes;
mod print;
mod releases;
mod ui;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use config::{Config, resolve_home};
use loader::Loader;

const USAGE: &str = "\
kanbr - a Kanban board for Firstmate work inside Herdr

Usage:
  kanbr [--home PATH] [--config PATH]           open the board
  kanbr print [--tab PROJECT] [--home PATH]     print the board once as text
  kanbr notes [--tab PROJECT] [--home PATH]     print release notes for the Live release
  kanbr doctor [--home PATH]                    verify every Firstmate surface Kanbr reads
  kanbr open [--home PATH]                      open the board in its own Herdr workspace
  kanbr --version | --help

Options:
  --home PATH     Firstmate home (else KANBR_FM_HOME, FM_HOME, `home` in the
                  config file, the current directory, or ~/firstmate)
  --config PATH   config file (else $HERDR_PLUGIN_CONFIG_DIR/config, then
                  $XDG_CONFIG_HOME/kanbr/config or ~/.config/kanbr/config)
  --tab PROJECT   with print or notes: only that project

Kanbr only reads Firstmate state; it never changes it.
";

#[derive(Debug, Default, PartialEq)]
struct Args {
    command: Option<String>,
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    tab: Option<String>,
    help: bool,
    version: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "-h" | "--help" => out.help = true,
            "-V" | "--version" => out.version = true,
            "--home" => out.home = Some(PathBuf::from(value("--home")?)),
            "--config" => out.config = Some(PathBuf::from(value("--config")?)),
            "--tab" => out.tab = Some(value("--tab")?),
            "help" if out.command.is_none() => out.help = true,
            cmd if !cmd.starts_with('-') && out.command.is_none() => match cmd {
                "board" | "print" | "notes" | "doctor" | "open" => {
                    out.command = Some(cmd.to_owned())
                }
                _ => return Err(format!("unknown command `{cmd}`")),
            },
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    if out.tab.is_some() && !matches!(out.command.as_deref(), Some("print" | "notes")) {
        return Err("--tab only applies to `kanbr print` and `kanbr notes`".to_owned());
    }
    Ok(out)
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("kanbr: {msg}");
    ExitCode::from(1)
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("kanbr: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.version {
        println!("kanbr {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.command.as_deref() == Some("doctor") {
        let checks = doctor::run(args.home.as_deref(), args.config.as_deref());
        let (text, code) = doctor::report(&checks);
        print!("{text}");
        return ExitCode::from(code as u8);
    }

    let config = match Config::load(args.config.as_deref()) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let home = resolve_home(args.home.as_deref(), &config);
    if args.command.as_deref() == Some("open") {
        // Open the board even when the home is unresolved: the board then shows
        // the reason in its pane instead of only in Herdr's plugin log.
        if let Err(e) = &home {
            eprintln!("kanbr: {e}");
        }
        let home = home.as_ref().ok().map(|(h, _)| h.as_path());
        return match herdr::open(home, config.source.as_deref()) {
            Ok(msg) => {
                println!("kanbr: {msg}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        };
    }
    let home = match home {
        Ok((home, _)) => home,
        Err(e) => return fail(&e),
    };

    match args.command.as_deref() {
        Some(cmd @ ("print" | "notes")) => {
            let loader = Loader::new(home, config.clone());
            match loader.load() {
                Ok(board) => {
                    if let Some(t) = &args.tab
                        && !board.projects.iter().any(|p| &p.name == t)
                    {
                        return fail(&format!(
                            "no tab `{t}`: that project has no work on the board"
                        ));
                    }
                    if cmd == "notes" {
                        print!("{}", notes::release_notes(&board, args.tab.as_deref()));
                        return ExitCode::SUCCESS;
                    }
                    print!(
                        "{}",
                        print::render_text(
                            &board,
                            &config,
                            args.tab.as_deref(),
                            dates::now_epoch()
                        )
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&format!(
                    "{e}\nRun `kanbr doctor` to check every surface Kanbr reads."
                )),
            }
        }
        _ => {
            if !std::io::stdout().is_terminal() {
                return fail("the board needs a terminal; use `kanbr print` for text output");
            }
            match ui::run(Loader::new(home, config)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&format!("terminal error: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_commands_and_flags() {
        assert_eq!(parse(&[]).unwrap(), Args::default());
        assert_eq!(
            parse(&["notes", "--tab", "Shop"]).unwrap().tab.as_deref(),
            Some("Shop")
        );
        let a = parse(&["print", "--home", "/fm", "--tab=Shop"]).unwrap();
        assert_eq!(a.command.as_deref(), Some("print"));
        assert_eq!(a.home, Some(PathBuf::from("/fm")));
        assert_eq!(a.tab.as_deref(), Some("Shop"));
        assert!(parse(&["help"]).unwrap().help);
        assert!(parse(&["--version"]).unwrap().version);
        assert_eq!(
            parse(&["doctor", "--config=/c"]).unwrap().config,
            Some(PathBuf::from("/c"))
        );
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&["--home"]).is_err());
        assert!(parse(&["--home="]).is_err());
        assert!(parse(&["--tab", "Shop"]).is_err());
        assert!(parse(&["print", "doctor"]).is_err());
        assert!(parse(&["--wat"]).is_err());
    }
}
