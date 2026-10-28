//! Where MANIFOLD keeps its files and finds external tools on each OS.
//!
//! The Mac folder names are fixed: existing prefs, presets, stems and logs
//! already live there. Linux follows the XDG base directories.

use std::path::{Path, PathBuf};

/// The app's folders for one user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDirs {
    /// Prefs and user presets.
    pub data: PathBuf,
    /// The instance lock and the session sentinel.
    pub state: PathBuf,
    /// Session and crash logs.
    pub logs: PathBuf,
    /// Caches that can be rebuilt: compiled pipelines, decoded meshes, thumbnails.
    pub cache: PathBuf,
    /// Separated stems, apart from `cache` because they are slow to rebuild.
    pub stems: PathBuf,
}

impl AppDirs {
    /// The current user's folders, or `None` when `HOME` is unset.
    pub fn current() -> Option<Self> {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        Some(Self::from_home(Path::new(&home), |var| {
            std::env::var_os(var).map(PathBuf::from)
        }))
    }

    /// The folders under `home`, ignoring any XDG overrides.
    pub fn for_home(home: &Path) -> Self {
        Self::from_home(home, |_| None)
    }

    #[cfg(target_os = "macos")]
    fn from_home(home: &Path, _env: impl Fn(&str) -> Option<PathBuf>) -> Self {
        let support = home.join("Library/Application Support");
        Self {
            data: support.join("MANIFOLD"),
            state: support.join("com.latentspace.manifold"),
            logs: home.join("Library/Logs/com.latentspace.manifold"),
            cache: home.join("Library/Caches/com.latentspace.manifold"),
            stems: support.join("Manifold/StemCache"),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn from_home(home: &Path, env: impl Fn(&str) -> Option<PathBuf>) -> Self {
        // An XDG variable counts only when it holds an absolute path.
        let base = |var: &str, default: &str| {
            env(var)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default))
                .join("manifold")
        };
        let state = base("XDG_STATE_HOME", ".local/state");
        let cache = base("XDG_CACHE_HOME", ".cache");
        Self {
            data: base("XDG_DATA_HOME", ".local/share"),
            logs: state.join("logs"),
            stems: cache.join("stems"),
            state,
            cache,
        }
    }
}

/// Finds an external program such as `ffmpeg`. Looks in the usual install
/// folders first, because an app started from the Mac's Finder or Dock does
/// not get the shell's `PATH`, then along `PATH`.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    const INSTALL_DIRS: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];
    let path = std::env::var_os("PATH");
    INSTALL_DIRS
        .iter()
        .map(PathBuf::from)
        .chain(path.iter().flat_map(std::env::split_paths))
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_folders_keep_their_names() {
        let dirs = AppDirs::for_home(Path::new("/Users/test"));
        assert_eq!(
            dirs,
            AppDirs {
                data: "/Users/test/Library/Application Support/MANIFOLD".into(),
                state: "/Users/test/Library/Application Support/com.latentspace.manifold".into(),
                logs: "/Users/test/Library/Logs/com.latentspace.manifold".into(),
                cache: "/Users/test/Library/Caches/com.latentspace.manifold".into(),
                stems: "/Users/test/Library/Application Support/Manifold/StemCache".into(),
            }
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_folders_follow_xdg_and_ignore_relative_overrides() {
        let env = |var: &str| match var {
            "XDG_CACHE_HOME" => Some(PathBuf::from("/fast/cache")),
            "XDG_STATE_HOME" => Some(PathBuf::from("relative/state")),
            _ => None,
        };
        let dirs = AppDirs::from_home(Path::new("/home/test"), env);
        assert_eq!(
            dirs,
            AppDirs {
                data: "/home/test/.local/share/manifold".into(),
                state: "/home/test/.local/state/manifold".into(),
                logs: "/home/test/.local/state/manifold/logs".into(),
                cache: "/fast/cache/manifold".into(),
                stems: "/fast/cache/manifold/stems".into(),
            }
        );
    }
}
