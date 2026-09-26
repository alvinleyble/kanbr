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
    assert_eq!(command(&build[0]), ["sh", "scripts/install.sh"]);
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
