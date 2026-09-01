//! Project-folder save semantics (docs/PROJECT_FOLDERS_DESIGN.md D3, section 3.2).
//!
//! A project is a folder containing at least one `.manifold` file (D1) — the
//! file itself is the identity, there is no separate marker. [`resolve_save_target`]
//! is the ONE place the version-vs-new-project rule lives, so the rfd dialog
//! stays a dumb shell and the rule is unit-testable (section 3.3).

use std::path::{Path, PathBuf};

/// Where a Save As lands, per Peter's rule (D3, verbatim): "when you first
/// save it, it makes a project unless it's already in a project, then it's a
/// version."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveTarget {
    /// `dir` already contains ≥1 `.manifold` → a sibling **version** inside
    /// that project folder. The caller appends `<stem>.manifold`; the name is
    /// the user's choice, never auto-suffixed (D3).
    Version { dir: PathBuf },
    /// `dir` contains no `.manifold` → a **new project**: the file lands in
    /// `dir/<Stem>/<Stem>.manifold`.
    NewProject { folder: PathBuf, file: PathBuf },
}

/// The save decision, as one pure function (D3 / section 3.2).
///
/// Reads `target_dir` for top-level `*.manifold` files and picks the variant.
/// No other I/O, no name mutation, no writes. `file_stem` passes through
/// untouched in both branches — a version name like `MyShow v2` stays exactly
/// `MyShow v2`.
pub fn resolve_save_target(target_dir: &Path, file_stem: &str) -> SaveTarget {
    if is_project_folder(target_dir) {
        SaveTarget::Version {
            dir: target_dir.to_path_buf(),
        }
    } else {
        let folder = target_dir.join(file_stem);
        let file = folder.join(format!("{file_stem}.manifold"));
        SaveTarget::NewProject { folder, file }
    }
}

/// D1: a project folder is detected by `.manifold` presence only — no marker
/// file, no index, no database. Scans only the top level, which is where a
/// version sibling would land.
pub fn is_project_folder(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|ext| ext == "manifold"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "manifold-project-folder-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folder_with_manifold_is_project() {
        let dir = temp_dir("is-project");
        std::fs::write(dir.join("MyShow.manifold"), b"v2").unwrap();
        assert!(is_project_folder(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_without_manifold_is_not_a_project() {
        let dir = temp_dir("not-a-project");
        std::fs::write(dir.join("notes.txt"), b"hi").unwrap();
        assert!(!is_project_folder(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_dir_resolves_to_new_project() {
        let dir = temp_dir("empty");
        assert_eq!(
            resolve_save_target(&dir, "MyShow"),
            SaveTarget::NewProject {
                folder: dir.join("MyShow"),
                file: dir.join("MyShow").join("MyShow.manifold"),
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_with_manifold_resolves_to_version() {
        let dir = temp_dir("version");
        std::fs::write(dir.join("MyShow.manifold"), b"v2").unwrap();
        assert_eq!(resolve_save_target(&dir, "MyShow v2"), SaveTarget::Version { dir: dir.clone() });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_stem_passes_through_untouched() {
        // D3: no auto-suffixing. `resolve_save_target` only picks the variant;
        // the caller appends the user's exact stem. "MyShow v2" must never
        // become "MyShow v2 (1)".
        let dir = temp_dir("no-suffix");
        std::fs::write(dir.join("MyShow.manifold"), b"v2").unwrap();
        match resolve_save_target(&dir, "MyShow v2") {
            SaveTarget::Version { dir: d } => {
                assert_eq!(d.join("MyShow v2.manifold").file_stem().unwrap(), "MyShow v2");
            }
            _ => panic!("expected Version"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
