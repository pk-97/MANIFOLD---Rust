//! The asset-path inventory (PROJECT_FOLDERS_DESIGN.md D4).
//!
//! [`collect_asset_paths`] is THE enumeration of every external file a project
//! references — video library clips, audio clips, layer video folders, image
//! clips, and every string param whose `stringBinding` targets a file-loading
//! node type on any generator instance. Path Resolver extension (P2), Collect
//! All and Save (P4), and any future missing-file report all read it. No
//! second list anywhere.
//!
//! The string-param half is binding-driven (P5): a string param is collected
//! when its binding targets a node whose `type_id` is in the core
//! `file_loader_kind` table — independent of any `is_file_path` flag, so
//! presets embedded before the flag existed collect their GLBs too. The io
//! layer never names a param id and never names a node type id — both live in
//! `manifold_core::file_loader` (P5). Enumeration precedence mirrors the
//! runtime merge: a present per-clip override wins, else the def default.

use crate::path_resolver::PathResolver;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::effects::PresetInstance;
use manifold_core::file_loader::{file_loader_kind, AssetFamily, NodeFileLoad};
use manifold_core::id::{ClipId, LayerId};
use manifold_core::project::Project;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Which media family an asset belongs to — the `Media/` subfolder it collects
/// into (D2): `Media/Video`, `Media/Audio`, `Media/Meshes`, `Media/HDRIs`,
/// `Media/Images`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    Video,
    Audio,
    Mesh,
    Hdri,
    Images,
}

/// Where a collected asset lives in the project, used to re-point the stored
/// path to the collected (relative) form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetTarget {
    /// A `VideoClip` in `Project.video_library`, by clip id.
    VideoClip { clip_id: String },
    /// A layer's `video_folder_path`, by layer id.
    LayerVideoFolder { layer_id: LayerId },
    /// A `TimelineClip`'s `audio_file_path`, by layer + clip id.
    AudioClip { layer_id: LayerId, clip_id: ClipId },
    /// A `TimelineClip`'s `image_path` (a still-image clip), by layer + clip
    /// id.
    ImageClip { layer_id: LayerId, clip_id: ClipId },
    /// A string param on a generator layer, keyed by param id. The value lives
    /// per-clip (`TimelineClip.string_params`) with a fallback to the
    /// preset-def default.
    StringParam { layer_id: LayerId, key: String },
}

/// One external asset a project references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRef {
    pub kind: AssetKind,
    pub path: PathBuf,
    pub target: AssetTarget,
}

/// Enumerate every external asset path a project references.
///
/// Families (D4):
/// - video library clips (`file_path`)
/// - layer video folders (`video_folder_path`)
/// - audio source clips (`audio_file_path`)
/// - still-image clips (`image_path`)
/// - every string param whose `stringBinding` targets a file-loading node
///   type on any generator instance (the model/HDRI/image-folder paths a
///   preset loads) — binding-driven (P5): the binding's node type_id is looked
///   up in `manifold_core::file_loader`, independent of any `is_file_path`
///   flag. Resolution from the instance's own def first, then the project's
///   embedded preset, then the global definition registry (which already
///   reflects this project's embedded presets through the app's overlay).
///
/// Empty paths are skipped (an unset HDRI path is `""` and means "no envmap").
/// Paths are returned verbatim — existence is NOT checked: the same inventory
/// feeds the missing-file report, so a broken path must still be enumerated.
pub fn collect_asset_paths(project: &Project) -> Vec<AssetRef> {
    let mut out = Vec::new();

    for clip in &project.video_library.clips {
        if clip.file_path.is_empty() {
            continue;
        }
        out.push(AssetRef {
            kind: AssetKind::Video,
            path: PathBuf::from(&clip.file_path),
            target: AssetTarget::VideoClip {
                clip_id: clip.id.clone(),
            },
        });
    }

    for layer in &project.timeline.layers {
        if let Some(folder) = &layer.video_folder_path
            && !folder.is_empty()
        {
            out.push(AssetRef {
                kind: AssetKind::Video,
                path: PathBuf::from(folder),
                target: AssetTarget::LayerVideoFolder {
                    layer_id: layer.layer_id.clone(),
                },
            });
        }
    }

    for layer in &project.timeline.layers {
        for clip in &layer.clips {
            if !clip.audio_file_path.is_empty() {
                out.push(AssetRef {
                    kind: AssetKind::Audio,
                    path: PathBuf::from(&clip.audio_file_path),
                    target: AssetTarget::AudioClip {
                        layer_id: layer.layer_id.clone(),
                        clip_id: clip.id.clone(),
                    },
                });
            }
            // Still-image clips (BUG-2jbn).
            if !clip.image_path.is_empty() {
                out.push(AssetRef {
                    kind: AssetKind::Images,
                    path: PathBuf::from(&clip.image_path),
                    target: AssetTarget::ImageClip {
                        layer_id: layer.layer_id.clone(),
                        clip_id: clip.id.clone(),
                    },
                });
            }
        }
    }

    // String params on every gen-carrying layer (D16 predicate — Generator
    // and Dmx lanes both host generators; BUG-bbg5). Effects gain string
    // params as the capability gap closes — the same walk extends to them
    // unchanged.
    for layer in &project.timeline.layers {
        if !layer.hosts_generator() {
            continue;
        }
        let Some(inst) = layer.gen_params() else {
            continue;
        };
        let defs = resolve_string_defs(project, inst);
        for (key, default, load) in &defs {
            // Value precedence mirrors the runtime merge (`generator_renderer`):
            // a present per-clip override wins ANY other clip's (and the def
            // default), even when empty — an explicit `""` clears the param.
            // Each clip on the layer may carry a different override, so every
            // distinct effective value on the layer gets a ref.
            let values: Vec<&String> = layer
                .clips
                .iter()
                .filter_map(|c| c.string_params.as_ref().and_then(|m| m.get(key)))
                .collect();
            let values = if values.is_empty() {
                vec![default]
            } else {
                values
            };
            for value in values {
                if value.is_empty() {
                    continue;
                }
                out.push(AssetRef {
                    kind: kind_of(*load),
                    path: PathBuf::from(value),
                    target: AssetTarget::StringParam {
                        layer_id: layer.layer_id.clone(),
                        key: key.clone(),
                    },
                });
            }
        }
    }

    // Dedupe exact triples — N clips carrying the identical override would
    // otherwise enumerate the same (kind, path, target) N times.
    out.dedup();
    out
}

/// The file-loading string-param defs for a generator instance, as owned
/// `(key, default_value, load)` triples where `load` names what the bound node
/// reads (file vs folder + family). Resolution order (same as the runtime
/// load): the instance's own graph metadata first, then the project's embedded
/// preset by tracked id, then the global definition registry.
///
/// Enumeration is binding-driven (P5): a def is included iff its `stringBinding`
/// targets a node whose type_id has a `file_loader_kind` entry — the
/// `is_file_path` flag is NOT consulted. To walk a binding we need the graph
/// that carries the binding (`bindings` live in the same `PresetMetadata` as
/// the `string_params`), so the registry fallback (which only carries defs by
/// key, not their targets) cannot resolve bindings: static registry presets
/// carry `is_file_path` as their author's declaration, which is the best
/// binding signal available there — still independent of the io layer naming
/// any id.
fn resolve_string_defs(project: &Project, inst: &PresetInstance) -> Vec<(String, String, NodeFileLoad)> {
    // 1. The instance's own graph metadata carries the full `StringParamSpecDef`
    //    list when it has diverged (or was just imported as an embedded graph).
    if let Some(graph) = inst_graph(inst)
        && graph.preset_metadata.is_some()
    {
        return defs_from_meta(graph);
    }
    // 2. An embedded preset by the tracked id is self-contained (graph +
    //    metadata) — the import case, where the layer tracks by id (graph: None).
    let id = inst.generator_type();
    if let Some(embedded) = project.embedded_preset(id) {
        return defs_from_meta(&embedded.def);
    }
    // 3. Stock/user catalog preset — the global registry. (The app's overlay
    //    installs this project's embedded presets into it at load, so 2 would
    //    usually have caught them; the registry is the fallback regardless.)
    manifold_core::preset_definition_registry::try_get(id).map_or_else(Vec::new, |def| {
        def.string_param_defs
            .iter()
            .filter(|sp| sp.is_file_path)
            .map(|sp| {
                // No graph to walk bindings against here — the extension hint
                // is the only family signal (pre-P5 behavior for this fallback).
                let family = match sp.default_value.rsplit('.').next() {
                    Some(ext) if ext.eq_ignore_ascii_case("exr") || ext.eq_ignore_ascii_case("hdr") => {
                        AssetFamily::Hdri
                    }
                    _ => AssetFamily::Mesh,
                };
                (
                    sp.key.to_string(),
                    sp.default_value.to_string(),
                    NodeFileLoad::File(family),
                )
            })
            .collect()
    })
}

/// The `StringParamSpecDef`s of a graph that carry a file-loading binding.
/// Each def is paired with the `NodeFileLoad` its binding's target node reads,
/// resolved through the graph's node table. Defs whose bindings are all absent,
/// non-node, or bound to non-file-loading nodes are skipped.
///
/// Two real-project shapes drive the lookup (both from grouped 3D-scene
/// presets, BUG-gqne follow-up): one param id fans out to MANY bindings (a
/// `model_file` feeding six mesh/texture nodes) — the def collects if ANY of
/// its bindings targets a file loader; and binding targets sit INSIDE `group`
/// node bodies — the node search recurses through group subgraphs.
fn defs_from_meta(graph: &EffectGraphDef) -> Vec<(String, String, NodeFileLoad)> {
    let Some(meta) = graph.preset_metadata.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for sp in &meta.string_params {
        let load = meta
            .string_bindings
            .iter()
            .filter(|b| b.id == sp.id)
            .find_map(|b| match &b.target {
                BindingTarget::Node { node_id, .. } => {
                    find_node_type(&graph.nodes, node_id).and_then(file_loader_kind)
                }
                _ => None,
            });
        if let Some(load) = load {
            out.push((sp.id.clone(), sp.default_value.clone(), load));
        }
    }
    out
}

/// Depth-first search for a node's `type_id` by `node_id`, descending into
/// group bodies (a group node's `group` field holds a nested node list, which
/// may itself contain groups).
fn find_node_type<'a>(
    nodes: &'a [manifold_core::effect_graph_def::EffectGraphNode],
    id: &manifold_core::id::NodeId,
) -> Option<&'a str> {
    for n in nodes {
        if &n.node_id == id {
            return Some(n.type_id.as_str());
        }
        if let Some(group) = &n.group
            && let Some(found) = find_node_type(&group.nodes, id)
        {
            return Some(found);
        }
    }
    None
}

/// The full `EffectGraphDef` carried directly on a `PresetInstance`'s own
/// graph.
fn inst_graph(inst: &PresetInstance) -> Option<&EffectGraphDef> {
    inst.graph.as_ref()
}

/// The `AssetKind` an enumerated `NodeFileLoad` collects into. The table
/// currently has no Folder(Mesh)/Folder(Hdri) entries; map them defensively to
/// the same family as their File sibling so a future `Folder(Mesh)` entry
/// (e.g. a model-drop folder) collects without a red-match refactor — the
/// family, not the file-vs-folder split, decides the `Media/` subfolder.
fn kind_of(load: NodeFileLoad) -> AssetKind {
    match load {
        NodeFileLoad::File(AssetFamily::Mesh) | NodeFileLoad::Folder(AssetFamily::Mesh) => {
            AssetKind::Mesh
        }
        NodeFileLoad::File(AssetFamily::Hdri) | NodeFileLoad::Folder(AssetFamily::Hdri) => {
            AssetKind::Hdri
        }
        NodeFileLoad::File(AssetFamily::Images) | NodeFileLoad::Folder(AssetFamily::Images) => {
            AssetKind::Images
        }
    }
}

// ── Collect All and Save (D6) ──────────────────────────────────────

/// What one Collect All and Save pass did (PROJECT_FOLDERS_DESIGN.md D6).
/// `copied` counts unique files physically written (identical content deduped
/// by full SHA-256), `already_local` counts refs already inside the project
/// folder, `re_pointed` counts refs whose stored path was rewritten to the
/// in-folder form.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CollectReport {
    pub copied: usize,
    pub already_local: usize,
    pub bytes_copied: u64,
    pub missing: usize,
    pub re_pointed: usize,
}

#[derive(Debug)]
pub enum CollectError {
    /// The project path has no parent directory — media must collect into the
    /// folder the `.manifold` file lives in (D1/D2).
    NoProjectDir,
    Io(String),
    Save(crate::saver::SaveError),
}

impl std::fmt::Display for CollectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectError::NoProjectDir => write!(f, "project path has no parent directory"),
            CollectError::Io(e) => write!(f, "IO error: {e}"),
            CollectError::Save(e) => write!(f, "save error: {e}"),
        }
    }
}

impl std::error::Error for CollectError {}

/// Collect All and Save (D6): copy every external asset into the project
/// folder's `Media/` family subfolders — copy-only, never moving or deleting a
/// source — dedup identical sources by full SHA-256, re-point the stored path
/// to the in-folder file, then run the normal save path.
///
/// The single enumeration is [`collect_asset_paths`] (D4) — no second list.
/// A flagged string param whose value lives only in the preset-def default is
/// materialized as a per-clip override (D5a); the def's `default_value` is
/// never written.
pub fn collect_all_and_save(
    project: &mut Project,
    project_path: &Path,
) -> Result<CollectReport, CollectError> {
    // Raw `project_path.parent()`, NOT canonicalized: the save path
    // (`saver::save_project` → `store_relative_paths`) derives its base from
    // the same raw parent, so a canonicalized dir here would make
    // `make_relative` compute a wrong `..`-laden sibling for the file paths we
    // just wrote. `path_is_inside` canonicalizes both sides itself.
    let project_dir = project_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .ok_or(CollectError::NoProjectDir)?;

    let refs = collect_asset_paths(project);
    let mut report = CollectReport::default();
    // Dedup identical file content by (family, full SHA-256) → the target path
    // the first copy landed at. Later refs with the same content re-point to
    // the same file instead of copying it again.
    let mut copied_files: HashMap<(AssetKind, [u8; 32]), PathBuf> = HashMap::new();
    // Directories (layer video folders) have no content-hash dedup; dedup by
    // canonical source path so two layers sharing one folder copy it once.
    let mut copied_dirs: HashMap<PathBuf, PathBuf> = HashMap::new();

    for r in &refs {
        let src = &r.path;

        // Layer video folder: a directory of footage, copied as a tree.
        if src.is_dir() {
            if path_is_inside(src, &project_dir) {
                report.already_local += 1;
                continue;
            }
            let canonical = std::fs::canonicalize(src).unwrap_or_else(|_| src.clone());
            let target = if let Some(existing) = copied_dirs.get(&canonical) {
                existing.clone()
            } else {
                let name = src
                    .file_name()
                    .map(|n| n.to_os_string())
                    .unwrap_or_else(|| "folder".into());
                let target = media_family_dir(&project_dir, r.kind).join(name);
                let mut bytes = 0u64;
                copy_dir_recursive(src, &target, &mut bytes)
                    .map_err(|e| CollectError::Io(format!("copy {}: {e}", src.display())))?;
                report.copied += 1;
                report.bytes_copied += bytes;
                copied_dirs.insert(canonical, target.clone());
                target
            };
            re_point(project, &r.target, src, &target, &project_dir, &mut report);
            continue;
        }

        if !src.is_file() {
            report.missing += 1;
            continue;
        }

        if path_is_inside(src, &project_dir) {
            report.already_local += 1;
            continue;
        }

        let hash = sha256_file(src)?;
        let target = if let Some(existing) = copied_files.get(&(r.kind, hash)) {
            existing.clone()
        } else {
            let family_dir = media_family_dir(&project_dir, r.kind);
            std::fs::create_dir_all(&family_dir)
                .map_err(|e| CollectError::Io(format!("create {}: {e}", family_dir.display())))?;
            let name = src
                .file_name()
                .map(|n| n.to_os_string())
                .unwrap_or_else(|| "asset".into());
            let target = resolve_target_path(&family_dir, &name, hash)?;
            std::fs::copy(src, &target)
                .map_err(|e| CollectError::Io(format!("copy {}: {e}", src.display())))?;
            report.copied += 1;
            report.bytes_copied += std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
            copied_files.insert((r.kind, hash), target.clone());
            target
        };

        re_point(project, &r.target, src, &target, &project_dir, &mut report);
    }

    crate::saver::save_project(project, project_path, None, false).map_err(CollectError::Save)?;
    Ok(report)
}

/// The `Media/<family>` subfolder a `kind` collects into (D2).
fn media_family_dir(project_dir: &Path, kind: AssetKind) -> PathBuf {
    let sub = match kind {
        AssetKind::Video => "Video",
        AssetKind::Audio => "Audio",
        AssetKind::Mesh => "Meshes",
        AssetKind::Hdri => "HDRIs",
        AssetKind::Images => "Images",
    };
    project_dir.join("Media").join(sub)
}

/// True when `path` is inside `dir`. Both are resolved to absolute where
/// possible; a missing `path` compares lexically, which may under-report but
/// never wrongly treats an in-folder path as external.
fn path_is_inside(path: &Path, dir: &Path) -> bool {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let d = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    p.starts_with(d)
}

/// Full SHA-256 of a file's bytes, streamed (no whole-file read).
fn sha256_file(path: &Path) -> Result<[u8; 32], CollectError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| CollectError::Io(format!("open {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| CollectError::Io(format!("hash {}: {e}", path.display())))?;
    Ok(hasher.finalize().into())
}

/// Pick the destination for `name` inside `dir`: the plain name, unless a file
/// already occupies it with DIFFERENT content — then a `_1`, `_2`, … suffix is
/// appended before the extension. Same-content is reused (a prior collect left
/// it); different content is never overwritten.
fn resolve_target_path(dir: &Path, name: &std::ffi::OsStr, new_hash: [u8; 32]) -> Result<PathBuf, CollectError> {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return Ok(candidate);
    }
    if sha256_file(&candidate).unwrap_or([0u8; 32]) == new_hash {
        return Ok(candidate);
    }
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("asset");
    let ext = Path::new(name).extension().and_then(|s| s.to_str());
    for i in 1u32.. {
        let numbered = match ext {
            Some(e) => format!("{stem}_{i}.{e}"),
            None => format!("{stem}_{i}"),
        };
        let p = dir.join(&numbered);
        if !p.exists() {
            return Ok(p);
        }
        if sha256_file(&p).unwrap_or([0u8; 32]) == new_hash {
            return Ok(p);
        }
    }
    Err(CollectError::Io(format!(
        "no free name for {} in {}",
        Path::new(name).display(),
        dir.display()
    )))
}

/// Copy a directory tree (`src` → `dst`), adding bytes written to `bytes`.
/// Copy-only: reads `src`, writes `dst`, never touches `src`'s contents.
fn copy_dir_recursive(src: &Path, dst: &Path, bytes: &mut u64) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to, bytes)?;
        } else if ty.is_file() {
            *bytes += std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Re-point one asset ref's stored path to `new_path` (an in-folder file or
/// directory), filling the relative sibling where one exists. Counts into
/// `report.re_pointed` only when a field actually changed.
fn re_point(
    project: &mut Project,
    target: &AssetTarget,
    old_path: &Path,
    new_path: &Path,
    project_dir: &Path,
    report: &mut CollectReport,
) {
    let new_str = new_path.to_string_lossy().to_string();
    let relative = PathResolver::make_relative(&new_str, &project_dir.to_string_lossy());
    let old_str = old_path.to_string_lossy().to_string();

    let changed = match target {
        AssetTarget::VideoClip { clip_id } => {
            if let Some(clip) = project.video_library.clips.iter_mut().find(|c| &c.id == clip_id) {
                clip.file_path = new_str.clone();
                clip.relative_file_path = relative;
                true
            } else {
                false
            }
        }
        AssetTarget::LayerVideoFolder { layer_id } => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str()) {
                layer.video_folder_path = Some(new_str.clone());
                layer.relative_video_folder_path = relative;
                true
            } else {
                false
            }
        }
        AssetTarget::AudioClip { layer_id, clip_id } => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str())
                && let Some(clip) = layer.clips.iter_mut().find(|c| &c.id == clip_id)
            {
                clip.audio_file_path = new_str.clone();
                clip.relative_audio_file_path = relative;
                true
            } else {
                false
            }
        }
        AssetTarget::ImageClip { layer_id, clip_id } => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str())
                && let Some(clip) = layer.clips.iter_mut().find(|c| &c.id == clip_id)
            {
                clip.image_path = new_str.clone();
                clip.relative_image_path = relative;
                true
            } else {
                false
            }
        }
        AssetTarget::StringParam { layer_id, key } => {
            re_point_string_param(project, layer_id, key, &old_str, &new_str)
        }
    };

    if changed {
        report.re_pointed += 1;
    }
}

/// Re-point a file-loading string param (D5a). Clips whose effective value equals
/// `old` are rewritten to `new`: an existing per-clip override is updated in
/// place; a value that came only from the preset-def default is materialized
/// as a per-clip override. The def's `default_value` is never touched.
fn re_point_string_param(
    project: &mut Project,
    layer_id: &LayerId,
    key: &str,
    old: &str,
    new: &str,
) -> bool {
    // Resolve the def default via the same chain collect_asset_paths uses, so
    // "is this the def-default value" is answered by the same source that
    // enumerated it. Owned so the immutable borrow ends before the mutation.
    let def_default: Option<String> = {
        let Some((_, layer)) = project.timeline.find_layer_by_id(layer_id.as_str()) else {
            return false;
        };
        let Some(inst) = layer.gen_params() else {
            return false;
        };
        resolve_string_defs(project, inst)
            .into_iter()
            .find(|(k, _, _)| k == key)
            .map(|(_, default, _)| default)
    };

    let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str()) else {
        return false;
    };
    let mut changed = false;
    for clip in &mut layer.clips {
        match clip.string_params.as_ref().and_then(|m| m.get(key)) {
            Some(ov) if ov == old => {
                if let Some(m) = clip.string_params.as_mut() {
                    m.insert(key.to_string(), new.to_string());
                    changed = true;
                }
            }
            Some(_) => {
                // A different explicit override — its own ref re-points it.
            }
            None => {
                if def_default.as_deref() == Some(old) {
                    let m = clip
                        .string_params
                        .get_or_insert_with(std::collections::BTreeMap::new);
                    m.insert(key.to_string(), new.to_string());
                    changed = true;
                }
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::clip::TimelineClip;
    use manifold_core::effect_graph_def::{
        BindingTarget, EffectGraphDef, EffectGraphNode, PresetMetadata, StringBindingDef,
        StringParamSpecDef,
    };
    use manifold_core::id::NodeId;
    use manifold_core::layer::Layer;
    use manifold_core::preset_type_id::PresetTypeId;
    use manifold_core::types::LayerType;
    use manifold_core::video::VideoClip;

    fn sp(id: &str, default: &str, file_path: bool) -> StringParamSpecDef {
        StringParamSpecDef {
            id: id.to_string(),
            name: id.to_string(),
            default_value: default.to_string(),
            is_file_picker: file_path,
            use_dropdown: false,
            is_file_path: file_path,
        }
    }

    fn node(node_id: &str, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id: 0,
            node_id: NodeId::new(node_id),
            type_id: type_id.to_string(),
            handle: Some(node_id.to_string()),
            params: Default::default(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        }
    }

    fn path_preset(
        name: &str,
        sps: Vec<StringParamSpecDef>,
        binds: Vec<StringBindingDef>,
        nodes: Vec<EffectGraphNode>,
    ) -> manifold_core::project::EmbeddedPreset {
        let meta = PresetMetadata {
            id: PresetTypeId::from_string(name.to_string()),
            display_name: name.to_string(),
            category: "Geometry".to_string(),
            osc_prefix: name.to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
                layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: sps,
            string_bindings: binds,
            scene_bounds: None,
        };
        manifold_core::project::EmbeddedPreset {
            kind: manifold_core::preset_def::PresetKind::Generator,
            def: EffectGraphDef {
                version: 1,
                name: Some(name.to_string()),
                description: None,
                preset_metadata: Some(meta),
                nodes,
                wires: Vec::new(),
            },
            origin: manifold_core::project::EmbeddedOrigin::Saved,
        }
    }

    /// Build a project with one generator layer tracking an embedded GLB+HDRI
    /// import (the model path baked as the def default, HDRI empty), one video
    /// clip, one audio clip, and one layer video folder.
    fn project_with_all_families() -> (Project, LayerId, LayerId) {
        let mut project = Project::default();

        // Video library clip.
        let vc = VideoClip {
            id: "vc1".to_string(),
            file_path: "/mnt/video/clip1.mp4".to_string(),
            relative_file_path: None,
            file_name: String::new(),
            duration: 0.0,
            resolution_width: 0,
            resolution_height: 0,
            file_size: 0,
            last_modified_ticks: 0,
        };
        project.video_library.add_clip(vc);

        // Layer video folder on a video layer.
        let mut folder_layer = Layer::new("FolderLayer".into(), LayerType::Video, 0);
        folder_layer.video_folder_path = Some("/mnt/footage".to_string());
        let folder_layer_id = folder_layer.layer_id.clone();
        project.timeline.layers.push(folder_layer);

        // Audio layer with one clip.
        let mut audio_layer = Layer::new("Audio".into(), LayerType::Audio, 1);
        let mut audio_clip = TimelineClip::new_audio(
            String::new(),
            manifold_core::Beats::ZERO,
            manifold_core::Beats::from_f32(4.0),
            manifold_core::Seconds::ZERO,
            manifold_core::Seconds::ZERO,
        );
        audio_clip.audio_file_path = "/mnt/audio/loop1.wav".to_string();
        audio_layer.clips.push(audio_clip);
        let audio_layer_id = audio_layer.layer_id.clone();
        project.timeline.layers.push(audio_layer);

        // Generator layer tracking an embedded GLB + HDRI import. Fixture keys are
        // synthetic on purpose — the enumeration must not know the real ids.
        let mesh_bind = StringBindingDef {
            id: "mesh_path".to_string(),
            label: "Model File".to_string(),
            default_value: "/mnt/models/azalea.glb".to_string(),
            target: BindingTarget::Node {
                node_id: NodeId::new("mesh"),
                param: "path".to_string(),
            },
        };
        let hdri_bind = StringBindingDef {
            id: "env_path".to_string(),
            label: "HDRI File".to_string(),
            default_value: String::new(),
            target: BindingTarget::Node {
                node_id: NodeId::new("hdri"),
                param: "path".to_string(),
            },
        };
        let embedded = path_preset(
            "azalea",
            vec![
                // NO `is_file_path` flag — a pre-flag embedded preset (V5):
                // collection must follow the binding regardless (BUG-gqne).
                sp("mesh_path", "/mnt/models/azalea.glb", false),
                sp("env_path", "", false),
            ],
            vec![mesh_bind, hdri_bind],
            vec![
                node("mesh", "node.gltf_mesh_source"),
                node("hdri", "node.hdri_source"),
            ],
        );
        project.upsert_embedded_preset(embedded);
        let preset_id = PresetTypeId::new("azalea");
        let mut gen_layer = Layer::new_generator("Azalea".into(), preset_id, 2);
        // A clip on the generator layer so the layer renders.
        gen_layer
            .clips
            .push(TimelineClip::new_generator(manifold_core::Beats::ZERO, manifold_core::Beats::from_f32(16.0)));
        project.timeline.layers.push(gen_layer);

        (project, folder_layer_id, audio_layer_id)
    }

    #[test]
    fn video_and_audio_and_folder_families_are_collected() {
        let (project, folder_layer_id, _audio_layer_id) = project_with_all_families();
        let refs = collect_asset_paths(&project);

        // Video clip.
        assert!(refs.iter().any(|r| {
            r.kind == AssetKind::Video
                && r.path == std::path::Path::new("/mnt/video/clip1.mp4")
                && r.target == AssetTarget::VideoClip { clip_id: "vc1".to_string() }
        }));
        // Layer video folder.
        assert!(refs.iter().any(|r| {
            r.kind == AssetKind::Video
                && r.path == std::path::Path::new("/mnt/footage")
                && r.target == AssetTarget::LayerVideoFolder { layer_id: folder_layer_id.clone() }
        }));
        // Audio clip.
        assert!(refs.iter().any(|r| {
            r.kind == AssetKind::Audio && r.path == std::path::Path::new("/mnt/audio/loop1.wav")
        }));
    }

    /// V5 regression (BUG-gqne): a string param with a `stringBinding` to
    /// `node.gltf_mesh_source` is collected as Mesh even though the
    /// `is_file_path` flag was never set — the class of embedded preset that
    /// silently skipped its GLB before P5.
    #[test]
    fn binding_driven_string_params_collect_without_file_path_flag() {
        let (project, _folder_layer_id, _audio_layer_id) = project_with_all_families();
        let refs = collect_asset_paths(&project);

        // mesh_path → Mesh, from the embedded preset's def default, with NO
        // flag on the def (the fixture sets is_file_path: false).
        assert!(refs.iter().any(|r| {
            r.kind == AssetKind::Mesh
                && r.path == std::path::Path::new("/mnt/models/azalea.glb")
                && matches!(&r.target, AssetTarget::StringParam { key, .. } if key == "mesh_path")
        }));
        // env_path is empty in the def and has no per-clip override → skipped.
        assert!(!refs.iter().any(|r| {
            matches!(&r.target, AssetTarget::StringParam { key, .. } if key == "env_path")
        }));
    }

    /// V5 live-fire follow-up (BUG-gqne): the real V5 embedded presets nest
    /// their mesh/texture nodes INSIDE `group` bodies and fan one param id out
    /// to several bindings. A flat-graph test passed while the real project
    /// collected nothing — this test pins both shapes.
    #[test]
    fn bindings_resolve_through_groups_and_fanout() {
        let mut project = Project::default();
        let bind = |node_id: &str| StringBindingDef {
            id: "model_file".to_string(),
            label: "Model File".to_string(),
            default_value: "/mnt/models/periwinkle.glb".to_string(),
            target: BindingTarget::Node {
                node_id: NodeId::new(node_id),
                param: "path".to_string(),
            },
        };
        let mut group_node = node("obj0", "group");
        group_node.group = Some(Box::new(manifold_core::effect_graph_def::GroupDef {
            interface: manifold_core::effect_graph_def::GroupInterface {
                inputs: Vec::new(),
                outputs: Vec::new(),
                params: Vec::new(),
            },
            nodes: vec![
                node("mesh_in_group", "node.gltf_mesh_source"),
                node("tex_in_group", "node.gltf_texture_source"),
            ],
            wires: Vec::new(),
            tint: None,
        }));
        let embedded = path_preset(
            "periwinkle",
            // NO `is_file_path` flag — the pre-flag embedded-preset shape.
            vec![sp("model_file", "/mnt/models/periwinkle.glb", false)],
            // One param id, two bindings — both nested inside the group.
            vec![bind("mesh_in_group"), bind("tex_in_group")],
            vec![group_node],
        );
        project.upsert_embedded_preset(embedded);
        let preset_id = PresetTypeId::new("periwinkle");
        let mut gen_layer = Layer::new_generator("Periwinkle".into(), preset_id, 0);
        gen_layer
            .clips
            .push(TimelineClip::new_generator(manifold_core::Beats::ZERO, manifold_core::Beats::from_f32(16.0)));
        project.timeline.layers.push(gen_layer);

        let refs = collect_asset_paths(&project);
        assert!(
            refs.iter().any(|r| {
                r.kind == AssetKind::Mesh
                    && r.path == std::path::Path::new("/mnt/models/periwinkle.glb")
                    && matches!(&r.target, AssetTarget::StringParam { key, .. } if key == "model_file")
            }),
            "grouped + fanned-out binding must collect; got {refs:?}"
        );
    }

    #[test]
    fn clip_override_wins_over_def_default() {
        let (mut project, _folder_layer_id, _audio_layer_id) = project_with_all_families();
        // Put a per-clip override on the generator layer's clip: swapping the
        // model path should change the collected ref (def default is shadowed).
        let gen_layer_idx = project
            .timeline
            .layers
            .iter()
            .position(|l| l.layer_type == LayerType::Generator)
            .unwrap();
        let clip = &mut project.timeline.layers[gen_layer_idx].clips[0];
        let mut m = std::collections::BTreeMap::new();
        m.insert("mesh_path".to_string(), "/mnt/models/swap.glb".to_string());
        clip.string_params = Some(m);

        let refs = collect_asset_paths(&project);
        assert!(refs.iter().any(|r| {
            r.kind == AssetKind::Mesh && r.path == std::path::Path::new("/mnt/models/swap.glb")
        }));
        assert!(!refs.iter().any(|r| r.path == std::path::Path::new("/mnt/models/azalea.glb")));
    }

    #[test]
    fn unset_string_params_are_skipped() {
        let mut project = Project::default();
        let embedded = path_preset(
            "noop",
            vec![
                sp("mesh_path", "/mnt/models/empty.glb", true),
                sp("text", "HELLO", false),
            ],
            vec![
                // `text` has no binding → never enumerated.
                StringBindingDef {
                    id: "text".to_string(),
                    label: "Text".to_string(),
                    default_value: "HELLO".to_string(),
                    target: BindingTarget::Node {
                        node_id: NodeId::new("mesh"),
                        param: "path".to_string(),
                    },
                },
            ],
            vec![node("mesh", "node.gltf_mesh_source")],
        );
        project.upsert_embedded_preset(embedded);
        let mut layer = Layer::new_generator("Noop".into(), PresetTypeId::new("noop"), 0);
        // Per-clip override EMPTIES the model path — the merge semantics treat
        // `.filter(!is_empty)` as "unset", so the ref must disappear.
        let mut m = std::collections::BTreeMap::new();
        m.insert("mesh_path".to_string(), String::new());
        layer.clips.push(TimelineClip::new_generator(
            manifold_core::Beats::ZERO,
            manifold_core::Beats::from_f32(4.0),
        ));
        let clip = &mut layer.clips[0];
        clip.string_params = Some(m);
        project.timeline.layers.push(layer);

        let refs = collect_asset_paths(&project);
        assert!(!refs.iter().any(|r| {
            matches!(&r.target, AssetTarget::StringParam { key, .. } if key == "mesh_path")
        }));
    }

    /// BUG-bbg5 pin (LED_STRIPS_DESIGN.md section 5b D16): the string-param
    /// walk gates on `hosts_generator()` (Generator + Dmx), not
    /// `== Generator`. A Dmx lane hosts a generator and its file-path string
    /// params must collect — pre-widening the walk silently skipped them and
    /// the export dropped the referenced assets.
    #[test]
    fn dmx_layer_string_params_collect() {
        let mut project = Project::default();
        let embedded = path_preset(
            "dmx_mesh",
            vec![sp("mesh_path", "/mnt/models/led_mesh.glb", true)],
            vec![StringBindingDef {
                id: "mesh_path".to_string(),
                label: "Model File".to_string(),
                default_value: "/mnt/models/led_mesh.glb".to_string(),
                target: BindingTarget::Node {
                    node_id: NodeId::new("mesh"),
                    param: "path".to_string(),
                },
            }],
            vec![node("mesh", "node.gltf_mesh_source")],
        );
        project.upsert_embedded_preset(embedded);
        // Same construction as AddLayerCommand's Dmx arm: build as a
        // generator layer (seeds gen_params), then flip the type — gen_params
        // has no public setter.
        let mut layer = Layer::new_generator("DMX 1".into(), PresetTypeId::new("dmx_mesh"), 0);
        layer.layer_type = LayerType::Dmx;
        project.timeline.layers.push(layer);

        let refs = collect_asset_paths(&project);
        assert!(
            refs.iter().any(|r| {
                r.kind == AssetKind::Mesh
                    && r.path == std::path::Path::new("/mnt/models/led_mesh.glb")
                    && matches!(&r.target, AssetTarget::StringParam { key, .. } if key == "mesh_path")
            }),
            "DMX lane string params must collect (hosts_generator gate)"
        );
    }

    /// D5 chain: the JSON `"file_path": true` marker lands on
    /// `StringParamSpecDef` and survives `preset_metadata_to_def` into the
    /// registry `StringParamDef`.
    #[test]
    fn file_path_marker_round_trips_through_metadata() {
        let sp = StringParamSpecDef {
            id: "mesh_path".to_string(),
            name: "Model File".to_string(),
            default_value: "/tmp/foo.glb".to_string(),
            is_file_picker: true,
            use_dropdown: false,
            is_file_path: true,
        };
        // Serialize emits the literal snake_case `"file_path": true`.
        let json = serde_json::to_string(&sp).unwrap();
        assert!(json.contains("\"file_path\":true"), "marker not on wire: {json}");
        // And round-trips back.
        let back: StringParamSpecDef = serde_json::from_str(&json).unwrap();
        assert!(back.is_file_path);

        // `false` (the default) is skipped on serialize — unflagged presets
        // stay byte-identical.
        let unflagged = StringParamSpecDef {
            id: "text".to_string(),
            name: "Text".to_string(),
            default_value: "HELLO".to_string(),
            is_file_picker: false,
            use_dropdown: false,
            is_file_path: false,
        };
        let unflagged_json = serde_json::to_string(&unflagged).unwrap();
        assert!(!unflagged_json.contains("file_path"), "{unflagged_json}");
    }

    // ── P4 gate (PROJECT_FOLDERS_DESIGN.md): Collect All and Save round trip ──
    //
    // Synthetic project referencing fixture files in a temp dir — video + audio
    // + a GLB carried as a binding-driven string param (no flag) whose value
    // lives ONLY in the preset-def default (so collect must materialize a
    // per-clip override, never write the def's `default_value`). Collect →
    // assert Media/ layout, relative paths, source hashes unchanged (copy-only)
    // → reload through the full load pipeline → every reference resolves.

    #[test]
    fn collect_all_and_save_round_trips_mixed_families_copy_only() {
        let base = std::env::temp_dir().join(format!("manifold-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src_dir = base.join("sources");
        let proj_dir = base.join("MyShow");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&proj_dir).unwrap();

        let video_src = src_dir.join("clip1.mp4");
        let audio_src = src_dir.join("loop1.wav");
        let glb_src = src_dir.join("azalea.glb");
        std::fs::write(&video_src, b"fake mp4 bytes").unwrap();
        std::fs::write(&audio_src, b"fake wav bytes").unwrap();
        std::fs::write(&glb_src, b"fake glb bytes").unwrap();

        let video_before = super::sha256_file(&video_src).unwrap();
        let audio_before = super::sha256_file(&audio_src).unwrap();
        let glb_before = super::sha256_file(&glb_src).unwrap();

        let mut project = Project {
            project_name: "MyShow".to_string(),
            ..Project::default()
        };

        // Video library clip.
        project.video_library.add_clip(VideoClip {
            id: "vc1".to_string(),
            file_path: video_src.to_string_lossy().to_string(),
            relative_file_path: None,
            file_name: "clip1.mp4".to_string(),
            duration: 0.0,
            resolution_width: 0,
            resolution_height: 0,
            file_size: 0,
            last_modified_ticks: 0,
        });

        // Audio layer with one clip.
        let mut audio_layer = Layer::new_audio("Audio".into(), 1);
        audio_layer.clips.push(TimelineClip::new_audio(
            audio_src.to_string_lossy().to_string(),
            manifold_core::Beats::ZERO,
            manifold_core::Beats::from_f32(4.0),
            manifold_core::Seconds::ZERO,
            manifold_core::Seconds::ZERO,
        ));
        project.timeline.layers.push(audio_layer);

        // Generator layer tracking an embedded GLB import whose `model_path`
        // lives ONLY in the def default (no per-clip override).
        let glb_path = glb_src.to_string_lossy().to_string();
        let mesh_bind = StringBindingDef {
            id: "model_path".to_string(),
            label: "Model File".to_string(),
            default_value: glb_path.clone(),
            target: BindingTarget::Node {
                node_id: NodeId::new("mesh"),
                param: "path".to_string(),
            },
        };
        project.upsert_embedded_preset(path_preset(
            "azalea",
            // No `is_file_path` flag — the pre-flag embedded preset case
            // (BUG-gqne): collect follows the binding, not the marker.
            vec![sp("model_path", &glb_path, false)],
            vec![mesh_bind],
            vec![node("mesh", "node.gltf_mesh_source")],
        ));
        let mut gen_layer = Layer::new_generator("Azalea".into(), PresetTypeId::new("azalea"), 2);
        gen_layer.clips.push(TimelineClip::new_generator(
            manifold_core::Beats::ZERO,
            manifold_core::Beats::from_f32(16.0),
        ));
        project.timeline.layers.push(gen_layer);

        let audio_layer_id = project
            .timeline
            .layers
            .iter()
            .find(|l| l.is_audio())
            .unwrap()
            .layer_id
            .clone();
        let gen_layer_id = project
            .timeline
            .layers
            .iter()
            .find(|l| l.layer_type == LayerType::Generator)
            .unwrap()
            .layer_id
            .clone();

        let project_path = proj_dir.join("MyShow.manifold");
        let report = collect_all_and_save(&mut project, &project_path).unwrap();

        // On-disk layout: one file per family (video, audio, meshes).
        assert!(proj_dir.join("Media/Video/clip1.mp4").is_file());
        assert!(proj_dir.join("Media/Audio/loop1.wav").is_file());
        assert!(proj_dir.join("Media/Meshes/azalea.glb").is_file());

        // Report: three unique files copied, none missing, none already local.
        assert_eq!(report.copied, 3, "one file per family");
        assert_eq!(report.re_pointed, 3, "all three refs re-pointed");
        assert_eq!(report.missing, 0);
        assert_eq!(report.already_local, 0);
        assert_eq!(report.bytes_copied, 42, "sum of the three fixture file sizes");

        // Copy-only invariant: sources untouched.
        assert_eq!(super::sha256_file(&video_src).unwrap(), video_before);
        assert_eq!(super::sha256_file(&audio_src).unwrap(), audio_before);
        assert_eq!(super::sha256_file(&glb_src).unwrap(), glb_before);

        // Video re-pointed to the relative form.
        let vc = &project.video_library.clips[0];
        assert_eq!(vc.relative_file_path.as_deref(), Some("Media/Video/clip1.mp4"));
        assert!(vc.file_path.ends_with("Media/Video/clip1.mp4"), "{}", vc.file_path);
        assert!(std::path::Path::new(&vc.file_path).exists());

        // Audio re-pointed to the relative form.
        let (_, audio_layer) = project.timeline.find_layer_by_id(audio_layer_id.as_str()).unwrap();
        let audio_clip = &audio_layer.clips[0];
        assert_eq!(
            audio_clip.relative_audio_file_path.as_deref(),
            Some("Media/Audio/loop1.wav")
        );
        assert!(audio_clip.audio_file_path.ends_with("Media/Audio/loop1.wav"));
        assert!(std::path::Path::new(&audio_clip.audio_file_path).exists());

        // The GLB def-default-only param was materialized as a per-clip override.
        let (_, gen_layer) = project.timeline.find_layer_by_id(gen_layer_id.as_str()).unwrap();
        let model = gen_layer.clips[0]
            .string_params
            .as_ref()
            .unwrap()
            .get("model_path")
            .unwrap();
        assert!(model.ends_with("Media/Meshes/azalea.glb"), "{model}");
        assert!(std::path::Path::new(model).exists());

        // D5a: the def default is untouched — it still names the ORIGINAL source.
        let def = project.embedded_preset(&PresetTypeId::new("azalea")).unwrap();
        let def_default = def
            .def
            .preset_metadata
            .as_ref()
            .unwrap()
            .string_params
            .iter()
            .find(|s| s.id == "model_path")
            .unwrap()
            .default_value
            .clone();
        assert_eq!(def_default, glb_path, "def default never rewritten");

        // Reload through the full load pipeline → every reference resolves.
        let reloaded = crate::loader::load_project(&project_path).unwrap();
        let vc2 = &reloaded.video_library.clips[0];
        assert!(std::path::Path::new(&vc2.file_path).exists());
        let (_, al2) = reloaded.timeline.find_layer_by_id(audio_layer_id.as_str()).unwrap();
        assert!(std::path::Path::new(&al2.clips[0].audio_file_path).exists());
        let (_, gl2) = reloaded.timeline.find_layer_by_id(gen_layer_id.as_str()).unwrap();
        let model2 = gl2.clips[0]
            .string_params
            .as_ref()
            .unwrap()
            .get("model_path")
            .unwrap();
        assert!(std::path::Path::new(model2).exists(), "reloaded GLB path resolves: {model2}");

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── P5 (PROJECT_FOLDERS_DESIGN.md) ─────────────────────────────

    /// `node.image_folder` string param → folder-valued ref enumerates Images,
    /// Collect copies the tree into `Media/Images/`, and the per-clip override
    /// is re-pointed to the in-folder tree (BUG-3i1p).
    #[test]
    fn image_folder_param_collects_tree_into_media_images() {
        let base = std::env::temp_dir().join(format!("manifold-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src_dir = base.join("scan");
        std::fs::create_dir_all(src_dir.join("sub")).unwrap();
        std::fs::write(src_dir.join("slice01.png"), b"png1").unwrap();
        std::fs::write(src_dir.join("sub/slice02.png"), b"png2").unwrap();

        let folder_path = src_dir.to_string_lossy().to_string();
        let folder_bind = StringBindingDef {
            id: "volume_folder".to_string(),
            label: "Volume Folder".to_string(),
            default_value: folder_path.clone(),
            target: BindingTarget::Node {
                node_id: NodeId::new("vol"),
                param: "folder".to_string(),
            },
        };
        let mut project = Project::default();
        project.upsert_embedded_preset(path_preset(
            "mri",
            vec![sp("volume_folder", &folder_path, false)],
            vec![folder_bind],
            vec![node("vol", "node.image_folder")],
        ));
        let mut gen_layer = Layer::new_generator("MRI".into(), PresetTypeId::new("mri"), 0);
        gen_layer.clips.push(TimelineClip::new_generator(
            manifold_core::Beats::ZERO,
            manifold_core::Beats::from_f32(8.0),
        ));
        let gen_layer_id = gen_layer.layer_id.clone();
        project.timeline.layers.push(gen_layer);

        // Enumeration: the folder is a single Images ref (directory-valued).
        let refs = collect_asset_paths(&project);
        let folder_refs: Vec<_> = refs
            .iter()
            .filter(|r| matches!(&r.target, AssetTarget::StringParam { key, .. } if key == "volume_folder"))
            .collect();
        assert_eq!(folder_refs.len(), 1);
        assert_eq!(folder_refs[0].kind, AssetKind::Images);
        assert_eq!(folder_refs[0].path, std::path::Path::new(&folder_path));

        // Collect: tree copied, override re-pointed to the in-folder tree.
        let proj_dir = base.join("Show");
        std::fs::create_dir_all(&proj_dir).unwrap();
        let project_path = proj_dir.join("Show.manifold");
        collect_all_and_save(&mut project, &project_path).unwrap();

        assert!(proj_dir.join("Media/Images/scan/slice01.png").is_file());
        assert!(proj_dir.join("Media/Images/scan/sub/slice02.png").is_file());
        // The DEF default is untouched (D5a): it still names the original.
        let def = project.embedded_preset(&PresetTypeId::new("mri")).unwrap();
        let def_folder = def
            .def
            .preset_metadata
            .as_ref()
            .unwrap()
            .string_params
            .iter()
            .find(|s| s.id == "volume_folder")
            .unwrap()
            .default_value
            .clone();
        assert_eq!(def_folder, folder_path);
        let (_, layer) = project.timeline.find_layer_by_id(gen_layer_id.as_str()).unwrap();
        let stored = layer.clips[0].string_params.as_ref().unwrap()["volume_folder"].clone();
        let stored_path = std::path::Path::new(&stored);
        assert!(stored_path.is_dir(), "override re-pointed to in-folder tree: {stored}");
        assert!(
            stored_path.starts_with(proj_dir.join("Media/Images")),
            "re-pointed folder lives under Media/Images: {stored}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Image clip round trip (BUG-2jbn): a still-image clip's file collects to
    /// `Media/Images/`, `image_path` + `relative_image_path` re-point, save +
    /// reload resolves.
    #[test]
    fn image_clip_collects_and_round_trips() {
        let base = std::env::temp_dir().join(format!("manifold-image-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src_dir = base.join("sources");
        let proj_dir = base.join("ImageShow");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&proj_dir).unwrap();
        let img_src = src_dir.join("logo.png");
        std::fs::write(&img_src, b"fake png bytes").unwrap();
        let img_before = super::sha256_file(&img_src).unwrap();

        let mut project = Project {
            project_name: "ImageShow".to_string(),
            ..Project::default()
        };
        let mut layer = Layer::new_video("Img".into(), 0);
        let mut clip = TimelineClip::new_image(
            img_src.to_string_lossy().to_string(),
            manifold_core::Beats::ZERO,
            manifold_core::Beats::from_f32(4.0),
        );
        clip.id = manifold_core::id::ClipId::new("img1");
        layer.clips.push(clip);
        project.timeline.layers.push(layer);
        let layer_id = project.timeline.layers[0].layer_id.clone();

        let project_path = proj_dir.join("ImageShow.manifold");
        let report = collect_all_and_save(&mut project, &project_path).unwrap();

        assert!(proj_dir.join("Media/Images/logo.png").is_file());
        assert_eq!(report.copied, 1);
        assert_eq!(report.re_pointed, 1);
        assert_eq!(super::sha256_file(&img_src).unwrap(), img_before, "copy-only invariant");

        let (_, l) = project.timeline.find_layer_by_id(layer_id.as_str()).unwrap();
        assert_eq!(l.clips[0].relative_image_path.as_deref(), Some("Media/Images/logo.png"));
        assert!(l.clips[0].image_path.ends_with("Media/Images/logo.png"));
        assert!(std::path::Path::new(&l.clips[0].image_path).exists());

        // Reload through the full load pipeline → the image path resolves.
        let reloaded = crate::loader::load_project(&project_path).unwrap();
        let (_, rl) = reloaded.timeline.find_layer_by_id(layer_id.as_str()).unwrap();
        assert!(std::path::Path::new(&rl.clips[0].image_path).exists(), "reloaded image resolves");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Coverage guard (PROJECT_FOLDERS_DESIGN P5 enforcement): source-scans the
    /// manifold-core model files for `pub …path…: String` / `Option<String>`
    /// field declarations and asserts each field name is named in this test's
    /// known-covered list. A NEW path field in the model must fail this test
    /// until the collect inventory is extended to cover it.
    #[test]
    fn every_model_path_field_is_inventory_covered() {
        let files = [
            "../manifold-core/src/clip.rs",
            "../manifold-core/src/video.rs",
            "../manifold-core/src/layer.rs",
            "../manifold-core/src/project/mod.rs",
        ];
        // Bookkeeping fields, explicitly exempt:
        // - `last_saved_path` — the .manifold file's own path, never collected.
        // - `legacy_perc_audio_path` — a legacy percent-analysis path, inert.
        let exempt = ["last_saved_path", "legacy_perc_audio_path"];
        let covered = [
            "file_path",
            "relative_file_path",
            "video_folder_path",
            "relative_video_folder_path",
            "audio_file_path",
            "relative_audio_file_path",
            "image_path",
            "relative_image_path",
        ];
        let mut found = Vec::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for f in &files {
            let text = std::fs::read_to_string(root.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
            for line in text.lines() {
                let t = line.trim();
                // Match `pub name: String` / `pub name: Option<String>` /
                // `pub relative_image_path: Option<String>`.
                let Some(start) = t.find("pub ") else { continue };
                let body = &t[start + 4..];
                let Some(colon) = body.find(':') else { continue };
                let (name, ty) = body.split_at(colon);
                let name = name.trim();
                let ty = ty
                    .trim_start_matches(':')
                    .trim()
                    .trim_end_matches(',')
                    .trim();
                if !(name.ends_with("path") || name.ends_with("_path"))
                    || !(ty == "String" || ty.starts_with("Option<"))
                {
                    continue;
                }
                found.push(name.to_string());
            }
        }
        for name in &found {
            if exempt.contains(&name.as_str()) {
                continue;
            }
            assert!(
                covered.contains(&name.as_str()),
                "model path field `{name}` is NOT named in the collect inventory's \
                 covered list — add it to collect_asset_paths and this test"
            );
        }
        // Sanity: the scan actually found the fields; the covered list is not
        // silently rot (a removed field would keep the list artificially green).
        for name in &covered {
            assert!(
                found.contains(&name.to_string()),
                "covered field `{name}` no longer exists in the model — remove it from the list"
            );
        }
    }
}