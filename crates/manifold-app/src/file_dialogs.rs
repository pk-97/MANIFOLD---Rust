//! Every native file dialog in the app opens through here.
//!
//! Each action remembers its own folder, so video import goes back to the
//! footage folder and preset export to the presets folder, independently.
//! When the remembered folder is gone (an unplugged drive), the action falls
//! back to the open project's folder, then a sensible home folder — never to
//! whatever folder the last unrelated panel used. The stored value is kept, so
//! the folder comes back when the drive does.
//!
//! The panel attaches to the editor window the action came from (never the
//! output window). It blocks the UI thread while open; the content thread
//! keeps rendering and presenting output, so the show never freezes behind a
//! dialog.

use std::path::{Path, PathBuf};

use crate::user_prefs::UserPrefs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogAction {
    OpenProject,
    SaveProjectAs,
    ExportVideo,
    ExportFrame,
    ImportVideo,
    ImportMidi,
    ImportModel,
    ImportPreset,
    ExportPreset,
    ReplaceAudio,
    NodeFile,
    NodeFolder,
}

/// Where an action looks after its remembered folder, past the project folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HomeFallback {
    None,
    Desktop,
    Movies,
    Music,
}

struct Spec {
    title: &'static str,
    filters: &'static [(&'static str, &'static [&'static str])],
    /// Persisted key. Existing keys keep their pre-module names so remembered
    /// folders survive the upgrade.
    pref_key: &'static str,
    /// Whether the open project's folder is a useful starting point.
    project_folder: bool,
    home: HomeFallback,
}

const VIDEO_EXTS: &[&str] = crate::project_io::VIDEO_EXTENSIONS;
const AUDIO_EXTS: &[&str] = crate::project_io::AUDIO_EXTENSIONS;

impl DialogAction {
    fn spec(self) -> Spec {
        use HomeFallback as H;
        let (title, filters, pref_key, project_folder, home): (_, &'static [(&'static str, &'static [&'static str])], _, _, _) = match self {
            Self::OpenProject => ("Open MANIFOLD Project", &[("MANIFOLD Project", &["manifold", "json"])], "MANIFOLD_DialogPath_ProjectOpen", false, H::None),
            Self::SaveProjectAs => ("Save MANIFOLD Project", &[("MANIFOLD Project", &["manifold"])], "MANIFOLD_DialogPath_ProjectSave", false, H::None),
            Self::ExportVideo => ("Export Video", &[("MP4 Video", &["mp4"])], "MANIFOLD_DialogPath_ExportMP4", true, H::Desktop),
            Self::ExportFrame => ("Export Frame", &[("PNG Image", &["png"]), ("JPEG Image", &["jpg", "jpeg"])], "MANIFOLD_DialogPath_ExportImage", true, H::Desktop),
            Self::ImportVideo => ("Import Video", &[("Video Files", VIDEO_EXTS)], "MANIFOLD_DialogPath_VideoImport", true, H::Movies),
            Self::ImportMidi => ("Import MIDI File", &[("MIDI Files", &["mid", "midi"])], "MANIFOLD_DialogPath_MidiImport", false, H::None),
            Self::ImportModel => ("Import Model", &[("glTF", &["glb", "gltf"])], "MANIFOLD_DialogPath_ModelImport", true, H::None),
            Self::ImportPreset => ("Import Preset", &[("MANIFOLD Preset", &["json"])], "MANIFOLD_DialogPath_PresetImport", false, H::None),
            Self::ExportPreset => ("Export Preset", &[("MANIFOLD Preset", &["json"])], "MANIFOLD_DialogPath_PresetExport", false, H::None),
            Self::ReplaceAudio => ("Replace Audio File", &[("Audio", AUDIO_EXTS)], "MANIFOLD_DialogPath_AudioReplace", true, H::Music),
            Self::NodeFile => ("Choose File", &[], "MANIFOLD_DialogPath_NodeFile", true, H::None),
            Self::NodeFolder => ("Choose Folder", &[], "MANIFOLD_DialogPath_NodeFolder", true, H::None),
        };
        Spec { title, filters, pref_key, project_folder, home }
    }
}

/// What a dialog needs from the window that opened it.
#[derive(Clone, Copy, Default)]
pub(crate) struct DialogHost<'a> {
    /// The editor window to attach the panel to. `None` floats it (headless
    /// callers, or no editor window yet).
    pub parent: Option<&'a winit::window::Window>,
    /// The open project's `.manifold` file, if it has been saved.
    pub project_path: Option<&'a Path>,
}

/// The host for a dialog opened by the app. `dialog_window` is the editor
/// window the user last focused (main or graph editor); if it has closed, the
/// main window. An output window is never a parent — the panel would land on
/// the projector.
pub(crate) fn host<'a>(
    registry: &'a crate::window_registry::WindowRegistry,
    dialog_window: Option<winit::window::WindowId>,
    primary_window: Option<winit::window::WindowId>,
    project_path: Option<&'a Path>,
) -> DialogHost<'a> {
    let editor = |id: Option<winit::window::WindowId>| {
        id.and_then(|id| registry.get(&id))
            .filter(|ws| matches!(ws.role, crate::window_registry::WindowRole::Workspace))
            .map(|ws| ws.window.as_ref())
    };
    DialogHost {
        parent: editor(dialog_window).or_else(|| editor(primary_window)),
        project_path,
    }
}

pub(crate) fn pick_file(action: DialogAction, host: DialogHost, prefs: &mut UserPrefs) -> Option<PathBuf> {
    let path = build(action, host, prefs).pick_file()?;
    remember(action, &path, prefs);
    Some(path)
}

pub(crate) fn pick_files(action: DialogAction, host: DialogHost, prefs: &mut UserPrefs) -> Option<Vec<PathBuf>> {
    let paths = build(action, host, prefs).pick_files()?;
    if let Some(first) = paths.first() {
        remember(action, first, prefs);
    }
    Some(paths)
}

pub(crate) fn pick_folder(action: DialogAction, host: DialogHost, prefs: &mut UserPrefs) -> Option<PathBuf> {
    let path = build(action, host, prefs).pick_folder()?;
    remember(action, &path, prefs);
    Some(path)
}

/// Save panel with `default_name` in the name field. Remembers the folder the
/// user picked in the panel, not wherever the caller later writes the file
/// (Save As may create a project folder inside it).
pub(crate) fn save_file(
    action: DialogAction,
    host: DialogHost,
    prefs: &mut UserPrefs,
    default_name: &str,
) -> Option<PathBuf> {
    let path = build(action, host, prefs).set_file_name(default_name).save_file()?;
    remember(action, &path, prefs);
    Some(path)
}

fn build(action: DialogAction, host: DialogHost, prefs: &UserPrefs) -> rfd::FileDialog {
    let spec = action.spec();
    let mut dialog = rfd::FileDialog::new()
        .set_title(spec.title)
        .set_can_create_directories(true);
    for (name, exts) in spec.filters {
        dialog = dialog.add_filter(*name, exts);
    }
    let remembered = Some(prefs.get_string(spec.pref_key, ""))
        .filter(|s| !s.is_empty())
        .map(|s| memory_dir(action, Path::new(&s)));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(dir) = start_dir(action, remembered, host.project_path, home.as_deref(), |p| p.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    if let Some(window) = host.parent {
        dialog = dialog.set_parent(window);
    }
    dialog
}

fn remember(action: DialogAction, picked: &Path, prefs: &mut UserPrefs) {
    if let Some(dir) = picked.parent().filter(|d| !d.as_os_str().is_empty()) {
        let dir = memory_dir(action, dir);
        prefs.set_string(action.spec().pref_key, &dir.to_string_lossy());
        prefs.save();
    }
}

/// Save As remembers where new projects go. A project folder is never that
/// place: an untitled project saved into one would become a version of the
/// old show (D3), so its parent is remembered instead.
fn memory_dir(action: DialogAction, dir: &Path) -> PathBuf {
    if action == DialogAction::SaveProjectAs
        && manifold_io::project_folder::is_project_folder(dir)
        && let Some(parent) = dir.parent()
    {
        return parent.to_path_buf();
    }
    dir.to_path_buf()
}

/// The folder a dialog opens in. Pure, so the fallback rules are testable.
///
/// Save As on a saved project starts in that project's folder, so it saves a
/// new version there (docs/PROJECT_FOLDERS_DESIGN.md D3); an untitled project
/// starts where the last new project was put.
fn start_dir(
    action: DialogAction,
    remembered: Option<PathBuf>,
    project_path: Option<&Path>,
    home: Option<&Path>,
    is_dir: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let spec = action.spec();
    let project_dir = project_path.and_then(Path::parent).filter(|d| is_dir(d));
    if action == DialogAction::SaveProjectAs
        && let Some(dir) = project_dir
    {
        return Some(dir.to_path_buf());
    }
    if let Some(dir) = remembered.filter(|d| is_dir(d)) {
        return Some(dir);
    }
    if action == DialogAction::OpenProject {
        // The folder that holds the current show — where its siblings live.
        return project_dir.and_then(Path::parent).filter(|d| is_dir(d)).map(Path::to_path_buf);
    }
    if spec.project_folder
        && let Some(dir) = project_dir
    {
        return Some(dir.to_path_buf());
    }
    let sub = match spec.home {
        HomeFallback::None => return None,
        HomeFallback::Desktop => "Desktop",
        HomeFallback::Movies => "Movies",
        HomeFallback::Music => "Music",
    };
    home.map(|h| h.join(sub)).filter(|d| is_dir(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [DialogAction; 12] = [
        DialogAction::OpenProject,
        DialogAction::SaveProjectAs,
        DialogAction::ExportVideo,
        DialogAction::ExportFrame,
        DialogAction::ImportVideo,
        DialogAction::ImportMidi,
        DialogAction::ImportModel,
        DialogAction::ImportPreset,
        DialogAction::ExportPreset,
        DialogAction::ReplaceAudio,
        DialogAction::NodeFile,
        DialogAction::NodeFolder,
    ];

    fn dirs(existing: &[&str]) -> impl Fn(&Path) -> bool {
        let set: Vec<PathBuf> = existing.iter().map(PathBuf::from).collect();
        move |p: &Path| set.iter().any(|d| d == p)
    }

    #[test]
    fn every_action_has_its_own_memory() {
        let mut keys: Vec<&str> = ALL.iter().map(|a| a.spec().pref_key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ALL.len());
    }

    #[test]
    fn existing_memory_keys_keep_their_names() {
        assert_eq!(DialogAction::OpenProject.spec().pref_key, "MANIFOLD_DialogPath_ProjectOpen");
        assert_eq!(DialogAction::SaveProjectAs.spec().pref_key, "MANIFOLD_DialogPath_ProjectSave");
        assert_eq!(DialogAction::ExportVideo.spec().pref_key, "MANIFOLD_DialogPath_ExportMP4");
        assert_eq!(DialogAction::ExportFrame.spec().pref_key, "MANIFOLD_DialogPath_ExportImage");
        assert_eq!(DialogAction::ImportMidi.spec().pref_key, "MANIFOLD_DialogPath_MidiImport");
    }

    #[test]
    fn remembered_folder_wins() {
        let got = start_dir(
            DialogAction::ImportVideo,
            Some("/Footage".into()),
            Some(Path::new("/Shows/A/A.manifold")),
            Some(Path::new("/Users/p")),
            dirs(&["/Footage", "/Shows/A", "/Users/p/Movies"]),
        );
        assert_eq!(got, Some("/Footage".into()));
    }

    #[test]
    fn missing_drive_falls_back_to_project_then_home() {
        let is_dir = dirs(&["/Shows/A", "/Users/p/Movies"]);
        let got = start_dir(
            DialogAction::ImportVideo,
            Some("/Volumes/GigDrive/Footage".into()),
            Some(Path::new("/Shows/A/A.manifold")),
            Some(Path::new("/Users/p")),
            &is_dir,
        );
        assert_eq!(got, Some("/Shows/A".into()));
        let got = start_dir(
            DialogAction::ImportVideo,
            Some("/Volumes/GigDrive/Footage".into()),
            None,
            Some(Path::new("/Users/p")),
            &is_dir,
        );
        assert_eq!(got, Some("/Users/p/Movies".into()));
    }

    #[test]
    fn save_as_on_saved_project_starts_in_its_folder() {
        let got = start_dir(
            DialogAction::SaveProjectAs,
            Some("/Shows".into()),
            Some(Path::new("/Shows/A/A.manifold")),
            None,
            dirs(&["/Shows", "/Shows/A"]),
        );
        assert_eq!(got, Some("/Shows/A".into()));
    }

    #[test]
    fn save_as_untitled_uses_remembered_folder() {
        let got = start_dir(
            DialogAction::SaveProjectAs,
            Some("/Shows".into()),
            None,
            None,
            dirs(&["/Shows"]),
        );
        assert_eq!(got, Some("/Shows".into()));
    }

    #[test]
    fn open_falls_back_to_folder_holding_current_show() {
        let got = start_dir(
            DialogAction::OpenProject,
            None,
            Some(Path::new("/Shows/A/A.manifold")),
            None,
            dirs(&["/Shows", "/Shows/A"]),
        );
        assert_eq!(got, Some("/Shows".into()));
    }

    #[test]
    fn save_as_never_remembers_inside_a_show() {
        let shows = std::env::temp_dir().join(format!("manifold-file-dialogs-{}", std::process::id()));
        let show = shows.join("MyShow");
        std::fs::create_dir_all(&show).unwrap();
        std::fs::write(show.join("MyShow.manifold"), b"v2").unwrap();
        assert_eq!(memory_dir(DialogAction::SaveProjectAs, &show), shows);
        assert_eq!(memory_dir(DialogAction::SaveProjectAs, &shows), shows);
        assert_eq!(memory_dir(DialogAction::ExportVideo, &show), show);
        let _ = std::fs::remove_dir_all(&shows);
    }

    #[test]
    fn nothing_known_lets_macos_choose() {
        for action in [DialogAction::ImportPreset, DialogAction::ImportMidi, DialogAction::NodeFile] {
            assert_eq!(start_dir(action, None, None, Some(Path::new("/Users/p")), dirs(&["/Users/p"])), None);
        }
    }
}
