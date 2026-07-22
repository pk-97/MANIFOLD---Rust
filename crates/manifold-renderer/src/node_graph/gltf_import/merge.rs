//! Merge an imported model into an EXISTING generator graph's
//! `render_scene`: reuse the per-object group builder against the target
//! scene's node-id / object-index range, no camera/envmap/lights/lens.

use std::path::Path;

use manifold_core::effect_graph_def::{
    BindingDef, EffectGraphDef, EffectGraphNode, EffectGraphWire, ParamSpecDef,
    SerializedParamValue, StringBindingDef,
};

use crate::node_graph::gltf_load;
use crate::node_graph::gltf_load::GltfImportSummary;
use crate::node_graph::primitives::render_scene::OBJECT_SAFETY_MAX;

use super::assembly::*;
use super::cards::*;
use super::object_group::*;

/// Recursively find the largest node `id` anywhere in `nodes`, including
/// inside group bodies. Node ids only need to be unique WITHIN the level
/// (`Vec<EffectGraphNode>`) that holds them — `descend_level` looks a group
/// id up in its own sibling list, and the flattener assigns every node a
/// brand-new global id at load time (`manifold_core::flatten::flatten_groups`
/// — `clone.id = new_id`) — so a merge only strictly needs to avoid
/// colliding with the TOP-LEVEL ids `render_scene`'s siblings use. Walking
/// every nesting level anyway costs nothing and is the simplest thing that
/// is obviously correct for every level at once.
pub(super) fn max_node_id_recursive(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|n| {
            let inner = n.group.as_ref().map(|g| max_node_id_recursive(&g.nodes)).unwrap_or(0);
            n.id.max(inner)
        })
        .max()
        .unwrap_or(0)
}

/// the largest KNOWN `source_bbox_radius` (BUG-194's
/// import-time provenance param, stamped on every `node.gltf_mesh_source`/
/// `node.gltf_skinned_mesh_source` this session's importer creates) among
/// every mesh-source node already in the target def, searched recursively
/// (mesh-source nodes live inside each object's group, same nesting
/// `max_node_id_recursive` walks). `-1.0` (the "unknown" sentinel — a
/// hand-built node the importer never touched) is excluded, never treated
/// as a real radius of zero. `None` when the def has no mesh-source node
/// with a known radius at all (a hand-built scene with no glTF import in
/// its history) — the caller falls back to the orbit-camera proxy.
fn max_known_source_bbox_radius(nodes: &[EffectGraphNode]) -> Option<f32> {
    nodes
        .iter()
        .filter_map(|n| {
            let own = matches!(
                n.type_id.as_str(),
                "node.gltf_mesh_source" | "node.gltf_skinned_mesh_source"
            )
            .then(|| match n.params.get("source_bbox_radius") {
                Some(SerializedParamValue::Float { value }) if *value >= 0.0 => Some(*value),
                _ => None,
            })
            .flatten();
            let inner = n.group.as_ref().and_then(|g| max_known_source_bbox_radius(&g.nodes));
            match (own, inner) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            }
        })
        .fold(None, |acc, v| Some(acc.map_or(v, |a: f32| a.max(v))))
}

/// D5's merge plan: everything `ImportModelIntoSceneCommand`
/// (`manifold-editing`) needs to splice a SECOND (third, nth) glTF's object
/// groups into an EXISTING scene's `render_scene`, without touching that
/// scene's own chrome (camera/envmap/lights/lens — the target scene keeps
/// its own). Every field is a plain `manifold_core` type so the editing
/// crate — which cannot depend on `manifold-renderer` (see
/// `AddSceneObjectCommand`'s own doc comment for the same constraint) —
/// can consume it without a dependency-direction violation: the caller
/// (`manifold-app`, which depends on both) builds this plan here, then
/// hands its fields to `ImportModelIntoSceneCommand::new`.
#[derive(Debug, Clone)]
pub struct MergePlan {
    /// The target scene's `node.render_scene` node id — informational, so
    /// the command doesn't have to re-search `def` for it.
    pub render_scene_node_id: u32,
    /// New top-level nodes: one `GROUP_TYPE_ID` group per incoming
    /// material, same shape [`build_import_graph`] emits per object. NO
    /// camera, NO envmap, NO lights, NO lens — the target scene's chrome is
    /// never touched or duplicated.
    pub new_nodes: Vec<EffectGraphNode>,
    /// New top-level wires: each new group's single `object` output into
    /// `render_scene`'s `object_{k}` port, `k` continuing from the target's
    /// existing `objects` count.
    pub new_wires: Vec<EffectGraphWire>,
    /// `render_scene`'s new `objects` param value (existing + incoming).
    pub new_objects_count: u32,
    /// Card-spec additions (per-object knobs, sectioned by object/group
    /// name — same shape the importer's own outer card carries).
    pub new_card_params: Vec<ParamSpecDef>,
    pub new_card_bindings: Vec<BindingDef>,
    pub new_string_bindings: Vec<StringBindingDef>,
    /// D9 doctrine ("every import produces a report") applied to the merge:
    /// unmapped per-material features (same as [`ImportReport::report_lines`])
    /// plus, when the D5 scale-sanity rule fired, one line naming the
    /// normalize factor applied. Never a silent adjustment.
    pub report_lines: Vec<String>,
}

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
/// **Scale sanity (D5):** the incoming asset keeps its native units; each
/// new object's `transform_3d` is seeded with `pos = -center` (the
/// importer's own recenter convention). A uniform `scale` is ALSO seeded,
/// but only when the incoming bbox radius differs from a "scene reference
/// radius" by more than 10× in either direction. **BUG-195, fixed:** the
/// reference radius is [`max_known_source_bbox_radius`] — the largest
/// `source_bbox_radius` (BUG-194's import-time provenance param) among the
/// target def's own existing mesh-source nodes, a real per-object size fact
/// rather than an inversion of a camera value the user may have hand-retuned.
/// The old proxy — the target's own synthesized `node.orbit_camera`'s
/// `distance` param, inverted through the EXACT formula [`build_import_graph`]
/// used to seed it (`distance = 2.2 * radius`) — is kept as the fallback for
/// a scene with no known-radius mesh-source node at all (a hand-built scene
/// with no glTF import in its history). No top-level `node.orbit_camera`
/// either → normalization is skipped entirely (native units), never guessed.
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

    // Prefer a STORED per-object radius (BUG-194's
    // `source_bbox_radius` provenance param) over the orbit-camera proxy
    // below — the largest known radius among the target's own existing
    // mesh-source nodes is a real fact about the scene's geometry, not an
    // inversion of a camera framing value the user may have hand-retuned.
    // Only when no mesh-source node in the def has a known radius (e.g. a
    // scene built entirely by hand, never touched by this importer) does
    // the proxy apply — kept unchanged as the fallback, never removed.
    let scene_reference_radius: Option<f32> =
        max_known_source_bbox_radius(&def.nodes).filter(|r| *r > 1e-6).or_else(|| {
            def.nodes
                .iter()
                .find(|n| n.type_id == "node.orbit_camera")
                .and_then(|n| match n.params.get("distance") {
                    Some(SerializedParamValue::Float { value }) => Some(*value / 2.2),
                    _ => None,
                })
                .filter(|r| *r > 1e-6)
        });

    let normalize_scale: Option<f32> = scene_reference_radius.and_then(|ref_radius| {
        let ratio = incoming_radius / ref_radius;
        if (0.1..=10.0).contains(&ratio) { None } else { Some(ref_radius / incoming_radius) }
    });

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

    for (local_k, m) in materials.iter().enumerate() {
        let port_index = existing_objects as usize + local_k;
        let mut out = build_object_group(
            local_k,
            port_index,
            render_scene_node_id,
            m,
            &path_str,
            center,
            &node_anims_by_clip,
            &mut used_group_names,
            &mut fresh_id,
            incoming_radius,
            &anim_prefix,
        );
        // D5 scale sanity: seeded on THIS object's own transform_3d — an
        // ordinary, visible, undoable value, never hidden state. Every
        // object in one incoming asset shares the same normalize factor
        // (it's the whole asset's scale, not a per-material one).
        //
        // Confessed shortcut: an object whose glTF animation ALSO drives
        // scale (`node.gltf_animation_source`'s `scale_x/y/z` wired as
        // port-shadows onto this SAME transform_3d, unconditionally, by
        // `build_object_group`) has this static seed overridden at runtime
        // by that wire — a port-shadow always wins over the static param
        // regardless of value. Normalizing a >10x-mismatched asset whose
        // objects are ALSO scale-animated is therefore a known gap, not
        // silently wrong: logged in BUG_BACKLOG (BUG-195 addendum) rather
        // than fixed here.
        if let Some(scale) = normalize_scale
            && let Some(transform_node) = out
                .group_node
                .group
                .as_mut()
                .and_then(|g| g.nodes.iter_mut().find(|n| n.type_id == "node.transform_3d"))
        {
            transform_node.params.insert("scale_x".to_string(), float(scale));
            transform_node.params.insert("scale_y".to_string(), float(scale));
            transform_node.params.insert("scale_z".to_string(), float(scale));
        }
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

    if let Some(scale) = normalize_scale {
        report_lines.push(format!(
            "merged import scaled ×{scale:.4} to match the scene (incoming radius \
             {incoming_radius:.4} vs scene reference {:.4})",
            scene_reference_radius.unwrap_or(0.0),
        ));
    }

    Ok(MergePlan {
        render_scene_node_id,
        new_nodes,
        new_wires,
        new_objects_count: (existing_objects as usize + incoming) as u32,
        new_card_params,
        new_card_bindings,
        new_string_bindings,
        report_lines,
    })
}

/// Public entry point for the "Import Model…" merge gesture
/// (`manifold-app`'s dispatch calls this — never [`merge_import_into_graph`]
/// directly, since that function takes a [`GltfImportSummary`], which is
/// `pub(crate)` to `manifold-renderer` and so cannot appear in a public
/// signature; the exact same constraint [`assemble_import_graph`] resolves
/// for [`build_import_graph`]). One CPU parse via
/// [`gltf_load::gltf_import_summary`], then the pure merge.
pub fn assemble_merge_plan(def: &EffectGraphDef, path: &Path) -> Result<MergePlan, String> {
    let summary = gltf_load::gltf_import_summary(path)?;
    merge_import_into_graph(def, &summary, path)
}
