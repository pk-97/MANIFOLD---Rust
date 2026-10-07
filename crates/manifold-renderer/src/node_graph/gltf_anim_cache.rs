//! GLTF_ANIM_RUNTIME_V2_DESIGN.md D1/D2 — the shared, file-backed,
//! `Weak`-held animation cache. Keyframe payload no longer lives in any
//! graph def's `ParamValue::Table`; instead every glTF sampler primitive
//! (`node.gltf_skeleton_pose` first, P1; the other two samplers follow in
//! P2) holds an `Arc<GltfAnimSet>` loaded once per FILE and shared across
//! every node/object/layer that references it. `Weak`-holding the cache
//! entry means the last `Arc` dropping (layer deleted, preset unloaded)
//! frees the payload — the delete-recovers-memory property is structural,
//! not a cleanup pass.
//!
//! `ANIM_CACHE` is this design's one approved piece of new shared state
//! (CLAUDE.md "No new shared state" — approved by
//! GLTF_ANIM_RUNTIME_V2_DESIGN.md D2): a coarse `Mutex` around a tiny map,
//! touched only at load/drop, never per-frame. Loads happen on a spawned
//! thread, the same `mpsc` background-load pattern
//! `gltf_morph_deltas_source.rs:96` already proves — this module reuses it
//! rather than inventing a second one.
//!
//! Parsing itself is NOT reimplemented here: [`load_anim_set`] calls
//! straight into `gltf_load.rs`'s existing `parse_buffer_snapshot`/
//! `parse_animations`/`parse_skins`/`build_parent_map` — the same parse
//! `gltf_import.rs` uses to build the (still-emitted-this-phase) Table
//! params. One parse entry point, two destinations.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, Weak, mpsc};

use super::gltf_load::{self, GltfInterp, Mat4};

pub(crate) type LoadedAnimSet = manifold_node_engine::scene::source_asset::LoadedAsset<GltfAnimSet>;

impl From<GltfAnimSet> for LoadedAnimSet {
    fn from(set: GltfAnimSet) -> Self {
        let identity = super::gltf_anim_identity::identity(&set);
        Self::new(set, identity)
    }
}

/// One node's static bind-pose local TRS (`node.transform().decomposed()`),
/// indexed by glTF node index across the WHOLE scene (not just skin
/// joints) — the fallback [`Channel`] sampling uses for any node a clip
/// doesn't touch, and D4 (P3)'s node-slot rigid-object composition will
/// read directly for non-jointed nodes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BindTrs {
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

/// One glTF `skins[]` entry's parse-time-static topology — the same data
/// `GltfSkinInfo` carries, minus the per-joint bind TRS (now read from
/// [`GltfAnimSet::node_bind_trs`] via `joint_node_indices[j]`, so it's
/// stored exactly once per file instead of once per skin).
#[derive(Debug, Clone)]
pub struct SkinTopology {
    /// Palette index -> scene node index (the order `JOINTS_0` values
    /// index into).
    pub joint_node_indices: Vec<u32>,
    /// Index (into THIS skin's own joint list) of each joint's parent, or
    /// `-1` when the joint's real scene-graph parent is not itself a
    /// joint in this skin.
    pub joint_parent: Vec<i32>,
    /// Static world transform of the node chain ABOVE the joint tree —
    /// identity for joints whose parent lies within the joint tree. Since
    /// BUG-209's fix this is a parse-time FACT kept for reference/migration
    /// only: `gltf_skeleton_pose` roots joint worlds in the whole-scene
    /// animated walk (`resolve_world_whole_scene`), which equals this value
    /// exactly when no ancestor is animated.
    pub joint_root_world: Vec<Mat4>,
    pub inverse_bind_matrices: Vec<Mat4>,
}

/// Which TRS/weights component a [`Channel`] carries. `Weights` carries
/// its target count since the value stride (unlike Translation/Scale's
/// fixed 3 or Rotation's fixed 4) depends on the mesh's morph target
/// count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Translation,
    Rotation,
    Scale,
    Weights { target_count: u32 },
}

impl ChannelKind {
    fn stride(self) -> usize {
        match self {
            ChannelKind::Translation | ChannelKind::Scale => 3,
            ChannelKind::Rotation => 4,
            ChannelKind::Weights { target_count } => target_count as usize,
        }
    }
}

/// One keyframe track, flattened out of `gltf_load`'s `Vec3Track`/
/// `QuatTrack`/`WeightsTrack` into contiguous slices — D3: every sampler
/// lookup against this is a `partition_point` binary search over `times`,
/// never a linear scan. `values` is flat SoA (`times.len() * kind.stride()`
/// elements); `in_tangents`/`out_tangents` mirror `values`' shape and are
/// non-empty only when `mode == CubicSpline` (glTF's own per-keyframe
/// in/out tangent triple, needed by `gltf_anim_shared`'s Hermite sampler).
#[derive(Debug, Clone)]
pub struct Channel {
    pub target_node: u32,
    pub kind: ChannelKind,
    pub mode: GltfInterp,
    pub times: Vec<f32>,
    pub values: Vec<f32>,
    pub in_tangents: Vec<f32>,
    pub out_tangents: Vec<f32>,
}

impl Channel {
    fn from_vec3(target_node: u32, kind: ChannelKind, track: &gltf_load::Vec3Track) -> Self {
        let stride = kind.stride();
        let mut values = Vec::with_capacity(track.values.len() * stride);
        let mut in_tangents = Vec::new();
        let mut out_tangents = Vec::new();
        let cubic = track.mode == GltfInterp::CubicSpline;
        if cubic {
            in_tangents.reserve(track.in_tangents.len() * stride);
            out_tangents.reserve(track.out_tangents.len() * stride);
        }
        for i in 0..track.values.len() {
            values.extend_from_slice(&track.values[i]);
            if cubic {
                in_tangents.extend_from_slice(&track.in_tangents[i]);
                out_tangents.extend_from_slice(&track.out_tangents[i]);
            }
        }
        Channel { target_node, kind, mode: track.mode, times: track.times.clone(), values, in_tangents, out_tangents }
    }

    fn from_quat(target_node: u32, track: &gltf_load::QuatTrack) -> Self {
        let kind = ChannelKind::Rotation;
        let stride = kind.stride();
        let mut values = Vec::with_capacity(track.values.len() * stride);
        let mut in_tangents = Vec::new();
        let mut out_tangents = Vec::new();
        let cubic = track.mode == GltfInterp::CubicSpline;
        if cubic {
            in_tangents.reserve(track.in_tangents.len() * stride);
            out_tangents.reserve(track.out_tangents.len() * stride);
        }
        for i in 0..track.values.len() {
            values.extend_from_slice(&track.values[i]);
            if cubic {
                in_tangents.extend_from_slice(&track.in_tangents[i]);
                out_tangents.extend_from_slice(&track.out_tangents[i]);
            }
        }
        Channel { target_node, kind, mode: track.mode, times: track.times.clone(), values, in_tangents, out_tangents }
    }

    /// Morph-weight channels are LINEAR-only (GLTF_ANIMATION_DESIGN.md A3,
    /// unchanged by this design) — never CubicSpline, so no tangent data.
    fn from_weights(target_node: u32, track: &gltf_load::WeightsTrack, target_count: u32) -> Self {
        let mut values = Vec::with_capacity(track.values.len() * target_count as usize);
        for row in &track.values {
            values.extend_from_slice(row);
        }
        Channel {
            target_node,
            kind: ChannelKind::Weights { target_count },
            mode: GltfInterp::Linear,
            times: track.times.clone(),
            values,
            in_tangents: Vec::new(),
            out_tangents: Vec::new(),
        }
    }
}

/// One `document.animations()` entry — index == glTF `animations[]` index.
#[derive(Debug, Clone)]
pub struct AnimClip {
    /// GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: read by all three samplers
    /// (`node.gltf_animation_source`/`node.gltf_skeleton_pose`/
    /// `node.gltf_morph_weights`) once an `Arc<GltfAnimSet>` is resident —
    /// wins over the `clip_durations` Table param, which stays declared
    /// (D5 back-compat / pre-load fallback) but is no longer the primary
    /// source once a path resolves.
    pub duration_s: f32,
    /// Sorted ascending by `target_node` (channels for the same node stay
    /// adjacent, in parse order among themselves — at most one
    /// Translation/Rotation/Scale/Weights each per node).
    pub channels: Vec<Channel>,
}

impl AnimClip {
    fn channels_for_node(&self, node: u32) -> &[Channel] {
        let start = self.channels.partition_point(|c| c.target_node < node);
        let rel_end = self.channels[start..].partition_point(|c| c.target_node == node);
        &self.channels[start..start + rel_end]
    }

    pub(crate) fn translation_channel(&self, node: u32) -> Option<&Channel> {
        self.channels_for_node(node).iter().find(|c| c.kind == ChannelKind::Translation)
    }

    pub(crate) fn rotation_channel(&self, node: u32) -> Option<&Channel> {
        self.channels_for_node(node).iter().find(|c| c.kind == ChannelKind::Rotation)
    }

    pub(crate) fn scale_channel(&self, node: u32) -> Option<&Channel> {
        self.channels_for_node(node).iter().find(|c| c.kind == ChannelKind::Scale)
    }

    /// GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: `node.gltf_morph_weights`' cache
    /// lookup — a `Weights` channel is keyed by the SAME `target_node`
    /// convention as TRS channels (the mesh-owning node), matched
    /// regardless of `target_count` (the caller checks that separately).
    pub(crate) fn weights_channel(&self, node: u32) -> Option<&Channel> {
        self.channels_for_node(node).iter().find(|c| matches!(c.kind, ChannelKind::Weights { .. }))
    }
}

/// One glTF file's whole parsed animation payload — one per FILE (not per
/// node, not per object), immutable after load, `Arc`-shared across every
/// primitive that references the same path.
#[derive(Debug, Clone)]
pub struct GltfAnimSet {
    /// Index == `document.animations()` index.
    pub clips: Vec<AnimClip>,
    /// Index == `document.skins()` index.
    pub skins: Vec<SkinTopology>,
    /// Whole-scene node hierarchy, index == glTF node index; `-1` = root.
    /// GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3): read by
    /// `gltf_skeleton_pose::resolve_world_whole_scene` — the node-slot
    /// rigid palette's whole-hierarchy parent-chain composition, which has
    /// no per-skin joint list to walk instead (unlike `SkinTopology::joint_parent`,
    /// used only within a skin's own joint list).
    pub node_parents: Vec<i32>,
    /// Whole-scene per-node bind TRS, index == glTF node index.
    pub node_bind_trs: Vec<BindTrs>,
}

/// Validate the current document and every resolved buffer before cache
/// lookup. A still-resident payload must not hide same-path replacement or
/// missing dependencies when a node prepares its source again. File IO stays
/// on the loader thread; running nodes retain their immutable loaded snapshot.
fn load_anim_set(path: &Path) -> Result<Arc<LoadedAnimSet>, String> {
    let snapshot = gltf_load::parse_buffer_snapshot(path)?;
    if let Some(set) = cached(&snapshot.identity) {
        return Ok(set);
    }
    let identity = snapshot.identity;
    Ok(insert_cache(identity, Arc::new(decode_anim_set(snapshot).into())))
}

/// Parse the exact snapshot that produced the cache key, without reopening
/// its files or maintaining another document/buffer parser.
fn decode_anim_set(snapshot: gltf_load::GltfBufferSnapshot) -> GltfAnimSet {
    let gltf_load::GltfBufferSnapshot { document, buffers, .. } = snapshot;

    let node_parents: Vec<i32> = gltf_load::build_parent_map(&document)
        .iter()
        .map(|p| p.map(|i| i as i32).unwrap_or(-1))
        .collect();
    let node_bind_trs: Vec<BindTrs> = document
        .nodes()
        .map(|n| {
            let (translation, rotation, scale) = n.transform().decomposed();
            BindTrs { translation, rotation, scale }
        })
        .collect();

    let clips: Vec<AnimClip> = gltf_load::parse_animations(&document, &buffers)
        .into_iter()
        .map(|info| {
            let mut channels = Vec::new();
            for na in &info.nodes {
                let node = na.node_index as u32;
                if let Some(t) = &na.translation {
                    channels.push(Channel::from_vec3(node, ChannelKind::Translation, t));
                }
                if let Some(r) = &na.rotation {
                    channels.push(Channel::from_quat(node, r));
                }
                if let Some(s) = &na.scale {
                    channels.push(Channel::from_vec3(node, ChannelKind::Scale, s));
                }
                if let Some(w) = &na.weights {
                    let target_count = w.values.first().map(|v| v.len()).unwrap_or(0) as u32;
                    if target_count > 0 {
                        channels.push(Channel::from_weights(node, w, target_count));
                    }
                }
            }
            // `info.nodes` is already node-index ascending
            // (`gltf_load::parse_animations` collects a `BTreeMap`'s
            // values), so this is a defensive no-op in the common case,
            // not a required re-sort.
            channels.sort_by_key(|c| c.target_node);
            let duration_s = channels
                .iter()
                .map(|c| c.times.last().copied().unwrap_or(0.0))
                .fold(0.0f32, f32::max)
                .max(1e-3);
            AnimClip { duration_s, channels }
        })
        .collect();

    let skins: Vec<SkinTopology> = gltf_load::parse_skins(&document, &buffers)
        .into_iter()
        .map(|s| SkinTopology {
            joint_node_indices: s.joint_node_indices.iter().map(|&i| i as u32).collect(),
            joint_parent: s.joint_parent,
            joint_root_world: s.joint_root_world,
            inverse_bind_matrices: s.inverse_bind_matrices,
        })
        .collect();

    GltfAnimSet { clips, skins, node_parents, node_bind_trs }
}

/// Reuse the existing approved cache and lock. Keys cover primary and
/// resolved buffer contents, and Weak entries never own animation payloads.
/// Only loader threads touch the map; no file IO or decoding holds its lock.
static ANIM_CACHE: LazyLock<Mutex<HashMap<[u8; 32], Weak<LoadedAnimSet>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached(identity: &[u8; 32]) -> Option<Arc<LoadedAnimSet>> {
    let cache = ANIM_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.get(identity).and_then(Weak::upgrade)
}

fn insert_cache(identity: [u8; 32], set: Arc<LoadedAnimSet>) -> Arc<LoadedAnimSet> {
    let mut cache = ANIM_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // Concurrent loads may decode the same snapshot. Publish one shared Arc
    // even when both missed the first lookup; an older load cannot overwrite
    // a changed file because its contents have a different key.
    if let Some(resident) = cache.get(&identity).and_then(Weak::upgrade) {
        return resident;
    }
    cache.retain(|_, entry| entry.strong_count() != 0);
    cache.insert(identity, Arc::downgrade(&set));
    set
}

/// Begin the existing key-change load/poll lifecycle. Every new request
/// validates source bytes on its background thread, including warm cache hits.
/// Nodes keep the resulting Arc, so ordinary frame sampling does no IO.
pub(crate) fn spawn_load(path: &Path) -> mpsc::Receiver<Result<Arc<LoadedAnimSet>, String>> {
    let path_buf = path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(load_anim_set(&path_buf));
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_channel(target_node: u32, key_count: usize) -> Channel {
        let times: Vec<f32> = (0..key_count).map(|i| i as f32 * 0.1).collect();
        let values: Vec<f32> = (0..key_count * 3).map(|i| i as f32).collect();
        Channel {
            target_node,
            kind: ChannelKind::Translation,
            mode: GltfInterp::Linear,
            times,
            values,
            in_tangents: Vec::new(),
            out_tangents: Vec::new(),
        }
    }

    #[test]
    fn clip_channel_lookup_finds_the_grouped_node_slice() {
        let clip = AnimClip {
            duration_s: 1.0,
            channels: vec![synthetic_channel(0, 4), synthetic_channel(2, 4), synthetic_channel(5, 4)],
        };
        assert!(clip.translation_channel(0).is_some());
        assert!(clip.translation_channel(2).is_some());
        assert!(clip.translation_channel(5).is_some());
        assert!(clip.translation_channel(1).is_none(), "no channel authored for node 1");
        assert!(clip.rotation_channel(0).is_none(), "node 0 has no rotation channel");
    }

    /// D2's structural property: the last `Arc<GltfAnimSet>` dropping
    /// frees the cache entry (delete-a-layer-frees-memory), because the
    /// cache holds only a `Weak`.
    #[test]
    fn anim_cache_drops_when_last_arc_drops() {
        let identity = [0xA5; 32];
        let set = Arc::new(LoadedAnimSet::from(GltfAnimSet {
            clips: Vec::new(),
            skins: Vec::new(),
            node_parents: Vec::new(),
            node_bind_trs: Vec::new(),
        }));
        let set = insert_cache(identity, set);
        assert!(cached(&identity).is_some(), "cache hit while the Arc is alive");
        drop(set);
        assert!(cached(&identity).is_none(), "Weak::upgrade() must be None once every Arc has dropped");
    }
}

#[cfg(test)]
mod dependency_tests {
    include!("gltf_anim_cache_tests.rs");
}
