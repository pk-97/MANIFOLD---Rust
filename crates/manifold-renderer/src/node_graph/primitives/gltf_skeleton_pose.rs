//! `node.gltf_skeleton_pose` — samples a parsed glTF skeleton's per-joint
//! keyframe tracks at a live `progress` (0..1) and emits the joint palette
//! (`Array(JointMatrix)`, one skin matrix per joint) `node.skin_mesh`
//! reads via a `BufferGather` lookup.
//!
//! GLTF_ANIMATION_DESIGN.md A2 (D2): CPU-side, content-thread, per-frame —
//! the same "sampling is graph-native, no parallel player subsystem"
//! doctrine `node.gltf_animation_source` (A1) already proves, extended
//! from 9 scalar outputs to an array of matrices because a joint palette
//! (up to ~256 joints for the spec-typical case) doesn't fit the
//! port-shadow-scalar shape.
//!
//! GLTF_ANIM_RUNTIME_V2_DESIGN.md P1: keyframe/topology payload no longer
//! lives in this node's Table params (the six Table params below stay
//! DECLARED for round-trip/migration but are no longer read — P2 deletes
//! them). Instead `path` + `skin_index` select an `Arc<GltfAnimSet>` from
//! `gltf_anim_cache`'s shared, file-backed, `Weak`-held cache, loaded once
//! per FILE on a background thread (the same mpsc pattern
//! `gltf_morph_deltas_source.rs` uses) and shared across every node/object
//! referencing that file. Sampling is a `partition_point` binary search
//! per channel (`gltf_anim_shared`'s slice samplers) instead of a linear
//! table scan.
//!
//! Per-frame algorithm (unchanged from A2, just re-sourced):
//! 1. Sample each joint's LOCAL translation/rotation/scale at the wrapped
//!    clip time `t` (falling back to its static BIND pose — read from
//!    `GltfAnimSet::node_bind_trs` — when the joint carries no animated
//!    channel in this clip — never the identity fallback A1's
//!    rigid-object sampler uses, because an unrigged joint's bind pose is
//!    frequently non-identity).
//! 2. Compose each joint's WORLD matrix by walking its parent chain
//!    (memoized, since `skin.joints()` order is not guaranteed
//!    parent-before-child per spec) — `parent[j] == -1` roots at
//!    `joint_root_world[j]` (the static ancestor-chain product ABOVE the
//!    joint tree, precomputed at parse time).
//! 3. `skin_matrix[j] = world[j] * inverse_bind_matrices[j]`.

use std::borrow::Cow;
use std::sync::{Arc, mpsc};

use super::gltf_anim_shared::{
    LOOP_MODES, LoopMode, TriggerLatch, clip_duration, resolve_progress, sample_quat_slice, sample_vec3_slice,
};
use crate::generators::mesh_common::JointMatrix;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::gltf_anim_cache::{AnimClip, AnimSetLookup, GltfAnimSet, get_or_spawn_load};
use crate::node_graph::gltf_load::{Mat4, MAT4_IDENTITY, mat4_from_trs, mat4_mul};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue, TableData};
use crate::node_graph::primitive::Primitive;

/// Maximum joints this primitive will pose in one frame — generous past
/// the spec-typical ≤256 (BrainStem is the documented stress case);
/// bounds the memoization scratch without a per-frame heap allocation of
/// unbounded size.
const MAX_JOINTS: usize = 512;

crate::primitive! {
    name: GltfSkeletonPose,
    type_id: "node.gltf_skeleton_pose",
    purpose: "Samples a parsed glTF skeleton's per-joint TRS keyframe tracks (or static bind pose, for an unanimated joint) at a live `progress` (0..1) and emits the joint palette as Array(JointMatrix) — one skin matrix (jointWorldMatrix * inverseBindMatrix) per joint, in skin.joints() order. Wire the output straight into node.skin_mesh's `matrices` input. LINEAR interpolation only (A1/A2 scope): lerp for translation/scale, slerp for rotation. `progress` port-shadowed with the same default beat-drive as node.gltf_animation_source: wrap(beats*rate/clip_beats), always wrapping into [0,1), never clamping.",
    inputs: {
        progress: ScalarF32 optional,
        clip_index: ScalarF32 optional,
        trigger_count: ScalarF32 optional,
    },
    outputs: {
        joint_matrices: Array(JointMatrix),
    },
    params: [
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md D1/P1: comes via
        // presetMetadata.stringBindings, same convention as
        // node.gltf_mesh_source's `path`. Selects the `Arc<GltfAnimSet>`
        // this node samples from `gltf_anim_cache`'s shared cache.
        ParamDef {
            name: Cow::Borrowed("path"),
            label: "File",
            ty: ParamType::String,
            default: ParamValue::Float(0.0), // String default supplied via stringBindings; this slot is never read.
            range: None,
            enum_values: &[],
        },
        // Which `document.skins()` entry (index into `GltfAnimSet::skins`)
        // this node poses.
        ParamDef {
            name: Cow::Borrowed("skin_index"),
            label: "Skin Index",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 63.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("duration_s"),
            label: "Duration (s)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.001, 3600.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rate"),
            label: "Rate (cycles/clip)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0625, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("clip_index"),
            label: "Clip",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 31.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("loop_mode"),
            label: "Loop Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (LOOP_MODES.len() - 1) as f32)),
            enum_values: LOOP_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("clip_durations"),
            label: "Clip Durations",
            ty: ParamType::Table,
            // Rows: [clip_index, duration_s]; sentinel means "use the
            // static duration_s param for every clip".
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("trigger_count"),
            label: "Retrigger",
            ty: ParamType::Trigger,
            // Port-shadowed by the same-named input; unwired, an outer-card
            // `is_trigger` button writes here directly. Trigger-typed (not
            // Int) or card validation rejects the import's Retrigger button.
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("joint_count"),
            label: "Joint Count",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, MAX_JOINTS as f32)),
            enum_values: &[],
        },
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md P1: these six Tables are NO LONGER
        // READ by `run()` — topology/track data now comes from the
        // `Arc<GltfAnimSet>` selected by `path`/`skin_index` above. Kept
        // DECLARED (not deleted) so an already-saved project round-trips
        // without panicking or silently dropping data; `gltf_import.rs`
        // still emits them this phase too (additive). P2 deletes both the
        // emission and the params, with a load-migration for old presets
        // (D5). Tables can't live in static-const ParamValue
        // (node.cycle_table_row's sentinel convention) — each defaults to
        // the Float(0.0) sentinel, read as "no rows" when something still
        // reads them (nothing does, post-P1).
        ParamDef {
            name: Cow::Borrowed("joint_parent_table"),
            label: "Joint Parent Table",
            ty: ParamType::Table,
            // Rows: [joint_index, parent_joint_index_or_-1].
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("joint_root_world_table"),
            label: "Joint Root World Table",
            ty: ParamType::Table,
            // Rows: [joint_index, m0..m15] (column-major 4x4), present
            // only for joints whose parent lies outside the joint tree.
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("inverse_bind_table"),
            label: "Inverse Bind Table",
            ty: ParamType::Table,
            // Rows: [joint_index, m0..m15], one per joint.
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("translation_tracks"),
            label: "Translation Tracks",
            ty: ParamType::Table,
            // Rows: [clip_index, joint_index, time_s, x, y, z] (A4:
            // clip_index prepended for D4 multi-clip selection), grouped
            // ascending by (clip_index, joint_index), ascending time within
            // a (clip, joint) block.
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rotation_tracks"),
            label: "Rotation Tracks",
            ty: ParamType::Table,
            // Rows: [clip_index, joint_index, time_s, x, y, z, w].
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_tracks"),
            label: "Scale Tracks",
            ty: ParamType::Table,
            // Rows: [clip_index, joint_index, time_s, x, y, z].
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "path comes via presetMetadata.stringBindings, same convention as node.gltf_mesh_source's `path`. skin_index selects which document.skins() entry (GltfAnimSet::skins[skin_index]) this node poses — gltf_import.rs stamps both from the object's resolved GltfObjectSkin. Wire `joint_matrices` into node.skin_mesh's `matrices` input (BufferGather — a joint-index lookup, not coincident with skin_mesh's per-vertex dispatch). Unwired `progress` follows the default beat-drive; wire node.lfo (Saw) for a performer-controlled loop.",
    examples: [],
    picker: { label: "glTF Skeleton Pose", category: Driver },
    summary: "Poses an imported glTF character's skeleton and outputs the joint matrices a Skin Mesh node needs to deform it. Wire progress to a beat or LFO to animate the pose.",
    category: Control,
    role: Source,
    aliases: ["skeleton pose", "joint palette", "skin pose", "rig pose"],
    boundary_reason: NonGpu,
    extra_fields: {
        trigger_latch: TriggerLatch = TriggerLatch::new(),
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md P1: last `path` a load was
        // triggered for (re-triggers on change, same key-gating shape
        // `gltf_mesh_source`'s `last_key` uses).
        last_path: String = String::new(),
        // Resident once loaded; `None` while unloaded/loading/failed.
        anim_set: Option<Arc<GltfAnimSet>> = None,
        // Background loader channel. `Some` means a load is in flight (or
        // was resolved from the shared cache without spawning a thread —
        // see `AnimSetLookup::Ready`); we don't spawn another until it
        // returns.
        pending_load: Option<mpsc::Receiver<Result<Arc<GltfAnimSet>, String>>> = None,
    },
}

fn table_or_empty(v: Option<&ParamValue>) -> Option<&TableData> {
    match v {
        Some(ParamValue::Table(t)) => Some(t.as_ref()),
        _ => None,
    }
}

/// Memoized world-matrix composition — `parent[j] == -1` roots at
/// `root_world[j]`; otherwise composes with the parent's own (recursively
/// resolved) world matrix. `depth_guard` refuses to recurse past
/// `MAX_JOINTS` — malformed cyclic parent data (never expected from a
/// real asset) resolves to identity rather than a stack overflow.
#[allow(clippy::too_many_arguments)]
fn resolve_world(
    j: usize,
    parent: &[i32],
    local: &[Mat4],
    root_world: &[Mat4],
    world: &mut [Option<Mat4>],
    depth: u32,
) -> Mat4 {
    if let Some(w) = world[j] {
        return w;
    }
    if depth > MAX_JOINTS as u32 {
        return MAT4_IDENTITY;
    }
    let p = parent[j];
    let w = if p < 0 || p as usize >= parent.len() {
        mat4_mul(&root_world[j], &local[j])
    } else {
        let pw = resolve_world(p as usize, parent, local, root_world, world, depth + 1);
        mat4_mul(&pw, &local[j])
    };
    world[j] = Some(w);
    w
}

/// Sample joint `node`'s LOCAL translation/rotation/scale at time `t`
/// within `clip`, falling back to `anim_set.node_bind_trs[node]` per
/// channel when `clip` carries no track for it — GLTF_ANIM_RUNTIME_V2_DESIGN.md
/// D3: `Some(channel)` runs the slice binary-search sampler,
/// `None` returns the bind value directly (a single static row, same as
/// A2's original mat4_from_table-identity-except-bind-pose behavior).
fn sample_joint_local(anim_set: &GltfAnimSet, clip: &AnimClip, node: u32, t: f32) -> Mat4 {
    let bind = anim_set.node_bind_trs.get(node as usize);
    let bind_t = bind.map(|b| b.translation).unwrap_or([0.0, 0.0, 0.0]);
    let bind_r = bind.map(|b| b.rotation).unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let bind_s = bind.map(|b| b.scale).unwrap_or([1.0, 1.0, 1.0]);

    let tr = match clip.translation_channel(node) {
        Some(c) => sample_vec3_slice(&c.times, &c.values, c.mode, &c.in_tangents, &c.out_tangents, t, bind_t),
        None => bind_t,
    };
    let rot = match clip.rotation_channel(node) {
        Some(c) => sample_quat_slice(&c.times, &c.values, c.mode, &c.in_tangents, &c.out_tangents, t),
        None => bind_r,
    };
    let sc = match clip.scale_channel(node) {
        Some(c) => sample_vec3_slice(&c.times, &c.values, c.mode, &c.in_tangents, &c.out_tangents, t, bind_s),
        None => bind_s,
    };
    mat4_from_trs(tr, rot, sc)
}

/// The full per-frame pose algorithm (GLTF_ANIM_RUNTIME_V2_DESIGN.md P1),
/// pure and `EffectNodeContext`-free so it's directly unit- and
/// perf-testable: sample every joint's local TRS from `anim_set`, compose
/// world matrices by walking the skin's own parent chain
/// ([`resolve_world`]), then `skin_matrix[j] = world[j] * inverse_bind[j]`.
/// Returns an empty `Vec` for an out-of-range `skin_index` or zero
/// `joint_count` — callers treat that as "nothing to write this frame",
/// same as the pre-cache behavior's `joint_count == 0` early return.
pub(crate) fn sample_skeleton_pose(
    anim_set: &GltfAnimSet,
    skin_index: usize,
    clip_index: usize,
    t: f32,
    joint_count: usize,
) -> Vec<JointMatrix> {
    let Some(skin) = anim_set.skins.get(skin_index) else { return Vec::new() };
    let n = joint_count.min(skin.joint_node_indices.len()).min(MAX_JOINTS);
    if n == 0 {
        return Vec::new();
    }
    let empty_clip;
    let clip = match anim_set.clips.get(clip_index) {
        Some(c) => c,
        None => {
            empty_clip = AnimClip { duration_s: 0.0, channels: Vec::new() };
            &empty_clip
        }
    };

    let mut parent = [-1i32; MAX_JOINTS];
    let mut root_world = [MAT4_IDENTITY; MAX_JOINTS];
    let mut inverse_bind = [MAT4_IDENTITY; MAX_JOINTS];
    let mut local = [MAT4_IDENTITY; MAX_JOINTS];
    let mut world: [Option<Mat4>; MAX_JOINTS] = [None; MAX_JOINTS];

    for j in 0..n {
        parent[j] = skin.joint_parent.get(j).copied().unwrap_or(-1);
        root_world[j] = skin.joint_root_world.get(j).copied().unwrap_or(MAT4_IDENTITY);
        inverse_bind[j] = skin.inverse_bind_matrices.get(j).copied().unwrap_or(MAT4_IDENTITY);
        local[j] = sample_joint_local(anim_set, clip, skin.joint_node_indices[j], t);
    }

    let mut skin_matrices = Vec::with_capacity(n);
    // Same three-independent-slices shape `resolve_world`'s own caller
    // used pre-cache — see that impl's identical comment.
    #[allow(clippy::needless_range_loop)]
    for j in 0..n {
        let w = resolve_world(j, &parent[..n], &local[..n], &root_world[..n], &mut world[..n], 0);
        let m = mat4_mul(&w, &inverse_bind[j]);
        skin_matrices.push(JointMatrix { c0: m[0], c1: m[1], c2: m[2], c3: m[3] });
    }
    skin_matrices
}

impl Primitive for GltfSkeletonPose {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "joint_matrices" {
            return None;
        }
        match params.get("joint_count") {
            Some(ParamValue::Float(f)) => Some(f.round().clamp(0.0, MAX_JOINTS as f32) as u32),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let duration_s = match ctx.params.get("duration_s") {
            Some(ParamValue::Float(f)) => f.max(1e-6),
            _ => 1.0,
        };
        let rate = match ctx.params.get("rate") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let loop_mode = LoopMode::from_enum_index(
            ctx.params.get("loop_mode").and_then(ParamValue::as_scalar).unwrap_or(0.0),
        );
        let clip_index = ctx
            .inputs
            .scalar("clip_index")
            .and_then(|v| v.as_scalar())
            .or_else(|| ctx.params.get("clip_index").and_then(ParamValue::as_scalar))
            .unwrap_or(0.0)
            .round()
            .max(0.0) as usize;

        let trigger_count = ctx
            .inputs
            .scalar("trigger_count")
            .and_then(|v| v.as_scalar())
            .or_else(|| ctx.params.get("trigger_count").and_then(ParamValue::as_scalar));
        self.trigger_latch.update(trigger_count, ctx.time.beats.0);

        let clip_durations = table_or_empty(ctx.params.get("clip_durations"));
        let duration_s = clip_duration(clip_durations, clip_index, duration_s);

        let wired_progress = ctx.inputs.scalar("progress").and_then(|v| v.as_scalar());
        let progress = resolve_progress(
            ctx.time,
            wired_progress,
            duration_s,
            rate,
            loop_mode,
            self.trigger_latch.origin_beats(),
        );
        let t = progress * duration_s;

        let joint_count = match ctx.params.get("joint_count") {
            Some(ParamValue::Float(f)) => (f.round().max(0.0) as usize).min(MAX_JOINTS),
            _ => 0,
        };
        if joint_count == 0 {
            return;
        }

        // GLTF_ANIM_RUNTIME_V2_DESIGN.md P1: `path`/`skin_index` select the
        // shared `Arc<GltfAnimSet>` instead of reading the six Table
        // params (declared above, no longer read).
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };
        let skin_index = match ctx.params.get("skin_index") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as usize,
            _ => 0,
        };

        if path != self.last_path {
            self.last_path = path.clone();
            self.anim_set = None;
            self.pending_load = None;
        }
        if self.anim_set.is_none() && self.pending_load.is_none() && !path.is_empty() {
            match get_or_spawn_load(std::path::Path::new(&path)) {
                AnimSetLookup::Ready(set) => self.anim_set = Some(set),
                AnimSetLookup::Pending(rx) => self.pending_load = Some(rx),
            }
        }
        if let Some(rx) = &self.pending_load {
            match rx.try_recv() {
                Ok(Ok(set)) => {
                    self.anim_set = Some(set);
                    self.pending_load = None;
                }
                Ok(Err(e)) => {
                    log::error!("node.gltf_skeleton_pose: {e}");
                    self.pending_load = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.gltf_skeleton_pose: background load channel disconnected");
                    self.pending_load = None;
                }
            }
        }

        // Nothing loaded yet (or the path is empty/failed) — leave the
        // pre-bound output buffer's existing contents, same "hold last
        // frame" convention `gltf_mesh_source`/`gltf_morph_deltas_source`
        // use while their own background parse is in flight.
        let Some(anim_set) = self.anim_set.clone() else {
            return;
        };
        let skin_matrices = sample_skeleton_pose(&anim_set, skin_index, clip_index, t, joint_count);
        if skin_matrices.is_empty() {
            return;
        }

        let Some(out_buf) = ctx.outputs.array("joint_matrices") else {
            return;
        };
        let capacity = (out_buf.size / std::mem::size_of::<JointMatrix>() as u64) as usize;
        let n = skin_matrices.len().min(capacity);
        if n == 0 {
            return;
        }
        // Safety: shared-memory MTLBuffer pre-bound by the chain build;
        // write count clamped to the buffer capacity; sequential executor
        // on the content thread means no concurrent writer.
        unsafe {
            out_buf.write(0, bytemuck::cast_slice(&skin_matrices[..n]));
        }
    }

    fn clear_state(&mut self) {
        self.trigger_latch.clear();
    }

    fn is_trigger_latch(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::gltf_anim_cache::{BindTrs, Channel, ChannelKind, SkinTopology};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_progress_input_and_joint_matrix_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        assert_eq!(GltfSkeletonPose::TYPE_ID, "node.gltf_skeleton_pose");
        assert_eq!(GltfSkeletonPose::INPUTS.len(), 3);
        assert_eq!(GltfSkeletonPose::INPUTS[0].name, "progress");
        assert!(!GltfSkeletonPose::INPUTS[0].required);
        assert_eq!(GltfSkeletonPose::INPUTS[0].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(GltfSkeletonPose::OUTPUTS.len(), 1);
        assert_eq!(GltfSkeletonPose::OUTPUTS[0].name, "joint_matrices");
        assert_eq!(GltfSkeletonPose::OUTPUTS[0].ty, PortType::Array(ArrayType::of_known::<JointMatrix>()));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GltfSkeletonPose::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gltf_skeleton_pose");
    }

    /// Two joints: joint 0 is the root (parent -1, identity root_world),
    /// joint 1 is its child. Joint 0's local translates by (10,0,0) at
    /// t=1; joint 1 is a static identity local (single-row track).
    /// `resolve_world` must compose joint 1's world as joint 0's world *
    /// joint 1's local — proving parent-chain composition, not just
    /// per-joint local sampling.
    #[test]
    fn resolve_world_composes_parent_chain() {
        let parent = [-1i32, 0i32];
        let mut local = [MAT4_IDENTITY; 2];
        local[0] = mat4_from_trs([10.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0]);
        local[1] = mat4_from_trs([0.0, 2.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0]);
        let root_world = [MAT4_IDENTITY; 2];
        let mut world: [Option<Mat4>; 2] = [None; 2];
        let w0 = resolve_world(0, &parent, &local, &root_world, &mut world, 0);
        let w1 = resolve_world(1, &parent, &local, &root_world, &mut world, 0);
        assert!((w0[3][0] - 10.0).abs() < 1e-5, "joint 0 world x = 10");
        assert!((w1[3][0] - 10.0).abs() < 1e-5, "joint 1 world x inherits joint 0's translation");
        assert!((w1[3][1] - 2.0).abs() < 1e-5, "joint 1 world y is its own local offset");
    }

    /// Full-pipeline value check without the graph/backend plumbing:
    /// world composed above, then `skin = world * inverse_bind`. With an
    /// identity inverse bind, the skin matrix's translation column equals
    /// the joint's own world translation.
    #[test]
    fn skin_matrix_is_world_times_inverse_bind() {
        let parent = [-1i32];
        let local = [mat4_from_trs([3.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0])];
        let root_world = [MAT4_IDENTITY];
        let mut world: [Option<Mat4>; 1] = [None];
        let w = resolve_world(0, &parent, &local, &root_world, &mut world, 0);
        let inverse_bind = mat4_from_trs([-3.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0]);
        let skin = mat4_mul(&w, &inverse_bind);
        // world translates +3, inverse_bind translates -3 in the SAME
        // (already-world) space composed AFTER world — net translation
        // should be world's rotation/scale applied to -3 then +3 world
        // offset: for pure translations this nets to 0.
        assert!((skin[3][0]).abs() < 1e-5, "world(+3) * inverse_bind(-3) nets to 0, got {}", skin[3][0]);
    }

    // ─── GLTF_ANIM_RUNTIME_V2_DESIGN.md P1 — cache-backed sampling ─────────

    fn translation_channel(node: u32, keys: &[(f32, [f32; 3])]) -> Channel {
        Channel {
            target_node: node,
            kind: ChannelKind::Translation,
            mode: crate::node_graph::gltf_load::GltfInterp::Linear,
            times: keys.iter().map(|(t, _)| *t).collect(),
            values: keys.iter().flat_map(|(_, v)| *v).collect(),
            in_tangents: Vec::new(),
            out_tangents: Vec::new(),
        }
    }

    #[test]
    fn sample_joint_local_falls_back_to_bind_trs_when_no_channel() {
        let anim_set = GltfAnimSet {
            clips: vec![AnimClip { duration_s: 1.0, channels: Vec::new() }],
            skins: Vec::new(),
            node_parents: Vec::new(),
            node_bind_trs: vec![BindTrs { translation: [1.0, 2.0, 3.0], rotation: [0.0, 0.0, 0.0, 1.0], scale: [1.0; 3] }],
        };
        let m = sample_joint_local(&anim_set, &anim_set.clips[0], 0, 0.5);
        assert_eq!(m[3][0], 1.0, "no channel for node 0 -> bind translation x");
        assert_eq!(m[3][1], 2.0);
        assert_eq!(m[3][2], 3.0);
    }

    #[test]
    fn sample_joint_local_samples_the_animated_channel_when_present() {
        let channel = translation_channel(0, &[(0.0, [0.0, 0.0, 0.0]), (1.0, [10.0, 0.0, 0.0])]);
        let anim_set = GltfAnimSet {
            clips: vec![AnimClip { duration_s: 1.0, channels: vec![channel] }],
            skins: Vec::new(),
            node_parents: Vec::new(),
            node_bind_trs: vec![BindTrs { translation: [99.0, 99.0, 99.0], rotation: [0.0, 0.0, 0.0, 1.0], scale: [1.0; 3] }],
        };
        let m = sample_joint_local(&anim_set, &anim_set.clips[0], 0, 0.5);
        assert!((m[3][0] - 5.0).abs() < 1e-4, "halfway lerp of the animated channel, not the bind pose, got {}", m[3][0]);
    }

    /// Two-joint parent chain (same shape as `resolve_world_composes_parent_chain`)
    /// exercised through the full [`sample_skeleton_pose`] entry point —
    /// proves the cache-backed path composes correctly end to end, not
    /// just its pieces.
    #[test]
    fn sample_skeleton_pose_composes_parent_chain_end_to_end() {
        let channel0 = translation_channel(0, &[(0.0, [0.0, 0.0, 0.0]), (1.0, [10.0, 0.0, 0.0])]);
        let anim_set = GltfAnimSet {
            clips: vec![AnimClip { duration_s: 1.0, channels: vec![channel0] }],
            skins: vec![SkinTopology {
                joint_node_indices: vec![0, 1],
                joint_parent: vec![-1, 0],
                joint_root_world: vec![MAT4_IDENTITY, MAT4_IDENTITY],
                inverse_bind_matrices: vec![MAT4_IDENTITY, MAT4_IDENTITY],
            }],
            node_parents: vec![-1, 0],
            node_bind_trs: vec![
                BindTrs { translation: [0.0; 3], rotation: [0.0, 0.0, 0.0, 1.0], scale: [1.0; 3] },
                BindTrs { translation: [0.0, 2.0, 0.0], rotation: [0.0, 0.0, 0.0, 1.0], scale: [1.0; 3] },
            ],
        };
        let matrices = sample_skeleton_pose(&anim_set, 0, 0, 1.0, 2);
        assert_eq!(matrices.len(), 2);
        assert!((matrices[0].c3[0] - 10.0).abs() < 1e-4, "joint 0 (animated) world x = 10");
        assert!((matrices[1].c3[0] - 10.0).abs() < 1e-4, "joint 1 inherits joint 0's translation");
        assert!((matrices[1].c3[1] - 2.0).abs() < 1e-4, "joint 1 keeps its own bind-pose y offset");
    }

    #[test]
    fn sample_skeleton_pose_returns_empty_for_out_of_range_skin() {
        let anim_set = GltfAnimSet {
            clips: Vec::new(),
            skins: Vec::new(),
            node_parents: Vec::new(),
            node_bind_trs: Vec::new(),
        };
        assert!(sample_skeleton_pose(&anim_set, 0, 0, 0.0, 10).is_empty());
    }

    /// P1's mandatory perf gate: a synthetic dragon-fixture-scale
    /// `GltfAnimSet` (52 clips x 630 channels x ~160 keys, 300 joints,
    /// GLTF_ANIM_RUNTIME_V2_DESIGN.md §3) sampled for one full pose must
    /// stay well under budget with the binary-search slice path — the
    /// exact O(rows) linear-scan cost class this design removes. Debug
    /// build asserts < 8ms (the doc's stated debug ceiling); release is
    /// far faster and isn't asserted here (measured, not gated, per the
    /// doc's own "release <1ms claim is documented not asserted").
    #[test]
    fn pose_sampling_dragon_scale_under_1ms() {
        const CLIP_COUNT: usize = 52;
        const CHANNELS_PER_CLIP: usize = 630;
        const KEYS_PER_CHANNEL: usize = 160;
        const JOINT_COUNT: usize = 300;

        // 630 channels spread across up to 300 joints, at most one
        // Translation channel per joint per clip (this perf test only
        // needs a realistic COUNT/lookup-cost shape, not real skeleton
        // topology).
        let mut clips = Vec::with_capacity(CLIP_COUNT);
        for _ in 0..CLIP_COUNT {
            let mut channels = Vec::with_capacity(CHANNELS_PER_CLIP);
            for c in 0..CHANNELS_PER_CLIP {
                let node = (c % JOINT_COUNT) as u32;
                let times: Vec<f32> = (0..KEYS_PER_CHANNEL).map(|k| k as f32 * 0.01).collect();
                let values: Vec<f32> = (0..KEYS_PER_CHANNEL * 3).map(|v| (v % 7) as f32 * 0.1).collect();
                channels.push(Channel {
                    target_node: node,
                    kind: ChannelKind::Translation,
                    mode: crate::node_graph::gltf_load::GltfInterp::Linear,
                    times,
                    values,
                    in_tangents: Vec::new(),
                    out_tangents: Vec::new(),
                });
            }
            channels.sort_by_key(|c| c.target_node);
            clips.push(AnimClip { duration_s: 1.6, channels });
        }

        let joint_node_indices: Vec<u32> = (0..JOINT_COUNT as u32).collect();
        // A shallow binary tree so parent-chain composition is exercised
        // (not every joint a root).
        let joint_parent: Vec<i32> =
            (0..JOINT_COUNT).map(|j| if j == 0 { -1 } else { ((j - 1) / 2) as i32 }).collect();
        let skin = SkinTopology {
            joint_node_indices,
            joint_parent,
            joint_root_world: vec![MAT4_IDENTITY; JOINT_COUNT],
            inverse_bind_matrices: vec![MAT4_IDENTITY; JOINT_COUNT],
        };
        let node_bind_trs =
            vec![BindTrs { translation: [0.0; 3], rotation: [0.0, 0.0, 0.0, 1.0], scale: [1.0; 3] }; JOINT_COUNT];

        let anim_set =
            GltfAnimSet { clips, skins: vec![skin], node_parents: Vec::new(), node_bind_trs };

        // Warm the branch predictor / allocator once, then measure a
        // single pose sample (the per-frame cost this gate bounds).
        let _ = sample_skeleton_pose(&anim_set, 0, 0, 0.8, JOINT_COUNT);
        let start = std::time::Instant::now();
        let matrices = sample_skeleton_pose(&anim_set, 0, 0, 0.8, JOINT_COUNT);
        let elapsed = start.elapsed();

        assert_eq!(matrices.len(), JOINT_COUNT);
        assert!(
            elapsed.as_secs_f64() * 1000.0 < 8.0,
            "one full dragon-scale pose sample took {:.3}ms, budget is 8ms (debug)",
            elapsed.as_secs_f64() * 1000.0
        );
    }
}
