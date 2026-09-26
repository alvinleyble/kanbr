//! The Herdr plugin manifest and the crate stay in step.

const MANIFEST: &str = include_str!("../herdr-plugin.toml");

fn manifest_value(key: &str) -> Option<String> {
    MANIFEST.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned())
    })
}

#[test]
fn plugin_version_matches_the_crate() {
    assert_eq!(
        manifest_value("version").as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn plugin_builds_and_runs_the_release_binary() {
    assert_eq!(manifest_value("id").as_deref(), Some("kanbr"));
    assert!(MANIFEST.contains(r#"command = ["sh", "scripts/install.sh"]"#));
    assert!(MANIFEST.contains(r#"command = ["./target/release/kanbr", "open"]"#));
    assert!(
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh")).is_file()
    );
}
