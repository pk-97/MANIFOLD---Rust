pub mod blob_ffi;
pub mod depth_ffi;
pub mod region_ffi;

use std::path::{Path, PathBuf};

/// A plugin's library under `assets/plugins`: the Mach-O inside its bundle
/// on macOS, `lib<name>.so` on Linux.
fn plugin_library(name: &str) -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from(format!("{name}.bundle/Contents/MacOS/{name}"))
    } else {
        PathBuf::from(format!(
            "{}{name}{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ))
    }
}

/// Resolve the path to a native plugin library.
///
/// Search order: explicit `MANIFOLD_{NAME}_PLUGIN`, executable-relative
/// assets, development executable root, current-directory assets, then the
/// build workspace's assets in debug builds (Cargo tests run from crate dirs).
pub fn resolve_bundle_path(name: &str) -> Option<PathBuf> {
    let env_key = format!("MANIFOLD_{}_PLUGIN", name.to_uppercase());
    if let Ok(path) = std::env::var(&env_key) {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        let candidate = exe_dir.join("assets/plugins").join(plugin_library(name));
        if candidate.exists() {
            return Some(candidate);
        }
        if let Some(project_dir) = exe_dir.parent().and_then(|p| p.parent()) {
            let candidate = project_dir
                .join("assets/plugins")
                .join(plugin_library(name));
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    let cwd_candidate = Path::new("assets/plugins").join(plugin_library(name));
    if cwd_candidate.exists() {
        return Some(cwd_candidate);
    }

    #[cfg(debug_assertions)]
    {
        let development_candidate = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/plugins")
            .join(plugin_library(name));
        if development_candidate.exists() {
            return Some(development_candidate);
        }
    }

    None
}
