//! The asset-path inventory (PROJECT_FOLDERS_DESIGN.md D4).
//!
//! [`collect_asset_paths`] is THE enumeration of every external file a project
//! references — video library clips, audio clips, layer video folders, and
//! every string param flagged `is_file_path` on any generator instance. Path
//! Resolver extension (P2), Collect All and Save (P4), and any future
//! missing-file report all read it. No second list anywhere.
//!
//! The string-param half is flag-driven by design (D5): a preset author marks
//! a new path param and it gets collected for free. The io layer never names a
//! param id — the design's negative `rg` gate (no path-param id literal in the
//! io crate) enforces that.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::PresetInstance;
use manifold_core::id::{ClipId, LayerId};
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use std::path::{Path, PathBuf};

/// Which media family an asset belongs to — the `Media/` subfolder it collects
/// into (D2): `Media/Video`, `Media/Audio`, `Media/Meshes`, `Media/HDRIs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Video,
    Audio,
    Mesh,
    Hdri,
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
    /// A flagged string param on a generator layer, keyed by param id. The
    /// value lives per-clip (`TimelineClip.string_params`) with a fallback to
    /// the preset-def default.
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
/// - every string param flagged `is_file_path` on any generator instance
///   (the model/HDRI globs a preset loads — any preset's path param, flagged
///   by its author) — flag-driven, from the instance's own def first, then the
///   project's embedded preset, then the global definition registry (which
///   already reflects this project's embedded presets through the app's
///   overlay).
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
        }
    }

    // Flagged string params on every generator layer. Only `LayerType::Generator`
    // layers carry a `gen_params` instance today; effects gain string params as
    // the capability gap closes — the same walk extends to them unchanged.
    for layer in &project.timeline.layers {
        if layer.layer_type != LayerType::Generator {
            continue;
        }
        let Some(inst) = layer.gen_params() else {
            continue;
        };
        let defs = resolve_string_defs(project, inst);
        for (key, default) in &defs {
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
                    kind: classify_string_param(key, value, inst_graph(inst)),
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

/// The flagged string-param defs for a generator instance, as owned `(key,
/// default_value)` pairs, ordered by the def. Resolution order (same as the
/// runtime load): the instance's own graph metadata first, then the project's
/// embedded preset by tracked id, then the global definition registry.
fn resolve_string_defs(project: &Project, inst: &PresetInstance) -> Vec<(String, String)> {
    // 1. The instance's own graph metadata carries the full `StringParamSpecDef`
    //    list when it has diverged (or was just imported as an embedded graph).
    if let Some(graph) = inst_graph(inst)
        && let Some(meta) = graph.preset_metadata.as_ref()
    {
        return meta
            .string_params
            .iter()
            .filter(|sp| sp.is_file_path)
            .map(|sp| (sp.id.clone(), sp.default_value.clone()))
            .collect();
    }
    // 2. An embedded preset by the tracked id is self-contained (graph +
    //    metadata) — the import case, where the layer tracks by id (graph: None).
    let id = inst.generator_type();
    if let Some(embedded) = project.embedded_preset(id)
        && let Some(meta) = embedded.def.preset_metadata.as_ref()
    {
        return meta
            .string_params
            .iter()
            .filter(|sp| sp.is_file_path)
            .map(|sp| (sp.id.clone(), sp.default_value.clone()))
            .collect();
    }
    // 3. Stock/user catalog preset — the global registry. (The app's overlay
    //    installs this project's embedded presets into it at load, so 2 would
    //    usually have caught them; the registry is the fallback regardless.)
    manifold_core::preset_definition_registry::try_get(id).map_or_else(Vec::new, |def| {
        def.string_param_defs
            .iter()
            .filter(|sp| sp.is_file_path)
            .map(|sp| (sp.key.to_string(), sp.default_value.to_string()))
            .collect()
    })
}

/// The full `EffectGraphDef` carried directly on a `PresetInstance`'s own
/// graph.
fn inst_graph(inst: &PresetInstance) -> Option<&EffectGraphDef> {
    inst.graph.as_ref()
}

/// Classify a flagged string param into a media family. Binding-following:
/// the param's `stringBinding` targets a node; `node.hdri_source` reads an
/// `.exr` envmap → Hdri, every other file-loading node in a generator reads
/// the GLB → Mesh. Fallback (no metadata / no binding / no matching node) is
/// extension-based: `.hdr`/`.exr` → Hdri, else Mesh.
fn classify_string_param(key: &str, value: &str, graph: Option<&EffectGraphDef>) -> AssetKind {
    if let Some(graph) = graph
        && let Some(meta) = graph.preset_metadata.as_ref()
    {
        let binding = meta.string_bindings.iter().find(|b| b.id == key);
        if let Some(binding) = binding
            && let manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } =
                &binding.target
        {
            let node_type = graph
                .nodes
                .iter()
                .find(|n| &n.node_id == node_id)
                .map(|n| n.type_id.as_str());
            return match node_type {
                Some(t) if t.contains("hdri") => AssetKind::Hdri,
                _ => AssetKind::Mesh,
            };
        }
    }
    // No binding-informed signal — fall back to the file extension.
    let ext = Path::new(value)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "hdr" | "exr") {
        AssetKind::Hdri
    } else {
        AssetKind::Mesh
    }
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
                sp("mesh_path", "/mnt/models/azalea.glb", true),
                sp("env_path", "", true),
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
                && r.path == PathBuf::from("/mnt/video/clip1.mp4")
                && r.target == AssetTarget::VideoClip { clip_id: "vc1".to_string() }
        }));
        // Layer video folder.
        assert!(refs.iter().any(|r| {
            r.kind == AssetKind::Video
                && r.path == PathBuf::from("/mnt/footage")
                && r.target == AssetTarget::LayerVideoFolder { layer_id: folder_layer_id.clone() }
        }));
        // Audio clip.
        assert!(refs.iter().any(|r| {
            r.kind == AssetKind::Audio && r.path == PathBuf::from("/mnt/audio/loop1.wav")
        }));
    }

    #[test]
    fn flagged_string_params_collect_mesh_and_hdri() {
        let (project, _folder_layer_id, _audio_layer_id) = project_with_all_families();
        let refs = collect_asset_paths(&project);

        // mesh_path → Mesh, from the embedded preset's def default.
        assert!(refs.iter().any(|r| {
            r.kind == AssetKind::Mesh
                && r.path == PathBuf::from("/mnt/models/azalea.glb")
                && matches!(&r.target, AssetTarget::StringParam { key, .. } if key == "mesh_path")
        }));
        // env_path is empty in the def and has no per-clip override → skipped.
        assert!(!refs.iter().any(|r| {
            matches!(&r.target, AssetTarget::StringParam { key, .. } if key == "env_path")
        }));
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
            r.kind == AssetKind::Mesh && r.path == PathBuf::from("/mnt/models/swap.glb")
        }));
        assert!(!refs.iter().any(|r| r.path == PathBuf::from("/mnt/models/azalea.glb")));
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
            vec![],
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
}