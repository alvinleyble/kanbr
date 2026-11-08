//! What differs on Windows: how Firstmate's bash scripts are run, and how the
//! paths they take and print are spelled.
//!
//! Firstmate's scripts are `#!/usr/bin/env bash` files. Unix runs them
//! directly; Windows cannot execute a `.sh`, so they go through Git Bash.
//! Under Git Bash a drive is `/d/alvin-workspace`, while native programs (and
//! Kanbr) say `D:/alvin-workspace` or `D:\alvin-workspace`; Firstmate itself
//! prints `D:/...`. Everything here is a no-op on Unix.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `/d/alvin-workspace` as `D:/alvin-workspace`: the Git Bash spelling of a
/// drive path in the Windows one. `None` for anything that is not one (a
/// Windows path, a relative path, a Unix path with a longer first component).
pub fn msys_to_drive(path: &str) -> Option<String> {
    let rest = path.strip_prefix('/')?;
    let mut chars = rest.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    let tail = chars.as_str();
    if !tail.is_empty() && !tail.starts_with('/') {
        return None;
    }
    // `C:` alone means "the current directory of drive C", not its root.
    let tail = if tail.is_empty() { "/" } else { tail };
    Some(format!("{}:{}", letter.to_ascii_uppercase(), tail))
}

/// A path as typed by a person or written by Firstmate, as one this platform
/// can open. On Windows `/d/x` means `D:/x`; elsewhere paths are untouched.
pub fn native_path(path: &str) -> PathBuf {
    if cfg!(windows)
        && let Some(drive) = msys_to_drive(path)
    {
        return PathBuf::from(drive);
    }
    PathBuf::from(path)
}

/// A path as handed to a Firstmate script (environment or argument): on
/// Windows the forward-slash spelling Firstmate prints itself, never a mix of
/// slashes. Elsewhere the path as is.
pub fn script_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// Whether two paths name the same place by spelling: on Windows ignoring
/// slash direction, case, and a trailing slash; elsewhere exact.
pub fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let key = |p: &Path| {
            let s = script_path(p).to_lowercase();
            let s = msys_to_drive(&s).map_or(s, |d| d.to_lowercase());
            s.trim_end_matches('/').to_owned()
        };
        key(a) == key(b)
    } else {
        a == b
    }
}

/// Whether `c` separates path components in a path Firstmate printed.
pub fn is_separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

/// A command that runs the Firstmate script at `path`.
pub fn script_command(path: &Path) -> Result<Command, String> {
    #[cfg(windows)]
    {
        let bash = windows::bash()?;
        let mut cmd = Command::new(bash);
        cmd.arg(script_path(path));
        Ok(cmd)
    }
    #[cfg(not(windows))]
    {
        Ok(Command::new(path))
    }
}

#[cfg(windows)]
mod windows {
    use std::env;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    /// Git Bash, found once: `KANBR_BASH`, then `bash.exe` on `PATH`, then the
    /// bash that ships beside `git.exe`, then the usual Git for Windows
    /// folders. The WSL launcher (`System32\bash.exe`, `WindowsApps\bash.exe`)
    /// is never taken: it runs scripts in a Linux that has no Firstmate.
    pub fn bash() -> Result<&'static Path, String> {
        static BASH: OnceLock<Option<PathBuf>> = OnceLock::new();
        BASH.get_or_init(|| find_bash(env::var_os("KANBR_BASH"), env::var_os("PATH")))
            .as_deref()
            .ok_or_else(|| {
                "Git Bash was not found: install Git for Windows, or set KANBR_BASH to its bash.exe"
                    .to_owned()
            })
    }

    pub(super) fn is_wsl_launcher(path: &Path) -> bool {
        let p = path.to_string_lossy().to_lowercase().replace('\\', "/");
        p.contains("/windows/system32/") || p.contains("/windowsapps/")
    }

    fn find_bash(explicit: Option<OsString>, path: Option<OsString>) -> Option<PathBuf> {
        if let Some(b) = explicit.filter(|b| !b.is_empty()).map(PathBuf::from)
            && b.is_file()
        {
            return Some(b);
        }
        let dirs: Vec<PathBuf> = path
            .iter()
            .flat_map(|p| env::split_paths(p).collect::<Vec<_>>())
            .collect();
        let on_path = dirs
            .iter()
            .map(|d| d.join("bash.exe"))
            .find(|b| b.is_file() && !is_wsl_launcher(b));
        if on_path.is_some() {
            return on_path;
        }
        // Git for Windows: <root>\cmd\git.exe or <root>\mingw64\bin\git.exe,
        // with <root>\bin\bash.exe and <root>\usr\bin\bash.exe.
        for git in dirs
            .iter()
            .map(|d| d.join("git.exe"))
            .filter(|g| g.is_file())
        {
            for root in git.ancestors().skip(1).take(3) {
                for rel in ["bin/bash.exe", "usr/bin/bash.exe"] {
                    let b = root.join(rel);
                    if b.is_file() {
                        return Some(b);
                    }
                }
            }
        }
        ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"]
            .iter()
            .filter_map(env::var_os)
            .flat_map(|base| {
                let base = PathBuf::from(base);
                [
                    base.join("Git/bin/bash.exe"),
                    base.join("Programs/Git/bin/bash.exe"),
                ]
            })
            .find(|b| b.is_file())
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::windows::{bash, is_wsl_launcher};
    use std::path::Path;

    #[test]
    fn the_wsl_launcher_is_never_git_bash() {
        assert!(is_wsl_launcher(Path::new(r"C:\WINDOWS\system32\bash.exe")));
        assert!(is_wsl_launcher(Path::new(
            r"C:\Users\a\AppData\Local\Microsoft\WindowsApps\bash.exe"
        )));
        assert!(!is_wsl_launcher(Path::new(
            r"C:\Program Files\Git\usr\bin\bash.exe"
        )));
    }

    #[test]
    fn finds_git_bash_here() {
        // Firstmate runs under Git Bash on Windows, so one must be found.
        let found = bash().expect("Git Bash is installed");
        assert!(found.is_file());
        assert!(!is_wsl_launcher(found), "{}", found.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_git_bash_drives_the_windows_way() {
        assert_eq!(
            msys_to_drive("/d/alvin-workspace").as_deref(),
            Some("D:/alvin-workspace")
        );
        assert_eq!(msys_to_drive("/c").as_deref(), Some("C:/"));
        assert_eq!(msys_to_drive("/c/").as_deref(), Some("C:/"));
        assert_eq!(msys_to_drive("/usr/bin"), None);
        assert_eq!(msys_to_drive("/d1/x"), None);
        assert_eq!(msys_to_drive("D:/x"), None);
        assert_eq!(msys_to_drive("d/x"), None);
        assert_eq!(msys_to_drive(""), None);
        assert_eq!(msys_to_drive("/"), None);
    }

    #[test]
    fn native_path_changes_only_windows_spellings() {
        assert_eq!(native_path("relative/x"), PathBuf::from("relative/x"));
        assert_eq!(native_path("D:/x"), PathBuf::from("D:/x"));
        if cfg!(windows) {
            assert_eq!(native_path("/d/x"), PathBuf::from("D:/x"));
        } else {
            assert_eq!(native_path("/d/x"), PathBuf::from("/d/x"));
        }
    }

    #[test]
    fn script_paths_use_one_slash_direction_on_windows() {
        let p = Path::new(r"D:\alvin-workspace\bin");
        if cfg!(windows) {
            assert_eq!(script_path(p), "D:/alvin-workspace/bin");
        } else {
            assert_eq!(script_path(p), r"D:\alvin-workspace\bin");
        }
        assert_eq!(script_path(Path::new("/fm/bin")), "/fm/bin");
    }

    #[test]
    fn same_path_ignores_windows_spelling_only_on_windows() {
        assert!(same_path(Path::new("/fm"), Path::new("/fm")));
        assert!(!same_path(Path::new("/fm"), Path::new("/fm2")));
        if cfg!(windows) {
            assert!(same_path(
                Path::new("D:/alvin-workspace/"),
                Path::new(r"d:\Alvin-Workspace")
            ));
            assert!(same_path(
                Path::new("/d/alvin-workspace"),
                Path::new("D:/alvin-workspace")
            ));
            assert!(!same_path(Path::new("D:/a"), Path::new("D:/b")));
        }
    }

    #[test]
    fn separators() {
        assert!(is_separator('/'));
        assert_eq!(is_separator('\\'), cfg!(windows));
        assert!(!is_separator('a'));
    }

    #[test]
    fn a_script_command_names_the_script() {
        let cmd = script_command(Path::new("/fm/bin/x.sh"));
        if cfg!(windows) {
            // Git Bash is required on Windows; where it is absent the error says so.
            match cmd {
                Ok(c) => assert!(c.get_args().any(|a| a == "/fm/bin/x.sh")),
                Err(e) => assert!(e.contains("Git Bash")),
            }
        } else {
            assert_eq!(cmd.unwrap().get_program(), "/fm/bin/x.sh");
        }
    }
}
