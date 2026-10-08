use std::path::Path;
use std::process::Command;

#[test]
fn testkit_visibility_uses_the_calling_crate_configuration() {
    let root = std::env::temp_dir().join(format!("manifold-testkit-visibility-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let owner = root.join("libvisibility_owner.rlib");
    let output = Command::new(&compiler)
        .args(["--edition=2024", "--crate-name", "visibility_owner", "--crate-type=rlib"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/testkit_visibility.rs"))
        .args(["--cfg", "feature=\"testkit\"", "-o"])
        .arg(&owner)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let caller = root.join("caller.rs");
    std::fs::write(&caller, r#"
pub mod surface {
    visibility_owner::testkit_visible! { pub(crate) struct Probe; }
}
pub use surface::Probe;
"#).unwrap();
    for configuration in [None, Some("feature=\"testkit\""), Some("test")] {
        let mut command = Command::new(&compiler);
        command.args(["--edition=2024", "--crate-type=rlib"])
            .arg(&caller)
            .arg("--extern").arg(format!("visibility_owner={}", owner.display()))
            .arg("--out-dir").arg(&root);
        if let Some(cfg) = configuration {
            command.args(["--cfg", cfg]);
        }
        let output = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if configuration.is_none() {
            assert!(!output.status.success(), "production item was publicly reachable");
            assert!(stderr.contains("E0364"), "unexpected failure: {stderr}");
        } else {
            assert!(output.status.success(), "{configuration:?}: {stderr}");
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
