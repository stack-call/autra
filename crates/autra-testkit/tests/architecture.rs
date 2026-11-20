use std::collections::BTreeSet;
use std::process::Command;

use serde_json::Value;

struct Package {
    name: String,
    normal: BTreeSet<String>,
}

fn workspace_packages() -> Vec<Package> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .expect("cargo metadata should run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("metadata JSON");
    metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|package| {
            let name = package["name"].as_str().expect("package name").to_owned();
            let mut normal = BTreeSet::new();
            for dependency in package["dependencies"].as_array().expect("dependencies") {
                let dep_name = dependency["name"].as_str().expect("dep name").to_owned();
                if !dep_name.starts_with("autra") {
                    continue;
                }
                if dependency["kind"].is_null() {
                    normal.insert(dep_name);
                }
            }
            Package { name, normal }
        })
        .collect()
}

fn package<'a>(packages: &'a [Package], name: &str) -> &'a Package {
    packages
        .iter()
        .find(|package| package.name == name)
        .unwrap_or_else(|| panic!("missing workspace package `{name}`"))
}

#[test]
fn workspace_dependency_direction_matches_the_architecture() {
    let packages = workspace_packages();

    let core = package(&packages, "autra-core");
    assert!(
        core.normal.is_empty(),
        "autra-core must not depend on Autra crates"
    );

    let execution = package(&packages, "autra-execution");
    assert_eq!(
        execution.normal,
        BTreeSet::from(["autra-core".to_owned()]),
        "autra-execution may depend on autra-core only"
    );

    let harness = package(&packages, "autra-harness");
    assert!(
        harness.normal.is_subset(&BTreeSet::from([
            "autra-core".to_owned(),
            "autra-execution".to_owned()
        ])),
        "autra-harness may depend on core and execution only: {:?}",
        harness.normal
    );

    let runtime = package(&packages, "autra-runtime");
    assert!(
        runtime.normal.is_subset(&BTreeSet::from([
            "autra-core".to_owned(),
            "autra-execution".to_owned()
        ])),
        "autra-runtime must not depend on harness or the facade: {:?}",
        runtime.normal
    );

    let durable = package(&packages, "autra-durable-sqlite");
    assert!(
        durable.normal.is_subset(&BTreeSet::from([
            "autra-core".to_owned(),
            "autra-execution".to_owned()
        ])),
        "durable adapters must not depend on harness, runtime or the facade: {:?}",
        durable.normal
    );

    let facade = package(&packages, "autra");
    assert!(
        facade.normal.contains("autra-harness")
            && facade.normal.contains("autra-runtime")
            && facade.normal.contains("autra-execution"),
        "the facade assembles harness, execution and runtime"
    );
}

#[test]
fn cli_and_eval_use_the_facade_not_the_runtime() {
    let packages = workspace_packages();
    for name in ["autra-cli", "autra-eval"] {
        let package = package(&packages, name);
        assert!(
            package.normal.contains("autra"),
            "{name} should depend on the facade"
        );
        assert!(
            !package.normal.contains("autra-runtime"),
            "{name} should not depend on runtime directly"
        );
    }
}
