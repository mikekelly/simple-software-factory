use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn repository_root_is_an_installable_widget_only_plugin() {
    let root = repo();
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json")).expect("root manifest.json must exist"),
    )
    .expect("root manifest.json must be valid JSON");
    assert_eq!(manifest["schemaVersion"], 1);
    assert_eq!(manifest["id"], "ssf.factory");
    assert_eq!(manifest["kinds"], serde_json::json!(["bar-widget"]));
    assert_eq!(
        manifest["entryPoints"]["barWidget"],
        "marketplace/FactoryPanel.qml"
    );
    let entry = manifest["entryPoints"]["barWidget"].as_str().unwrap();
    assert!(root.join(entry).is_file(), "widget entry point must exist");

    let mut files: Vec<_> = std::fs::read_dir(root.join("marketplace"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    files.sort();
    assert_eq!(
        files,
        ["FactoryPanel.qml"],
        "plugin payload must be QML-only"
    );
}

#[test]
fn widget_probes_the_packaged_application_without_bootstrapping_it() {
    let panel = std::fs::read_to_string(repo().join("marketplace/FactoryPanel.qml")).unwrap();
    for expected in [
        "readonly property string applicationPath: \"/usr/bin/ssf\"",
        "[\"/usr/bin/test\", \"-x\", root.applicationPath]",
        "[root.applicationPath, \"ui\", \"service\", \"status\", \"--json\"]",
        "[root.applicationPath, \"status\", \"--json\"]",
        "root.serviceStatus.configured !== true",
        "missing-package",
        "missing-configuration",
        "service-failed",
        "service-stopped",
    ] {
        assert!(panel.contains(expected), "widget is missing {expected:?}");
    }
    for forbidden in [
        "ssf-marketplace",
        "needs-install",
        "needs-update",
        "setupProc",
        "bootstrapProc",
        "mise",
        "cargo build",
        "systemctl --user enable",
        "pacman -S",
    ] {
        assert!(
            !panel.contains(forbidden),
            "obsolete lifecycle behavior: {forbidden:?}"
        );
    }
}

#[test]
fn widget_changes_state_only_from_explicit_controls() {
    let panel = std::fs::read_to_string(repo().join("marketplace/FactoryPanel.qml")).unwrap();
    assert!(panel.contains("onClicked: root.configure()"));
    assert!(panel.contains(
        "root.run(\"omarchy-launch-terminal \" + root.applicationPath + \" dashboard\")"
    ));
    assert!(panel.contains("root.applicationPath + \" setup\""));
    assert!(panel.contains("onToggled: root.toggleService()"));
    assert!(panel.contains("root.applicationPath + \" ui service toggle\""));
    assert_eq!(panel.matches("root.configure()").count(), 1);
    assert_eq!(panel.matches("root.toggleService()").count(), 2);
}

#[test]
fn missing_states_have_actionable_and_honest_messages() {
    let panel = std::fs::read_to_string(repo().join("marketplace/FactoryPanel.qml")).unwrap();
    assert!(panel.contains("sudo pacman -U <package-file>"));
    assert!(panel.contains("Choose Configure to run `ssf setup` in a terminal."));
    assert!(panel.contains("The Software Factory service failed. Open Logs for details."));
    assert!(panel.contains("Could not read the Software Factory service state"));
    assert!(!panel.contains("A newer Software Factory runtime is available"));
}

#[test]
fn plugin_installation_has_no_application_lifecycle_hooks() {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(repo().join("manifest.json")).unwrap()).unwrap();
    for forbidden in [
        "install",
        "uninstall",
        "removeHook",
        "postInstall",
        "preRemove",
    ] {
        assert!(
            manifest.get(forbidden).is_none(),
            "manifest owns lifecycle hook: {forbidden}"
        );
    }
    assert_eq!(manifest["entryPoints"].as_object().unwrap().len(), 1);
}
