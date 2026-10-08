use std::path::{Path, PathBuf};

/// Every production source tree that owns the standalone primitive mirrors.
/// Keep these paths explicit: a crate move must update the proof rather than
/// silently shrinking its walk.
pub const PRIMITIVE_SOURCE_ROOTS: &[&str] = &[
    "../manifold-nodes-scene/src/node_graph/primitives",
    "../manifold-nodes-image/src/node_graph/primitives",
    "src/node_graph/primitives",
    "../manifold-node-engine/src/primitives",
    "../manifold-node-engine/src/water/primitives",
];

/// Every crate `src` root that currently owns WGSL. A new WGSL-owning crate
/// must be named here explicitly; the guard below discovers shader
/// subdirectories beneath these roots.
pub const WGSL_SRC_ROOTS: &[&str] = &[
    "../manifold-nodes-scene/src",
    "../manifold-nodes-image/src",
    "../manifold-led/src",
    "../manifold-node-engine/src",
    "../manifold-recording/src",
    "src",
    "../manifold-spectral/src",
];

pub fn primitive_source_roots() -> Result<Vec<PathBuf>, String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    PRIMITIVE_SOURCE_ROOTS
        .iter()
        .map(|relative| {
            let path = manifest.join(relative);
            if path.is_dir() {
                path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))
            } else {
                Err(format!("missing ABI source root: {}", path.display()))
            }
        })
        .collect()
}

fn collect_wgsl_roots(dir: &Path, roots: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .path();
        if path.is_dir() {
            collect_wgsl_roots(&path, roots)?;
        } else if path.extension().is_some_and(|ext| ext == "wgsl")
            && let Some(parent) = path.parent()
        {
            let parent = parent.to_path_buf();
            if !roots.contains(&parent) {
                roots.push(parent);
            }
        }
    }
    Ok(())
}

pub fn verify_wgsl_roots() -> Result<(), String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let expected: Vec<PathBuf> = WGSL_SRC_ROOTS
        .iter()
        .map(|root| {
            let path = manifest.join(root);
            path.canonicalize()
                .map_err(|e| format!("{}: {e}", path.display()))
        })
        .collect::<Result<_, _>>()?;
    let mut actual = Vec::new();
    let crates = manifest
        .parent()
        .ok_or("renderer manifest has no workspace crates parent")?;
    for entry in std::fs::read_dir(crates).map_err(|e| format!("{}: {e}", crates.display()))? {
        let path = entry
            .map_err(|e| format!("{}: {e}", crates.display()))?
            .path()
            .join("src");
        if path.is_dir() {
            let mut shader_dirs = Vec::new();
            collect_wgsl_roots(&path, &mut shader_dirs)?;
            if !shader_dirs.is_empty() {
                actual.push(
                    path.canonicalize()
                        .map_err(|e| format!("{}: {e}", path.display()))?,
                );
            }
        }
    }
    let missing: Vec<_> = expected
        .iter()
        .filter(|path| !actual.contains(path))
        .map(|path| path.display().to_string())
        .collect();
    let unexpected: Vec<_> = actual
        .iter()
        .filter(|path| !expected.contains(path))
        .map(|path| path.display().to_string())
        .collect();
    if missing.is_empty() && unexpected.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "WGSL source-root inventory drift; missing: {missing:?}; unexpected existing roots: {unexpected:?}"
        ))
    }
}
