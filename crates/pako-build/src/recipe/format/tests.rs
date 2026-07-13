use std::fs;

use tempfile::TempDir;

use super::load;
use crate::recipe::{Assertion, Transform};

#[test]
fn normalizes_a_minimal_local_recipe() {
    let fixture = TempDir::new().unwrap();
    fs::create_dir_all(fixture.path().join("payload")).unwrap();
    fs::write(
        fixture.path().join("payload/hello"),
        "#!/bin/sh\necho hello\n",
    )
    .unwrap();
    fs::write(
        fixture.path().join("recipe.toml"),
        r#"
schema = 1
name = "hello"
version = "1.0.0"
summary = "Small example"
license = "MIT"
executables = ["bin/hello"]

[commands]
hello = "bin/hello"

[source.x86_64]
path = "payload/hello"
to = "bin/hello"
"#,
    )
    .unwrap();

    let recipe = load(&fixture.path().join("recipe.toml")).unwrap();
    recipe.validate().unwrap();

    assert_eq!(recipe.package.release, 1);
    assert_eq!(recipe.metadata.display_name, "hello");
    assert_eq!(recipe.metadata.description, "Small example");
    assert_eq!(recipe.targets[0].platform, "linux/x86_64");
    assert!(recipe.targets[0].sources[0].hash.starts_with("sha256:"));
    assert!(recipe.transforms.iter().any(|transform| matches!(
        transform,
        Transform::Chmod { path, mode } if path == "bin/hello" && mode == "0755"
    )));
    assert!(recipe.assertions.iter().any(|assertion| matches!(
        assertion,
        Assertion::Path { path, executable: true, .. } if path == "bin/hello"
    )));
    assert_eq!(recipe.integrations.launchers[0].name, "hello");
}

#[test]
fn infers_remote_archive_format() {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("recipe.toml"),
        r#"
schema = 1
name = "editor"
version = "2.0.0"
summary = "Editor"
license = "MIT"

[source.aarch64]
url = "https://example.invalid/editor.tar.gz?download=1"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
strip = 1
"#,
    )
    .unwrap();

    let recipe = load(&fixture.path().join("recipe.toml")).unwrap();
    recipe.validate().unwrap();
    let source = &recipe.targets[0].sources[0];

    assert_eq!(recipe.targets[0].platform, "linux/aarch64");
    assert_eq!(source.format.as_deref(), Some("tar.gz"));
    assert_eq!(source.strip_components, 1);
    assert!(source.hash.starts_with("sha256:"));
}

#[test]
fn rejects_unpinned_remote_sources() {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("recipe.toml"),
        r#"
schema = 1
name = "editor"
version = "2.0.0"
summary = "Editor"
license = "MIT"

[source.x86_64]
url = "https://example.invalid/editor.zip"
"#,
    )
    .unwrap();

    let error = load(&fixture.path().join("recipe.toml")).unwrap_err();
    assert!(error.to_string().contains("must define sha256"));
}

#[test]
fn rejects_unknown_fields() {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("recipe.toml"),
        r#"
schema = 1
name = "editor"
version = "2.0.0"
summary = "Editor"
license = "MIT"
unknown = true
"#,
    )
    .unwrap();

    let error = load(&fixture.path().join("recipe.toml")).unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}
