//! User configuration and Firstmate home resolution.
//!
//! Kanbr is one public tool that each user adapts through a small config file
//! instead of a fork (decision 23): column labels, the branches that back Dev,
//! Staging, and Live, the words that mark grills and halted projects, and
//! which requests prompt for a passphrase all live here with defaults matching
//! the workflow Kanbr was designed for. The file is optional; its format is
//! `key = value` lines with `#` comments. It never holds a passphrase.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::firstmate::SNAPSHOT_SCRIPT;
use crate::model::Column;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Firstmate home from the config file (`home = /path`).
    pub home: Option<PathBuf>,
    /// Words in a hold reason or title that mark a Booked card as needing a grill.
    pub grill_words: Vec<String>,
    /// Words in a hold reason that mark the item's whole project as halted (greyed, paused).
    pub halted_words: Vec<String>,
    /// How many days finished work that git does not place stays on the
    /// board: merged work whose PR is not in its project's history yet, and
    /// (while no newer release clears it) finished work with no PR.
    pub finished_days: i64,
    /// Seconds between cheap change checks of the Firstmate home.
    pub interval_secs: u64,
    /// Upper bound in seconds between full snapshot reads, for state that changes without files.
    pub refresh_secs: u64,
    /// Display labels for the six columns, in board order.
    pub labels: [String; 6],
    /// The branch that backs Dev; empty means no project uses Dev.
    pub dev_branch: String,
    /// The branch that backs Staging; empty means no project uses Staging.
    pub staging_branch: String,
    /// The branch that backs Live; empty means no project uses Live.
    pub live_branch: String,
    /// Release lanes whose requests prompt for that branch's passphrase: a
    /// merge or promotion that lands on one of them.
    pub passphrase_lanes: Vec<Column>,
    /// Minutes a request waits for its outcome before the card snaps back.
    pub request_timeout_mins: i64,
    /// Where the config was read from, if anywhere.
    pub source: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            home: None,
            grill_words: vec!["grill".to_owned()],
            halted_words: vec!["halted".to_owned()],
            finished_days: 7,
            interval_secs: 2,
            refresh_secs: 15,
            labels: Column::ALL.map(|c| c.title().to_owned()),
            dev_branch: "dev".to_owned(),
            staging_branch: "staging".to_owned(),
            live_branch: "main".to_owned(),
            passphrase_lanes: vec![Column::Staging, Column::Live],
            request_timeout_mins: 30,
            source: None,
        }
    }
}

impl Config {
    /// Loads the config from `explicit`, or the first existing default location.
    /// A missing default file is not an error; a missing explicit file is.
    pub fn load(explicit: Option<&Path>) -> Result<Config, String> {
        if let Some(path) = explicit {
            let text = fs::read_to_string(path)
                .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
            return Config::parse(&text, path);
        }
        for path in default_config_paths() {
            if let Ok(text) = fs::read_to_string(&path) {
                return Config::parse(&text, &path);
            }
        }
        Ok(Config::default())
    }

    pub fn parse(text: &str, path: &Path) -> Result<Config, String> {
        let mut cfg = Config {
            source: Some(path.to_path_buf()),
            ..Config::default()
        };
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!(
                    "{}:{}: expected `key = value`",
                    path.display(),
                    n + 1
                ));
            };
            let (key, value) = (key.trim(), value.trim());
            let bad = |what: &str| format!("{}:{}: {key} {what}", path.display(), n + 1);
            match key {
                "home" => cfg.home = Some(expand_tilde(value)),
                "grill_words" => cfg.grill_words = word_list(value),
                "halted_words" => cfg.halted_words = word_list(value),
                "finished_days" => {
                    cfg.finished_days = value
                        .parse()
                        .ok()
                        .filter(|d: &i64| *d >= 0)
                        .ok_or_else(|| bad("must be a whole number of days"))?
                }
                "interval" => {
                    cfg.interval_secs = value
                        .parse()
                        .ok()
                        .filter(|s: &u64| *s > 0)
                        .ok_or_else(|| bad("must be a positive number of seconds"))?
                }
                "refresh" => {
                    cfg.refresh_secs = value
                        .parse()
                        .ok()
                        .filter(|s: &u64| *s > 0)
                        .ok_or_else(|| bad("must be a positive number of seconds"))?
                }
                "dev_branch" => cfg.dev_branch = value.to_owned(),
                "staging_branch" => cfg.staging_branch = value.to_owned(),
                "live_branch" => cfg.live_branch = value.to_owned(),
                "passphrase_lanes" => {
                    cfg.passphrase_lanes = lane_list(value)
                        .ok_or_else(|| bad("must list lanes from dev, staging, live"))?
                }
                "request_timeout" => {
                    cfg.request_timeout_mins = value
                        .parse()
                        .ok()
                        .filter(|m: &i64| *m > 0)
                        .ok_or_else(|| bad("must be a positive number of minutes"))?
                }
                _ => match LABEL_KEYS.iter().position(|k| *k == key) {
                    Some(i) if !value.is_empty() => cfg.labels[i] = value.to_owned(),
                    Some(_) => return Err(bad("must not be empty")),
                    None => {
                        return Err(format!("{}:{}: unknown key `{key}`", path.display(), n + 1));
                    }
                },
            }
        }
        let b = cfg.branches();
        for (i, name) in b.iter().enumerate() {
            if !name.is_empty() && b[i + 1..].contains(name) {
                return Err(format!(
                    "{}: dev_branch, staging_branch, and live_branch must be different branches (`{name}` backs two lanes)",
                    path.display()
                ));
            }
        }
        Ok(cfg)
    }

    /// The configured Dev, Staging, and Live branch names, in that order.
    pub fn branches(&self) -> [&str; 3] {
        [
            self.dev_branch.as_str(),
            self.staging_branch.as_str(),
            self.live_branch.as_str(),
        ]
    }

    pub fn label(&self, column: Column) -> &str {
        &self.labels[column.index()]
    }

    /// The branch that backs a release lane (empty when the lane is off).
    pub fn branch(&self, column: Column) -> &str {
        match column {
            Column::Dev => &self.dev_branch,
            Column::Staging => &self.staging_branch,
            Column::Live => &self.live_branch,
            _ => "",
        }
    }
}

/// `staging, live` as lanes; `None` for anything else.
fn lane_list(value: &str) -> Option<Vec<Column>> {
    let mut lanes = Vec::new();
    for word in word_list(value) {
        let lane = match word.as_str() {
            "dev" => Column::Dev,
            "staging" => Column::Staging,
            "live" => Column::Live,
            _ => return None,
        };
        if !lanes.contains(&lane) {
            lanes.push(lane);
        }
    }
    Some(lanes)
}

/// Config keys for the column labels, in board order.
pub const LABEL_KEYS: [&str; 6] = [
    "booked_label",
    "ready_label",
    "building_label",
    "dev_label",
    "staging_label",
    "live_label",
];

fn word_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn expand_tilde(value: &str) -> PathBuf {
    match value.strip_prefix("~/") {
        Some(rest) => home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(value)),
        None => PathBuf::from(value),
    }
}

/// Config locations in lookup order: the Herdr plugin config directory (when
/// run by Herdr), then `$XDG_CONFIG_HOME/kanbr/config` or `~/.config/kanbr/config`.
pub fn default_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = env::var_os("HERDR_PLUGIN_CONFIG_DIR").filter(|d| !d.is_empty()) {
        paths.push(PathBuf::from(dir).join("config"));
    }
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        paths.push(PathBuf::from(dir).join("kanbr").join("config"));
    } else if let Some(home) = home_dir() {
        paths.push(home.join(".config").join("kanbr").join("config"));
    }
    paths
}

/// How the Firstmate home was chosen, for `kanbr doctor`.
#[derive(Clone, Debug, PartialEq)]
pub enum HomeSource {
    Flag,
    KanbrEnv,
    FmHomeEnv,
    ConfigFile(PathBuf),
    WorkingDirectory,
    DefaultLocation,
}

impl HomeSource {
    pub fn describe(&self) -> String {
        match self {
            HomeSource::Flag => "--home flag".to_owned(),
            HomeSource::KanbrEnv => "KANBR_FM_HOME".to_owned(),
            HomeSource::FmHomeEnv => "FM_HOME".to_owned(),
            HomeSource::ConfigFile(p) => format!("config {}", p.display()),
            HomeSource::WorkingDirectory => "current directory".to_owned(),
            HomeSource::DefaultLocation => "~/firstmate".to_owned(),
        }
    }
}

pub fn is_firstmate_home(path: &Path) -> bool {
    path.join(SNAPSHOT_SCRIPT).is_file()
}

/// Resolves the Firstmate home. Explicit choices (flag, environment, config)
/// must point at a real home and fail loudly otherwise; implicit discovery
/// (working directory ancestors, `~/firstmate`) is tried only when none is set.
pub fn resolve_home(flag: Option<&Path>, cfg: &Config) -> Result<(PathBuf, HomeSource), String> {
    let env_path = |name: &str| {
        env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let explicit: [(Option<PathBuf>, HomeSource); 4] = [
        (flag.map(Path::to_path_buf), HomeSource::Flag),
        (env_path("KANBR_FM_HOME"), HomeSource::KanbrEnv),
        (env_path("FM_HOME"), HomeSource::FmHomeEnv),
        (
            cfg.home.clone(),
            HomeSource::ConfigFile(cfg.source.clone().unwrap_or_default()),
        ),
    ];
    for (candidate, source) in explicit {
        if let Some(path) = candidate {
            if is_firstmate_home(&path) {
                return Ok((path, source));
            }
            return Err(format!(
                "{} points at {}, which is not a Firstmate home (no {SNAPSHOT_SCRIPT})",
                source.describe(),
                path.display()
            ));
        }
    }
    if let Ok(cwd) = env::current_dir()
        && let Some(found) = cwd.ancestors().find(|p| is_firstmate_home(p))
    {
        return Ok((found.to_path_buf(), HomeSource::WorkingDirectory));
    }
    if let Some(home) = home_dir() {
        let path = home.join("firstmate");
        if is_firstmate_home(&path) {
            return Ok((path, HomeSource::DefaultLocation));
        }
    }
    Err(format!(
        "no Firstmate home found. Pass --home PATH, set KANBR_FM_HOME, or add `home = PATH` to {}",
        cfg.source
            .clone()
            .or_else(|| default_config_paths().pop())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.config/kanbr/config".to_owned())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_key() {
        let text = "# comment\nhome = /fm\ngrill_words = Grill, workshop\nhalted_words=halted,frozen\nfinished_days = 3\ninterval = 1\nrefresh = 30\n";
        let cfg = Config::parse(text, Path::new("c")).unwrap();
        assert_eq!(cfg.home, Some(PathBuf::from("/fm")));
        assert_eq!(cfg.grill_words, vec!["grill", "workshop"]);
        assert_eq!(cfg.halted_words, vec!["halted", "frozen"]);
        assert_eq!(cfg.finished_days, 3);
        assert_eq!(cfg.interval_secs, 1);
        assert_eq!(cfg.refresh_secs, 30);
    }

    #[test]
    fn labels_and_branches() {
        let d = Config::default();
        assert_eq!(d.branches(), ["dev", "staging", "main"]);
        assert_eq!(d.label(Column::Booked), "Booked");
        assert_eq!(d.label(Column::Live), "Live");
        let text = "live_label = Production\nbooked_label = Backlog\nlive_branch = master\nstaging_branch =\n";
        let cfg = Config::parse(text, Path::new("c")).unwrap();
        assert_eq!(cfg.label(Column::Live), "Production");
        assert_eq!(cfg.label(Column::Booked), "Backlog");
        assert_eq!(cfg.label(Column::Ready), "Ready");
        assert_eq!(cfg.branches(), ["dev", "", "master"]);
        assert!(Config::parse("dev_label =", Path::new("c")).is_err());
    }

    #[test]
    fn passphrase_lanes_and_request_timeout() {
        let d = Config::default();
        assert_eq!(d.passphrase_lanes, vec![Column::Staging, Column::Live]);
        assert_eq!(d.request_timeout_mins, 30);
        let cfg = Config::parse(
            "passphrase_lanes = Live\nrequest_timeout = 5\n",
            Path::new("c"),
        )
        .unwrap();
        assert_eq!(cfg.passphrase_lanes, vec![Column::Live]);
        assert_eq!(cfg.request_timeout_mins, 5);
        let none = Config::parse("passphrase_lanes =", Path::new("c")).unwrap();
        assert!(none.passphrase_lanes.is_empty());
        assert!(Config::parse("passphrase_lanes = prod", Path::new("c")).is_err());
        assert!(Config::parse("request_timeout = 0", Path::new("c")).is_err());
        assert_eq!(d.branch(Column::Staging), "staging");
        assert_eq!(d.branch(Column::Ready), "");
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(Config::parse("nonsense", Path::new("c")).is_err());
        assert!(Config::parse("colour = red", Path::new("c")).is_err());
        assert!(Config::parse("interval = 0", Path::new("c")).is_err());
        assert!(Config::parse("finished_days = -1", Path::new("c")).is_err());
        let dup = Config::parse("staging_branch = main", Path::new("c")).unwrap_err();
        assert!(dup.contains("`main` backs two lanes"), "{dup}");
        assert!(Config::parse("dev_branch =\nstaging_branch =", Path::new("c")).is_ok());
    }

    #[test]
    fn empty_file_is_defaults() {
        let cfg = Config::parse("\n# only comments\n", Path::new("c")).unwrap();
        assert_eq!(cfg.grill_words, Config::default().grill_words);
        assert_eq!(cfg.finished_days, 7);
    }

    #[test]
    fn explicit_home_must_be_real() {
        let err = resolve_home(
            Some(Path::new("/definitely/not/a/home")),
            &Config::default(),
        );
        assert!(err.unwrap_err().contains("not a Firstmate home"));
    }
}
