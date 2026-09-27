use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const NATIVE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "inc", "in", "inl", "ipp", "tpp",
];
const UNITS_SOURCE: &str = "../manifold-foundation/src/units.rs";

pub fn emit_source_identity(
    crate_root: &Path,
    relative_roots: &[&str],
    env_var: &str,
) -> io::Result<String> {
    let mut files = metadata_files(crate_root);
    let mut watch_roots = Vec::with_capacity(relative_roots.len());
    for relative_root in relative_roots {
        let path = crate_root.join(relative_root);
        watch_roots.push(path.clone());
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some("native") {
                collect_native(&path, crate_root, &mut files)?;
            } else {
                collect_rust_path(&path, relative_root, &mut files)?;
            }
        } else if path.is_file() && !is_test_file(&path) {
            files.push((normalize_relative(relative_root), path));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("selected source root does not exist: {relative_root}"),
            ));
        }
    }
    add_selected_file(crate_root, UNITS_SOURCE, &mut files)?;
    emit_files(
        crate_root,
        &watch_roots,
        &mut files,
        "scripts/native_source_identity.rs",
        env_var,
    )
}

fn metadata_files(crate_root: &Path) -> Vec<(String, PathBuf)> {
    vec![
        ("Cargo.toml".to_string(), crate_root.join("Cargo.toml")),
        ("build.rs".to_string(), crate_root.join("build.rs")),
    ]
}

fn add_selected_file(
    crate_root: &Path,
    relative: &str,
    files: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    let path = crate_root.join(relative);
    if !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("selected source file does not exist: {relative}"),
        ));
    }
    files.push((normalize_relative(relative), path));
    Ok(())
}

fn emit_files(
    crate_root: &Path,
    watch_roots: &[PathBuf],
    files: &mut Vec<(String, PathBuf)>,
    shared_helper: &str,
    env_var: &str,
) -> io::Result<String> {
    let shared_path = crate_root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "crate has no repository root"))?
        .join(shared_helper);
    files.push((normalize_relative(shared_helper), shared_path.clone()));
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files.dedup_by(|left, right| left.0 == right.0);
    for path in rerun_paths(crate_root, watch_roots, files, &shared_path) {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let mut hasher = Sha256::new();
    for (relative, path) in files {
        let bytes = fs::read(path)?;
        hash_bytes(&mut hasher, relative.as_bytes());
        hash_bytes(&mut hasher, &bytes);
    }
    let identity = format!("{:x}", hasher.finalize());
    println!("cargo:rustc-env={env_var}={identity}");
    Ok(identity)
}

fn collect_native(
    directory: &Path,
    crate_root: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_native(&path, crate_root, files)?;
        } else if path.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| NATIVE_EXTENSIONS.contains(&extension))
        {
            files.push((relative_path(crate_root, &path)?, path));
        }
    }
    Ok(())
}

fn collect_rust_path(
    directory: &Path,
    logical_root: &str,
    files: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let logical = format!("{}/{}", normalize_relative(logical_root), name);
        if path.is_dir() {
            if name == "tests" {
                continue;
            }
            collect_rust_path(&path, &logical, files)?;
        } else if path.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("rs")
            && !is_test_file(&path)
        {
            files.push((normalize_relative(&logical), path));
        }
    }
    Ok(())
}

fn is_test_file(path: &Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| {
            matches!(stem, "test" | "tests") || stem.ends_with("_test") || stem.ends_with("_tests")
        })
}

fn relative_path(crate_root: &Path, path: &Path) -> io::Result<String> {
    path.strip_prefix(crate_root)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source escaped crate root"))
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

fn rerun_paths(
    crate_root: &Path,
    watch_roots: &[PathBuf],
    files: &[(String, PathBuf)],
    shared_path: &Path,
) -> Vec<PathBuf> {
    let mut paths = vec![
        crate_root.join("Cargo.toml"),
        crate_root.join("build.rs"),
        shared_path.to_path_buf(),
    ];
    paths.extend(watch_roots.iter().cloned());
    paths.extend(files.iter().map(|(_, path)| path.clone()));
    paths.sort();
    paths.dedup();
    paths
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn normalize_relative(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
        crate_root: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "manifold-source-identity-{label}-{}-{}-{}",
                std::process::id(),
                nonce,
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            let crate_root = root.join("crates/demo");
            fs::create_dir_all(crate_root.join("native/include")).unwrap();
            fs::create_dir_all(crate_root.join("src/tests")).unwrap();
            fs::create_dir_all(root.join("crates/manifold-foundation/src")).unwrap();
            fs::create_dir_all(root.join("scripts")).unwrap();
            fs::write(
                crate_root.join("Cargo.toml"),
                "[package]\nname = \"demo\"\n",
            )
            .unwrap();
            fs::write(crate_root.join("build.rs"), "fn main() {}\n").unwrap();
            fs::write(crate_root.join("src/lib.rs"), "pub mod physics;\n").unwrap();
            fs::write(
                crate_root.join("src/physics.rs"),
                "pub const VALUE: u32 = 1;\n",
            )
            .unwrap();
            fs::write(
                crate_root.join("src/material.rs"),
                "pub const MATERIAL: u32 = 1;\n",
            )
            .unwrap();
            fs::write(crate_root.join("src/tests/ignored.rs"), "ignored\n").unwrap();
            fs::write(crate_root.join("src/physics_tests.rs"), "ignored\n").unwrap();
            fs::write(
                crate_root.join("native/bridge.c"),
                "int bridge(void) { return 1; }\n",
            )
            .unwrap();
            fs::write(crate_root.join("native/include/base.h"), "#define BASE 1\n").unwrap();
            fs::write(
                root.join("crates/manifold-foundation/src/units.rs"),
                "pub struct Seconds(pub f64);\n",
            )
            .unwrap();
            fs::write(
                root.join("scripts/native_source_identity.rs"),
                include_str!("native_source_identity.rs"),
            )
            .unwrap();
            Self { root, crate_root }
        }

        fn identity(&self, roots: &[&str]) -> String {
            emit_source_identity(&self.crate_root, roots, "TEST_SOURCE_IDENTITY").unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn identity_is_stable_under_relocation_and_root_order() {
        let first = Fixture::new("relocated-a");
        let second = Fixture::new("relocated-b");
        assert_eq!(
            first.identity(&["native", "src"]),
            second.identity(&["native", "src"])
        );
        assert_eq!(
            first.identity(&["native", "src"]),
            first.identity(&["src", "native"])
        );
    }

    #[test]
    fn identity_changes_for_native_bytes_and_added_headers() {
        let fixture = Fixture::new("native");
        let baseline = fixture.identity(&["native", "src"]);
        fs::write(
            fixture.crate_root.join("native/bridge.c"),
            "int bridge(void) { return 2; }\n",
        )
        .unwrap();
        assert_ne!(baseline, fixture.identity(&["native", "src"]));
        let changed = fixture.identity(&["native", "src"]);
        fs::write(
            fixture.crate_root.join("native/include/added.inl"),
            "#define ADDED 1\n",
        )
        .unwrap();
        assert_ne!(changed, fixture.identity(&["native", "src"]));
    }

    #[test]
    fn identity_changes_for_units_and_helper_bytes() {
        let fixture = Fixture::new("shared");
        let baseline = fixture.identity(&["native", "src"]);
        fs::write(
            fixture
                .crate_root
                .join("../manifold-foundation/src/units.rs"),
            "pub struct Seconds(pub f32);\n",
        )
        .unwrap();
        assert_ne!(baseline, fixture.identity(&["native", "src"]));
        let changed_units = fixture.identity(&["native", "src"]);
        fs::write(
            fixture.root.join("scripts/native_source_identity.rs"),
            "changed helper\n",
        )
        .unwrap();
        assert_ne!(changed_units, fixture.identity(&["native", "src"]));
    }

    #[test]
    fn selected_identity_ignores_tests_and_unselected_materials() {
        let fixture = Fixture::new("selected");
        let selected_baseline = fixture.identity(&["src/physics.rs"]);
        fs::write(
            fixture.crate_root.join("src/material.rs"),
            "pub const MATERIAL: u32 = 2;\n",
        )
        .unwrap();
        assert_eq!(selected_baseline, fixture.identity(&["src/physics.rs"]));
        let entire_src_baseline = fixture.identity(&["src"]);
        fs::write(
            fixture.crate_root.join("src/tests/ignored.rs"),
            "changed ignored test\n",
        )
        .unwrap();
        fs::write(
            fixture.crate_root.join("src/physics_tests.rs"),
            "changed ignored test\n",
        )
        .unwrap();
        assert_eq!(entire_src_baseline, fixture.identity(&["src"]));
        fs::write(
            fixture.crate_root.join("src/physics.rs"),
            "pub const VALUE: u32 = 2;\n",
        )
        .unwrap();
        assert_ne!(selected_baseline, fixture.identity(&["src/physics.rs"]));
    }

    #[test]
    fn identity_reports_missing_selected_source() {
        let fixture = Fixture::new("missing");
        let error =
            emit_source_identity(&fixture.crate_root, &["native", "src/missing.rs"], "TEST")
                .expect_err("missing source must fail");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
