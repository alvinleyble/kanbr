//! The Herdr plugin manifest and the crate stay in step.

use toml::Table;

fn manifest() -> Table {
    include_str!("../herdr-plugin.toml")
        .parse()
        .expect("herdr-plugin.toml parses")
}

fn command(entry: &toml::Value) -> Vec<&str> {
    entry["command"]
        .as_array()
        .expect("command is an array")
        .iter()
        .map(|v| v.as_str().expect("command parts are strings"))
        .collect()
}

#[test]
fn plugin_version_matches_the_crate() {
    assert_eq!(
        manifest()["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn plugin_builds_and_runs_the_release_binary() {
    let m = manifest();
    assert_eq!(m["id"].as_str(), Some("kanbr"));
    let build = m["build"].as_array().expect("build steps");
    let platforms = |e: &toml::Value| -> Vec<String> {
        e["platforms"]
            .as_array()
            .expect("platforms")
            .iter()
            .map(|p| p.as_str().expect("platform names are strings").to_owned())
            .collect()
    };
    // Unix downloads the release binary or builds with cargo; Windows has no
    // `sh` for Herdr to run, so it builds with cargo directly.
    assert_eq!(build.len(), 2);
    assert_eq!(command(&build[0]), ["sh", "scripts/install.sh"]);
    assert_eq!(platforms(&build[0]), ["linux", "macos"]);
    assert_eq!(
        command(&build[1]),
        ["cargo", "build", "--release", "--locked"]
    );
    assert_eq!(platforms(&build[1]), ["windows"]);
    assert_eq!(
        m["platforms"].as_array().map(Vec::len),
        Some(3),
        "linux, macos, windows"
    );
    let actions = m["actions"].as_array().expect("actions");
    let open = actions
        .iter()
        .find(|a| a["id"].as_str() == Some("open"))
        .expect("open action");
    assert_eq!(command(open), ["./target/release/kanbr", "open"]);
    assert!(
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh")).is_file()
    );
}
