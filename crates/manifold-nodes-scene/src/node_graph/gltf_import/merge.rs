//! Merge an imported model into an EXISTING generator graph's
//! `render_scene`: reuse the per-object group builder against the target
//! scene's node-id / object-index range, no camera/envmap/lights/lens.


use std::path::Path;

use manifold_core::effect_graph_def::{
    BindingDef, EffectGraphDef, EffectGraphNode, EffectGraphWire, ParamSpecDef,
    SerializedParamValue, StringBindingDef, StringParamSpecDef,
};

use crate::node_graph::gltf_load;
use crate::node_graph::gltf_load::GltfImportSummary;
use crate::node_graph::primitives::render_scene::OBJECT_SAFETY_MAX;

use super::assembly::*;
use super::cards::*;
use super::object_group::*;
mod scans;
pub(super) use scans::max_node_id_recursive;
use scans::max_local_k_recursive;


/// D5's merge plan: everything `ImportModelIntoSceneCommand`
/// (`manifold-editing`) needs to splice a SECOND (third, nth) glTF's object
/// groups into an EXISTING scene's `render_scene`, without touching that
/// scene's own chrome (camera/envmap/lights/lens — the target scene keeps
/// its own). Every field is a plain `manifold_core` type so the editing
/// crate, which cannot depend on `manifold-nodes`, can consume it without a dependency-direction violation: the caller
/// (`manifold-app`, which depends on both) builds this plan here, then
/// hands its fields to `ImportModelIntoSceneCommand::new`.
#[derive(Debug, Clone)]
pub struct MergePlan {
    /// The target scene's `node.render_scene` node id — informational, so
    /// the command doesn't have to re-search `def` for it.
    pub render_scene_node_id: u32,
    /// New object groups, matching [`build_import_graph`]: one compound group
    /// for a static asset, or the existing material groups for animated assets.
    /// Camera, environment, lights and lens remain owned by the target scene.
    pub new_nodes: Vec<EffectGraphNode>,
    /// New top-level wires: every material Object output into `render_scene`'s
    /// `object_{k}` port, continuing from the target's existing draw count.
    pub new_wires: Vec<EffectGraphWire>,
    /// `render_scene`'s new `objects` param value (existing + incoming).
    pub new_objects_count: u32,
    /// Card-spec additions (per-object knobs, sectioned by object/group
    /// name — same shape the importer's own outer card carries).
    pub new_card_params: Vec<ParamSpecDef>,
    pub new_card_bindings: Vec<BindingDef>,
    /// The incoming file's Model File spec when the scene does not bind that
    /// file yet; its bindings use this id (typed identity D5).
    pub new_string_params: Vec<StringParamSpecDef>,
    pub new_string_bindings: Vec<StringBindingDef>,
    /// D9 doctrine ("every import produces a report") applied to the merge:
    /// unmapped per-material features (same as [`ImportReport::report_lines`])
    /// plus, when the D5 scale-sanity rule fired, one line naming the
    /// normalize factor applied. Never a silent adjustment.
    pub report_lines: Vec<String>,
    /// The graph's scene bounds after the merge: the target's own when it has
    /// them, else the incoming asset's. The card ranges above are scaled by
    /// this radius and the command saves it, so the live lookup reads the same.
    pub scene_bounds: ([f32; 3], [f32; 3]),
}

manifold_core::testkit_visible! {
/// D5 — merge a second (third, nth) glTF's objects into `def`'s EXISTING
/// `node.render_scene`, reusing [`build_object_group`] (the SAME per-object
/// shape [`build_import_graph`] emits) for every incoming material. Never
/// calls [`assemble_import_graph`] / [`build_import_graph`] — this function
/// builds ONLY object groups, no chrome, so there is nothing to filter back
/// out and no chrome-duplication risk (the rejected "splice the whole
/// assembled def" alternative, D5).
///
/// New node ids are allocated above `def`'s current max id (see
/// [`max_node_id_recursive`]) — a merge twin of [`build_import_graph`]'s own
/// `fresh_id`. Group names that collide with an existing top-level handle
/// get suffixed by [`unique_group_name`] (the importer's own dedup helper,
/// reused verbatim) — `used_group_names` is seeded with every existing
/// top-level handle, not just names from this merge, so a merged object
/// can never silently share a namespace root with the scene's own chrome
/// or another object.
///
/// **Units (D-115):** the incoming asset keeps its file's own units (glTF
/// metres), whatever the size of what is already in the scene. Each new
/// object's `transform_3d` is seeded with `pos = -center` (the importer's
/// recenter convention) and no scale, so card Scale 1.0 is the imported size.
/// The camera's Frame shows the result, as in Blender, Unity and Cinema 4D.
pub(super) fn merge_import_into_graph(
    def: &EffectGraphDef,
    summary: &GltfImportSummary,
    path: &Path,
) -> Result<MergePlan, String> {
    if summary.materials.is_empty() {
        return Err(format!(
            "{}: no materials with geometry — nothing to import",
            path.display()
        ));
    }

    let Some(render_scene_node) =
        def.nodes.iter().find(|n| n.type_id == super::scene_vm::RENDER_SCENE_TYPE_ID)
    else {
        return Err(
            "target scene graph has no top-level node.render_scene — cannot merge an import \
             into it"
                .to_string(),
        );
    };
    let render_scene_node_id = render_scene_node.id;
    let existing_objects: u32 = match render_scene_node.params.get("objects") {
        Some(SerializedParamValue::Float { value }) => value.round().max(0.0) as u32,
        Some(SerializedParamValue::Int { value }) => (*value).max(0) as u32,
        _ => 0,
    };

    let mut materials = summary.materials.clone();
    materials.sort_by(|a, b| b.vertex_count.cmp(&a.vertex_count));
    let incoming = materials.len();

    // GLB_CONFORMANCE_DESIGN.md D4 / OBJECT_SAFETY_MAX — enforced on the
    // POST-MERGE total (P4), same loud-error posture as the importer's own
    // over-bound reject. Never a silent partial merge.
    if incoming > OBJECT_SAFETY_MAX as usize {
        return Err(format!(
            "{}: {incoming} materials with geometry exceeds the {OBJECT_SAFETY_MAX}-object \
             safety bound on its own — this asset cannot be imported 1:1 without risking a \
             runaway port-list (raise OBJECT_SAFETY_MAX in render_scene.rs if a real asset \
             legitimately needs more; never silently truncate)",
            path.display(),
        ));
    }
    let post_merge_total = existing_objects as usize + incoming;
    if post_merge_total > OBJECT_SAFETY_MAX as usize {
        return Err(format!(
            "{}: merging {incoming} object(s) into a scene that already has {existing_objects} \
             would total {post_merge_total}, exceeding the {OBJECT_SAFETY_MAX}-object safety \
             bound — this merge cannot proceed without risking a runaway port-list on \
             render_scene (raise OBJECT_SAFETY_MAX in render_scene.rs if a real scene \
             legitimately needs more; never silently drop objects)",
            path.display(),
        ));
    }

    let center = [
        (summary.bbox_min[0] + summary.bbox_max[0]) * 0.5,
        (summary.bbox_min[1] + summary.bbox_max[1]) * 0.5,
        (summary.bbox_min[2] + summary.bbox_max[2]) * 0.5,
    ];
    let dims = [
        summary.bbox_max[0] - summary.bbox_min[0],
        summary.bbox_max[1] - summary.bbox_min[1],
        summary.bbox_max[2] - summary.bbox_min[2],
    ];
    let incoming_radius =
        ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(1e-3);
    let mut next_id = max_node_id_recursive(&def.nodes) + 1;
    let mut fresh_id = move || {
        let v = next_id;
        next_id += 1;
        v
    };

    // Seeded with every existing top-level handle (not just group names) —
    // conservative, and cheap, so a merged object can never silently share
    // a namespace root with existing scene chrome or another object.
    let mut used_group_names: std::collections::HashSet<String> =
        def.nodes.iter().filter_map(|n| n.handle.clone()).collect();

    let node_anims_by_clip: Vec<std::collections::BTreeMap<usize, gltf_load::GltfNodeAnimation>> =
        summary
            .animations
            .iter()
            .map(|a| a.nodes.iter().map(|n| (n.node_index, n.clone())).collect())
            .collect();

    let path_str = path.to_string_lossy().into_owned();
    // One string id per file: a file the scene already binds keeps its id,
    // any other file gets its own, so its Model File and its loads are its own.
    let (model_file_id, new_model_file_spec) = manifold_core::binding_identity::string_id_for(
        def.preset_metadata.as_ref(),
        super::MODEL_FILE_PARAM_ID,
        &path_str,
        &super::model_file_spec(&path_str),
    );

    let mut new_nodes = Vec::new();
    let mut new_wires = Vec::new();
    let mut new_card_params = Vec::new();
    let mut new_card_bindings = Vec::new();
    let mut new_string_bindings = Vec::new();
    let mut report_lines = Vec::new();
    let mut any_animated = false;

    // Per-glb animation section (see `animation_card_params`): each merged
    // file gets its OWN linked Rate/Clip/Loop/Retrigger set, so the card id
    // prefix must not collide with the target scene's existing sections
    // ("anim" from the original import, or an earlier merge's "anim2"…).
    let existing_card_ids: std::collections::HashSet<&str> = def
        .preset_metadata
        .as_ref()
        .map(|m| m.params.iter().map(|p| p.id.as_str()).collect())
        .unwrap_or_default();
    let anim_prefix = {
        let mut n = 1usize;
        loop {
            let candidate = if n == 1 { "anim".to_string() } else { format!("anim{n}") };
            if !existing_card_ids.contains(format!("{candidate}_rate").as_str()) {
                break candidate;
            }
            n += 1;
        }
    };

    let mut import_ctx = ImportCtx {
        render_id: render_scene_node_id,
        path_str: &path_str,
        model_file_id: &model_file_id,
        center,
        bbox_radius: incoming_radius,
        node_anims_by_clip: &node_anims_by_clip,
        used_group_names: &mut used_group_names,
        fresh_id: &mut fresh_id,
        texture_dims: &summary.texture_dims,
    };
    // BUG-w5wv: `local_k` (this merge's OWN inner-handle numbering) starts
    // past whatever the target scene's existing objects already used, so
    // "mat_{k}"/"mesh_{k}"/… never collides with a target object's own
    // handles — see `max_local_k_recursive`. `port_index` (render_scene's
    // `object_{N}` port numbering) is a separate, already-correct offset,
    // unchanged.
    let local_k_offset = max_local_k_recursive(&def.nodes).map_or(0usize, |k| k as usize + 1);
    let static_compound = materials.len() > 1
        && materials.iter().all(|m| {
            m.animations.iter().all(Option::is_none)
                && m.skin.is_none()
                && m.morph.is_none()
                && m.rigid_multi_node.is_none()
        });
    let asset_name = path
        .file_stem()
        .map(|s| sanitize_identifier(&s.to_string_lossy()))
        .unwrap_or_else(|| "ImportedModel".to_string());
    let mut outputs = if static_compound {
        vec![build_static_compound_group(
            &mut import_ctx,
            local_k_offset,
            existing_objects as usize,
            &materials,
            &asset_name,
            &anim_prefix,
        )]
    } else {
        materials
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let local_k = local_k_offset + i;
                let port_index = existing_objects as usize + i;
                build_object_group(&mut import_ctx, local_k, i, port_index, m, &anim_prefix)
            })
            .collect()
    };
    for mut out in outputs.drain(..) {
        new_nodes.push(out.group_node);
        new_wires.append(&mut out.wires_to_render);
        new_card_params.append(&mut out.card_params);
        new_card_bindings.append(&mut out.card_bindings);
        new_string_bindings.append(&mut out.string_bindings);
        report_lines.append(&mut out.report_lines);
        any_animated |= out.animated;
    }

    if any_animated {
        // Section named after the file so a scene holding several animated
        // glbs reads as one linked control per model, not one anonymous
        // "Animation" pile.
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ImportedModel".to_string());
        let section = format!("Animation — {}", sanitize_identifier(&stem));
        let mut anim_params = Vec::new();
        animation_card_params(
            &mut anim_params,
            &anim_prefix,
            &section,
            summary.animations.len().max(1) as u32,
            clip_labels(&summary.animations),
        );
        anim_params.append(&mut new_card_params);
        new_card_params = anim_params;
    }

    // One radius per graph: the scene's saved bounds win, and a scene with
    // none takes the incoming asset's; the lookup scales ranges by it.
    let scene_bounds = def
        .preset_metadata
        .as_ref()
        .and_then(|meta| meta.scene_bounds)
        .unwrap_or((summary.bbox_min, summary.bbox_max));

    Ok(MergePlan {
        render_scene_node_id,
        new_nodes,
        new_wires,
        new_objects_count: (existing_objects as usize + incoming) as u32,
        new_card_params,
        new_card_bindings,
        new_string_params: new_model_file_spec.into_iter().collect(),
        new_string_bindings,
        report_lines,
        scene_bounds,
    })
}
}

/// Public entry point for the "Import Model…" merge gesture
/// (`manifold-app`'s dispatch calls this — never [`merge_import_into_graph`]
/// directly, since that function takes a [`GltfImportSummary`], which is
/// `pub(crate)` to `manifold-nodes` and so cannot appear in a public
/// signature; the exact same constraint [`assemble_import_graph`] resolves
/// for [`build_import_graph`]). One CPU parse via
/// [`gltf_load::gltf_import_summary`], then the pure merge.
pub fn assemble_merge_plan(def: &EffectGraphDef, path: &Path) -> Result<MergePlan, String> {
    let summary = gltf_load::gltf_import_summary(path)?;
    merge_import_into_graph(def, &summary, path)
}
