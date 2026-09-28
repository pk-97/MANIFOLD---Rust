use super::*;

/// Recursively find the largest node `id` anywhere in `nodes`, including
/// inside group bodies. Node ids only need to be unique WITHIN the level
/// (`Vec<EffectGraphNode>`) that holds them — `descend_level` looks a group
/// id up in its own sibling list, and the flattener assigns every node a
/// brand-new global id at load time (`manifold_core::flatten::flatten_groups`
/// — `clone.id = new_id`) — so a merge only strictly needs to avoid
/// colliding with the TOP-LEVEL ids `render_scene`'s siblings use. Walking
/// every nesting level anyway costs nothing and is the simplest thing that
/// is obviously correct for every level at once.
pub(in super::super) fn max_node_id_recursive(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|n| {
            let inner = n.group.as_ref().map(|g| max_node_id_recursive(&g.nodes)).unwrap_or(0);
            n.id.max(inner)
        })
        .max()
        .unwrap_or(0)
}

/// BUG-w5wv: `build_object_group`'s `local_k` argument numbers ONE
/// object's own inner STRING handles ("mat_{k}", "mesh_{k}",
/// "transform_{k}", …) — a SEPARATE identifier system from the numeric
/// `EffectGraphNode.id` [`max_node_id_recursive`] already guards. Those
/// numeric ids only need to be unique per-level and get reassigned fresh at
/// flatten time (this file's own doc comment above), but the STRING handles
/// do NOT get renamed anywhere downstream — `check_card_lints` and
/// `loaded_preset_view::collect_node_handles` both resolve a card binding's
/// `NodeId` against a GLOBAL map keyed by that bare string, built by
/// walking every group's body. A fresh import's own `local_k` always starts
/// at 0 (`build_import_graph`), so merging a SECOND asset whose `local_k`
/// ALSO starts at 0 collides with the target's own "mat_0"/"mesh_0"/… —
/// silently harmless while every colliding node was identically
/// `node.pbr_material` (a binding resolving to the "wrong" one of two
/// functionally-identical nodes was never observable), now a real
/// misresolution once `node.unlit_material` makes two same-named nodes
/// genuinely different types.
///
/// Every object group unconditionally carries exactly one `mat_{k}` node
/// (every material with geometry gets a material node, pbr or unlit) at
/// this same `k` its `mesh_{k}`/`transform_{k}`/… siblings share by
/// construction (`build_object_group` takes ONE `local_k` for all of an
/// object's inner handles) — so scanning for the `mat_` prefix alone finds
/// every existing object's `k`, recursively through group bodies exactly
/// like [`max_node_id_recursive`]. `None` when the target has no glTF-
/// imported object in its history at all (a hand-built scene, or the
/// target's very first merge).
pub(super) fn max_local_k_recursive(nodes: &[EffectGraphNode]) -> Option<u32> {
    nodes
        .iter()
        .filter_map(|n| {
            let own = n.handle.as_deref().and_then(|h| h.strip_prefix("mat_")).and_then(|k| k.parse::<u32>().ok());
            let inner = n.group.as_ref().and_then(|g| max_local_k_recursive(&g.nodes));
            match (own, inner) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            }
        })
        .fold(None, |acc, v| Some(acc.map_or(v, |a: u32| a.max(v))))
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
pub(super) fn max_known_source_bbox_radius(nodes: &[EffectGraphNode]) -> Option<f32> {
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
