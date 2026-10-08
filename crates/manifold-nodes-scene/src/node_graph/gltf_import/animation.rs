//! Animation topology helpers: the tiny per-clip duration / static-weight
//! rows the importer stamps as pre-load fallbacks (the shared
//! `gltf_anim_cache` supplies the real payload once loaded).

use crate::node_graph::gltf_load;

/// GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: replaces the P1-era
/// `build_skeleton_pose_tables`, which built six flat keyframe/topology
/// Tables — payload now lives entirely in the shared `gltf_anim_cache`
/// (loaded from `path`), so the importer only needs the tiny
/// `clip_durations` rows (`[clip_index, duration_s]`, D1: small enough to
/// stay a graph-def param) plus a fallback `duration_s` (clip 0's, or
/// `1e-3` if even that has no animated joints — the primitive's own
/// zero-guard floor). `node_anims_by_clip` empty (a skin with zero
/// animation clips in the whole asset) is treated as one implicit static
/// clip 0.
pub(super) fn skeleton_pose_clip_durations(
    skin: &gltf_load::GltfSkinInfo,
    node_anims_by_clip: &[std::collections::BTreeMap<usize, gltf_load::GltfNodeAnimation>],
) -> (Vec<Vec<f32>>, f32) {
    let n = skin.joint_node_indices.len();
    let empty_anims = std::collections::BTreeMap::new();
    let clip_count = node_anims_by_clip.len().max(1);
    let mut clip_durations_rows = Vec::with_capacity(clip_count);
    let mut fallback_duration_s = 1e-3;

    for c in 0..clip_count {
        let node_anims = node_anims_by_clip.get(c).unwrap_or(&empty_anims);
        let mut duration_s: f32 = 0.0;
        for j in 0..n {
            let Some(anim) = node_anims.get(&skin.joint_node_indices[j]) else { continue };
            let last = |t: &[f32]| t.last().copied().unwrap_or(0.0);
            duration_s = duration_s
                .max(anim.translation.as_ref().map(|t| last(&t.times)).unwrap_or(0.0))
                .max(anim.rotation.as_ref().map(|r| last(&r.times)).unwrap_or(0.0))
                .max(anim.scale.as_ref().map(|s| last(&s.times)).unwrap_or(0.0));
        }
        let duration_s = duration_s.max(1e-3);
        clip_durations_rows.push(vec![c as f32, duration_s]);
        if c == 0 {
            fallback_duration_s = duration_s;
        }
    }

    (clip_durations_rows, fallback_duration_s)
}

/// GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3): [`skeleton_pose_clip_durations`]'s
/// sibling for the node-slot rigid palette — same shape (`[clip_index,
/// duration_s]` rows + a fallback).
///
/// Invariant: each row equals the runtime `AnimClip::duration_s` (the last
/// keyframe of EVERY channel in the clip), because the pose node samples the
/// whole hierarchy — ancestors included — on that clock once the shared cache
/// loads. Counting only the slot nodes' own channels stamped a shorter clip
/// whenever an ancestor animated longer (BoxAnimated: node 2's rotation ends
/// at 2.5s, the node 0 translation it rides on ends at 3.71s), so the card,
/// the pre-load fallback, and every reader of `duration_s` ran on a different
/// clock from the sampler.
pub(super) fn rigid_multi_node_clip_durations(
    node_anims_by_clip: &[std::collections::BTreeMap<usize, gltf_load::GltfNodeAnimation>],
) -> (Vec<Vec<f32>>, f32) {
    let empty_anims = std::collections::BTreeMap::new();
    let clip_count = node_anims_by_clip.len().max(1);
    let mut clip_durations_rows = Vec::with_capacity(clip_count);
    let mut fallback_duration_s = 1e-3;

    for c in 0..clip_count {
        let node_anims = node_anims_by_clip.get(c).unwrap_or(&empty_anims);
        let mut duration_s: f32 = 0.0;
        for anim in node_anims.values() {
            let last = |t: &[f32]| t.last().copied().unwrap_or(0.0);
            duration_s = duration_s
                .max(anim.translation.as_ref().map(|t| last(&t.times)).unwrap_or(0.0))
                .max(anim.rotation.as_ref().map(|r| last(&r.times)).unwrap_or(0.0))
                .max(anim.scale.as_ref().map(|s| last(&s.times)).unwrap_or(0.0))
                .max(anim.weights.as_ref().map(|w| last(&w.times)).unwrap_or(0.0));
        }
        let duration_s = duration_s.max(1e-3);
        clip_durations_rows.push(vec![c as f32, duration_s]);
        if c == 0 {
            fallback_duration_s = duration_s;
        }
    }

    (clip_durations_rows, fallback_duration_s)
}

/// GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: replaces the P1-era
/// `build_morph_weight_table`, which built a `weight_tracks` keyframe
/// Table — payload now lives in the shared `gltf_anim_cache`. The importer
/// only needs `static_weights` (D1: O(target_count) topology, the tiny
/// `[target_index, weight]` fallback `node.gltf_morph_weights` uses for a
/// target the resolved clip's Weights channel doesn't cover — never a
/// silent 0.0, `MorphPrimitivesTest.glb`'s `mesh.weights = [0.5]` is the
/// documented case this guards) and `clip_durations` (tiny, D1). Returns
/// `(static_weights_rows, clip_durations_rows, fallback_duration_s)`.
/// `node_anims_by_clip` empty is treated as one implicit static clip 0
/// (mirrors `skeleton_pose_clip_durations`).
pub(super) fn morph_weights_topology(
    morph: &gltf_load::GltfObjectMorph,
    node_anims_by_clip: &[std::collections::BTreeMap<usize, gltf_load::GltfNodeAnimation>],
) -> (Vec<Vec<f32>>, Vec<Vec<f32>>, f32) {
    let n = morph.target_count as usize;
    let static_weights_rows: Vec<Vec<f32>> =
        (0..n).map(|i| vec![i as f32, morph.static_weights.get(i).copied().unwrap_or(0.0)]).collect();

    let empty_anims = std::collections::BTreeMap::new();
    let clip_count = node_anims_by_clip.len().max(1);
    let mut clip_durations_rows = Vec::with_capacity(clip_count);
    let mut fallback_duration_s = 1e-3;
    for c in 0..clip_count {
        let node_anims = node_anims_by_clip.get(c).unwrap_or(&empty_anims);
        let track = node_anims.get(&morph.mesh_node_index).and_then(|a| a.weights.as_ref());
        let duration_s = track
            .and_then(|t| t.times.last().copied())
            .unwrap_or(0.0)
            .max(1e-3);
        clip_durations_rows.push(vec![c as f32, duration_s]);
        if c == 0 {
            fallback_duration_s = duration_s;
        }
    }

    (static_weights_rows, clip_durations_rows, fallback_duration_s)
}
