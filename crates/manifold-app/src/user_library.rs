//! `UserLibrary` — the file-ops service behind "My Library"
//! (`docs/PRESET_LIBRARY_DESIGN.md` §4/§6 D4, phase P3).
//!
//! Writes/renames/duplicates/deletes standalone preset JSON files under the
//! SAME user preset root `manifold_renderer::preset_loader` already resolves
//! read-only (`~/Library/Application Support/MANIFOLD/presets/{effects,
//! generators}`), so a save here is picked up by the existing hot-reload
//! watcher with no separate wiring — no new storage tier, just a writer for
//! the one that already exists.
//!
//! App-side (not `manifold-ui`/`manifold-core`, per the repo hard rule):
//! file IO for the user preset dir already lives app/renderer-side, and this
//! service needs `manifold_renderer::preset_loader`'s live catalog to check
//! id collisions — core has no renderer dependency.
//!
//! The struct is a thin `{ root: PathBuf }` (§4) deliberately — every
//! operation re-derives its target path from `root` + `kind` + `id` each
//! call, so there's no cached state to go stale against the hot-reload
//! watcher's own view of the directory.

use std::path::PathBuf;

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::preset_def::PresetKind;
use manifold_io::preset_file::PresetFileError;

/// Failure modes for a [`UserLibrary`] operation.
#[derive(Debug)]
pub enum LibError {
    /// The typed name was empty (after trim) — nothing to save/rename to.
    EmptyName,
    /// The def being saved carries no `preset_metadata` — a preset with no
    /// metadata isn't addressable by id, so it can't be minted a home.
    MissingMetadata,
    /// `rename` / `duplicate` / `delete` / `reveal` targeted an id with no
    /// file under the user library root. Never a factory/stock file — those
    /// live in a completely different directory (the resolved stock root),
    /// not `root`, so this is the structural guarantee that management
    /// operations can't reach a factory file even by accident.
    NotFound,
    /// Underlying file read/write/parse/serialize failure.
    PresetFile(PresetFileError),
}

impl std::fmt::Display for LibError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => write!(f, "preset name is empty"),
            Self::MissingMetadata => write!(f, "preset def carries no presetMetadata"),
            Self::NotFound => write!(f, "no user library file for this id"),
            Self::PresetFile(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LibError {}

impl From<PresetFileError> for LibError {
    fn from(e: PresetFileError) -> Self {
        Self::PresetFile(e)
    }
}

/// The user preset library: reads/writes standalone `.json` preset files
/// under `root`. `root` is the presets directory itself (parent of
/// `effects`/`generators`), matching
/// `manifold_renderer::preset_loader::resolve_user_root`'s base — [`Self::new`]
/// resolves the SAME path (it just doesn't require the directory to already
/// exist, since `save` creates it on demand; the read-only loader resolution
/// treats an absent directory as "no user presets").
pub struct UserLibrary {
    root: PathBuf,
}

impl UserLibrary {
    /// Resolve the real user library root:
    /// `~/Library/Application Support/MANIFOLD/presets`. Falls back to a
    /// relative `MANIFOLD-user-presets` dir if `$HOME` is unset (matches the
    /// (rare, dev-only) shape of `preset_loader::resolve_user_root`'s own
    /// failure mode — better than a panic on a save action).
    pub fn new() -> Self {
        let root = match std::env::var("HOME") {
            Ok(home) => PathBuf::from(home).join("Library/Application Support/MANIFOLD/presets"),
            Err(_) => PathBuf::from("MANIFOLD-user-presets"),
        };
        Self { root }
    }

    /// The sub-directory for `kind`, matching
    /// `preset_loader::KindDirs::bundle_subdir`.
    fn kind_dir(&self, kind: PresetKind) -> PathBuf {
        self.root.join(match kind {
            PresetKind::Effect => "effects",
            PresetKind::Generator => "generators",
        })
    }

    /// Path a library file for `id` would live at, for `kind`.
    fn path_for(&self, kind: PresetKind, id: &str) -> PathBuf {
        self.kind_dir(kind).join(format!("{}.json", id))
    }

    /// Path a library entry's save-time thumbnail PNG lives (or would live)
    /// at, for `kind` (PRESET_LIBRARY_DESIGN P6, D7) — same directory + stem
    /// as [`Self::path_for`]'s JSON, `.png` extension. Pure path computation
    /// (no device access; the caller renders + writes the bytes after a
    /// successful [`Self::save`]) so callers can check existence
    /// (`Path::is_file`) for the browser's clean-fallback rule without
    /// touching the filesystem here.
    pub fn thumbnail_path(&self, kind: PresetKind, id: &str) -> PathBuf {
        self.kind_dir(kind).join(format!("{}.png", id))
    }

    /// Whether `id` already names a preset for `kind`, checked two ways:
    /// (1) a file already on disk under THIS root (catches a just-saved file
    /// before the ~1s hot-reload watcher has re-scanned, and makes an
    /// injected test root self-consistent for disambiguation tests), and
    /// (2) the live merged catalog (stock + user + the current project's
    /// overlay) — the "never colliding with an existing stock or user id"
    /// rule (D4, §6.9: no save may silently create a new stem-override).
    fn id_taken(&self, kind: PresetKind, id: &str) -> bool {
        if self.path_for(kind, id).is_file() {
            return true;
        }
        match kind {
            PresetKind::Effect => {
                manifold_renderer::preset_loader::EFFECT_CATALOG.load().json(id).is_some()
            }
            PresetKind::Generator => {
                manifold_renderer::preset_loader::GENERATOR_CATALOG.load().json(id).is_some()
            }
        }
    }

    /// Mint a collision-free name for `kind`: `base` if free, else
    /// `"{base} 2"`, `"{base} 3"`, ... — same human-readable disambiguation
    /// style as `Project::mint_forked_preset_id`, checked against
    /// [`Self::id_taken`]'s combined disk+catalog domain.
    fn mint_name(&self, kind: PresetKind, base: &str) -> String {
        if !self.id_taken(kind, base) {
            return base.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base} {n}");
            if !self.id_taken(kind, &candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Save `def` as a new library entry named `name` (disambiguated on
    /// collision). Writes `<root>/{effects,generators}/<mintedName>.json` —
    /// the id and filename stem are the minted name itself (D2's
    /// display-based-id style), so the file is human-readable AND
    /// self-describing. Never overwrites an existing entry (that's `Push to
    /// Library`, a P4 action targeting an EXISTING entry's file, not this).
    pub fn save(
        &self,
        kind: PresetKind,
        name: &str,
        def: &EffectGraphDef,
    ) -> Result<PresetTypeId, LibError> {
        // "/" is the one byte a macOS filename can't contain; swapped for a
        // dash so a stray slash in a typed name can't escape `root` or split
        // across directories. Trim first so pure whitespace is caught by
        // the empty-name check below.
        let sanitized = name.trim().replace('/', "-");
        if sanitized.is_empty() {
            return Err(LibError::EmptyName);
        }

        let minted = self.mint_name(kind, &sanitized);
        let id = PresetTypeId::from_string(minted.clone());

        let mut def = def.clone();
        let meta = def.preset_metadata.as_mut().ok_or(LibError::MissingMetadata)?;
        meta.id = id.clone();
        meta.display_name = minted.clone();

        let dir = self.kind_dir(kind);
        std::fs::create_dir_all(&dir).map_err(PresetFileError::Io)?;
        manifold_io::preset_file::export_preset(&def, &self.path_for(kind, &minted))?;

        log::info!("[UserLibrary] saved {kind:?} preset '{minted}' to {}", dir.display());
        Ok(id)
    }

    /// Whether `id` names an entry with a file under THIS user root — the
    /// user-vs-factory test (`PRESET_LIBRARY_DESIGN` D3/P4): a factory/stock
    /// preset never lives under `root` (§4's structural guarantee, same one
    /// [`Self::delete`]/[`Self::rename`] rely on), so this is exactly "can
    /// [`Self::push`] overwrite this id, or does the caller need to fall
    /// back to Save-to-Library-as-new instead."
    pub fn is_user_entry(&self, kind: PresetKind, id: &PresetTypeId) -> bool {
        self.path_for(kind, id.as_str()).is_file()
    }

    /// Push to Library (PRESET_LIBRARY_DESIGN §4: "Push to Library =
    /// `UserLibrary::save` targeting the existing entry's file"): overwrite
    /// an EXISTING user-library entry's file in place with `def` — unlike
    /// [`Self::save`], this NEVER mints a new id/name; the id and filename
    /// stay exactly as they are, so every OTHER instance still tracking
    /// this id picks up the change via the existing hot-reload watcher.
    ///
    /// User entries only: returns `NotFound` if `id` has no file under
    /// `root` (a factory/stock id, or a typo) — the caller (the card menu /
    /// graph editor header) is expected to check [`Self::is_user_entry`]
    /// first and route a factory id to Save-to-Library-as-new instead of
    /// calling this; `NotFound` here is the same structural guarantee as
    /// `delete`'s (no path reaches a factory file), not a UI decision this
    /// method makes.
    pub fn push(
        &self,
        kind: PresetKind,
        id: &PresetTypeId,
        def: &EffectGraphDef,
    ) -> Result<(), LibError> {
        let path = self.path_for(kind, id.as_str());
        if !path.is_file() {
            return Err(LibError::NotFound);
        }
        // Stamp the id defensively so filename and `presetMetadata.id` can
        // never drift apart (matches `save`'s own stamp); display_name is
        // left as `def` carries it — Push overwrites the definition, not
        // the name (that's `rename`'s job).
        let mut def = def.clone();
        if let Some(meta) = def.preset_metadata.as_mut() {
            meta.id = id.clone();
        }
        manifold_io::preset_file::export_preset(&def, &path)?;
        log::info!("[UserLibrary] pushed {kind:?} preset '{}' to {}", id.as_str(), path.display());
        Ok(())
    }

    /// Rename an existing library entry's `display_name` in place. The id
    /// and filename stay — only `preset_metadata.display_name` changes, so
    /// every project that already references this id by name keeps
    /// resolving (D8: `effect_type`/id is the stable serialization anchor).
    ///
    /// Consumed by the browser management menu (right-click → Rename),
    /// via `TextInputField::RenamePreset`'s commit arm in `app.rs`.
    pub fn rename(&self, kind: PresetKind, id: &PresetTypeId, new_name: &str) -> Result<(), LibError> {
        let path = self.path_for(kind, id.as_str());
        if !path.is_file() {
            return Err(LibError::NotFound);
        }
        let mut def = manifold_io::preset_file::import_preset(&path)?;
        let meta = def.preset_metadata.as_mut().ok_or(LibError::MissingMetadata)?;
        meta.display_name = new_name.trim().to_string();
        manifold_io::preset_file::export_preset(&def, &path)?;
        Ok(())
    }

    /// Duplicate an existing library entry under a freshly minted,
    /// collision-free id/name (`"{name} 2"` style), leaving the original
    /// untouched. Returns the new entry's id.
    ///
    /// Consumed by the browser management menu (right-click → Duplicate),
    /// via `PanelAction::BrowserDuplicatePresetClicked`'s dispatch arm.
    pub fn duplicate(&self, kind: PresetKind, id: &PresetTypeId) -> Result<PresetTypeId, LibError> {
        let path = self.path_for(kind, id.as_str());
        if !path.is_file() {
            return Err(LibError::NotFound);
        }
        let def = manifold_io::preset_file::import_preset(&path)?;
        let base = def
            .preset_metadata
            .as_ref()
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| id.as_str().to_string());
        self.save(kind, &base, &def)
    }

    /// Delete a library entry's file. User entries ONLY — a factory/stock
    /// preset never lives under `root`, so there is no path by which this
    /// can reach one; `NotFound` is returned instead of touching anything
    /// outside the library root.
    ///
    /// Consumed by the browser management menu (right-click → Delete, gated
    /// by a native Yes/No confirm), via
    /// `PanelAction::BrowserDeletePresetClicked`'s dispatch arm.
    pub fn delete(&self, kind: PresetKind, id: &PresetTypeId) -> Result<(), LibError> {
        let path = self.path_for(kind, id.as_str());
        if !path.is_file() {
            return Err(LibError::NotFound);
        }
        std::fs::remove_file(&path).map_err(PresetFileError::Io)?;
        log::info!("[UserLibrary] deleted {kind:?} preset '{}'", id.as_str());
        Ok(())
    }

    /// Reveal a library entry's file in Finder (`open -R`). Best-effort —
    /// logs on failure rather than returning a `Result`, matching the
    /// fire-and-forget nature of a Finder reveal (nothing in the app state
    /// depends on whether Finder actually raised a window).
    ///
    /// Consumed by the browser management menu (right-click → Reveal in
    /// Finder), via `PanelAction::BrowserRevealPresetClicked`'s dispatch arm.
    pub fn reveal(&self, kind: PresetKind, id: &PresetTypeId) {
        let path = self.path_for(kind, id.as_str());
        if let Err(e) = std::process::Command::new("open").arg("-R").arg(&path).spawn() {
            log::error!("[UserLibrary] reveal failed for {}: {e}", path.display());
        }
    }
}

impl Default for UserLibrary {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::PresetMetadata;

    fn scratch_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "manifold-user-library-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ))
    }

    fn def_named(display_name: &str) -> EffectGraphDef {
        EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: Some(display_name.to_string()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::from_string("placeholder".to_string()),
                display_name: display_name.to_string(),
                category: "Test".to_string(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: Vec::new(),
                bindings: Vec::new(),
                skip_mode: Default::default(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
            }),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    /// (a) + (b): the minted id collides with no stock/user id (checked
    /// against the REAL dev-assets catalog, which always loads in tests —
    /// `Bloom` is a genuine stock effect), and the file lands in the right
    /// subdir with the right JSON shape (a bare `EffectGraphDef` with
    /// `presetMetadata`, matching what a bundled preset file looks like).
    #[test]
    fn save_avoids_stock_collision_and_writes_the_expected_shape() {
        let root = scratch_root("stock-collision");
        let lib = UserLibrary { root: root.clone() };

        let id = lib
            .save(PresetKind::Effect, "Bloom", &def_named("Bloom"))
            .expect("save succeeds");
        assert_eq!(
            id.as_str(),
            "Bloom 2",
            "must not silently create a stem-override of the stock `Bloom` id"
        );

        let path = root.join("effects").join("Bloom 2.json");
        assert!(path.is_file(), "file must land in the effects subdir");
        let on_disk = std::fs::read_to_string(&path).expect("read saved file");
        let parsed: EffectGraphDef = serde_json::from_str(&on_disk).expect("valid preset JSON");
        let meta = parsed.preset_metadata.expect("presetMetadata present");
        assert_eq!(meta.id.as_str(), "Bloom 2");
        assert_eq!(meta.display_name, "Bloom 2");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// (c): saving twice under the same typed name disambiguates the SECOND
    /// save to "Name 2" — this exercises the disk-side of `id_taken` (the
    /// injected temp root, not the real global catalog), since a from-
    /// scratch name has no stock/user counterpart to collide with.
    #[test]
    fn save_twice_with_same_name_disambiguates_second_save() {
        let root = scratch_root("self-collision");
        let lib = UserLibrary { root: root.clone() };

        let first = lib
            .save(PresetKind::Generator, "My Look", &def_named("My Look"))
            .expect("first save succeeds");
        assert_eq!(first.as_str(), "My Look");

        let second = lib
            .save(PresetKind::Generator, "My Look", &def_named("My Look"))
            .expect("second save succeeds");
        assert_eq!(second.as_str(), "My Look 2", "a name collision must disambiguate to 'Name 2'");

        assert!(root.join("generators").join("My Look.json").is_file());
        assert!(root.join("generators").join("My Look 2.json").is_file());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_rejects_empty_name() {
        let root = scratch_root("empty-name");
        let lib = UserLibrary { root: root.clone() };
        assert!(matches!(
            lib.save(PresetKind::Effect, "   ", &def_named("x")),
            Err(LibError::EmptyName)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_edits_display_name_but_keeps_id_and_filename() {
        let root = scratch_root("rename");
        let lib = UserLibrary { root: root.clone() };
        let id = lib.save(PresetKind::Effect, "Original", &def_named("Original")).unwrap();

        lib.rename(PresetKind::Effect, &id, "Renamed").expect("rename succeeds");

        let path = root.join("effects").join("Original.json");
        assert!(path.is_file(), "filename/id must stay Original");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        let parsed: EffectGraphDef = serde_json::from_str(&on_disk).unwrap();
        let meta = parsed.preset_metadata.unwrap();
        assert_eq!(meta.id.as_str(), "Original", "id must not change");
        assert_eq!(meta.display_name, "Renamed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_creates_a_second_disambiguated_file_leaving_original_intact() {
        let root = scratch_root("duplicate");
        let lib = UserLibrary { root: root.clone() };
        let id = lib.save(PresetKind::Generator, "Seed", &def_named("Seed")).unwrap();

        let dup_id = lib.duplicate(PresetKind::Generator, &id).expect("duplicate succeeds");
        assert_eq!(dup_id.as_str(), "Seed 2");
        assert!(root.join("generators").join("Seed.json").is_file(), "original must survive");
        assert!(root.join("generators").join("Seed 2.json").is_file());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_removes_the_file_and_reports_not_found_for_a_missing_id() {
        let root = scratch_root("delete");
        let lib = UserLibrary { root: root.clone() };
        let id = lib.save(PresetKind::Effect, "Gone Soon", &def_named("Gone Soon")).unwrap();
        assert!(root.join("effects").join("Gone Soon.json").is_file());

        lib.delete(PresetKind::Effect, &id).expect("delete succeeds");
        assert!(!root.join("effects").join("Gone Soon.json").exists());

        assert!(matches!(
            lib.delete(PresetKind::Effect, &id),
            Err(LibError::NotFound)
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── push (PRESET_LIBRARY_DESIGN P4) ─────────────────────────────────

    #[test]
    fn push_overwrites_an_existing_user_entry_in_place() {
        let root = scratch_root("push-overwrite");
        let lib = UserLibrary { root: root.clone() };
        let id = lib.save(PresetKind::Effect, "Glow", &def_named("Glow")).unwrap();
        assert!(lib.is_user_entry(PresetKind::Effect, &id), "just-saved entry must be a user entry");

        // A "diverged" def: same id, different name embedded on the def
        // itself (standing in for a real topology edit) — push must land
        // this content at the SAME path/id, not mint a new one.
        let mut edited = def_named("Glow (edited)");
        edited.preset_metadata.as_mut().unwrap().id = id.clone();
        lib.push(PresetKind::Effect, &id, &edited).expect("push succeeds for a user entry");

        let path = root.join("effects").join("Glow.json");
        assert!(path.is_file(), "push must not rename the file");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        let parsed: EffectGraphDef = serde_json::from_str(&on_disk).unwrap();
        let meta = parsed.preset_metadata.unwrap();
        assert_eq!(meta.id.as_str(), "Glow", "id must stay stable across a push");
        assert_eq!(meta.display_name, "Glow (edited)", "push must write the new definition");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn push_refuses_an_id_with_no_user_file() {
        let root = scratch_root("push-not-found");
        let lib = UserLibrary { root: root.clone() };
        let stock_like_id = PresetTypeId::from_string("Bloom".to_string());
        assert!(!lib.is_user_entry(PresetKind::Effect, &stock_like_id), "no file under this root");
        assert!(matches!(
            lib.push(PresetKind::Effect, &stock_like_id, &def_named("Bloom")),
            Err(LibError::NotFound)
        ), "a factory/never-saved id must refuse rather than write outside root");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_and_delete_never_touch_a_path_outside_root() {
        // There is no factory-file path by construction: every operation
        // targets `self.path_for(kind, id)`, always rooted under `self.root`
        // — a stock preset (a totally different directory) is simply never
        // addressable through this type. This test pins that a missing id
        // (the only way a caller could try to reach "outside") errors
        // rather than doing anything.
        let root = scratch_root("outside-root");
        let lib = UserLibrary { root: root.clone() };
        let stock_like_id = PresetTypeId::from_string("Bloom".to_string());
        assert!(matches!(
            lib.rename(PresetKind::Effect, &stock_like_id, "Hacked"),
            Err(LibError::NotFound)
        ));
        assert!(matches!(
            lib.delete(PresetKind::Effect, &stock_like_id),
            Err(LibError::NotFound)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}
