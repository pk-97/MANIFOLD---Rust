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
//! port-shadow-scalar shape. Reuses the SAME LINEAR lerp/slerp sampler
//! math as A1 (binary-search + lerp/slerp per keyframe pair), just against
//! per-joint row RANGES inside four flat Tables instead of one Table per
//! channel — `gltf_import.rs` builds those Tables sorted ascending by
//! `joint_index` so this primitive can find each joint's range with one
//! linear scan per frame (cheap: a scan over a few hundred to a few
//! thousand rows, not a per-vertex cost).
//!
//! Per-frame algorithm:
//! 1. Sample each joint's LOCAL translation/rotation/scale at the wrapped
//!    clip time `t` (falling back to its static BIND pose when the joint
//!    carries no animated channel — never the identity fallback A1's
//!    rigid-object sampler uses, because an unrigged joint's bind pose is
//!    frequently non-identity).
//! 2. Compose each joint's WORLD matrix by walking its parent chain
//!    (memoized, since `skin.joints()` order is not guaranteed
//!    parent-before-child per spec) — `parent[j] == -1` roots at
//!    `joint_root_world[j]` (the static ancestor-chain product ABOVE the
//!    joint tree, precomputed at import time).
//! 3. `skin_matrix[j] = world[j] * inverse_bind_matrices[j]`.

use std::borrow::Cow;

use crate::generators::mesh_common::JointMatrix;
use crate::node_graph::effect_node::{EffectNodeContext, FrameTime};
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
    },
    outputs: {
        joint_matrices: Array(JointMatrix),
    },
    params: [
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
            name: Cow::Borrowed("joint_count"),
            label: "Joint Count",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, MAX_JOINTS as f32)),
            enum_values: &[],
        },
        // Tables can't live in static-const ParamValue (node.cycle_table_row's
        // sentinel convention) — each defaults to the Float(0.0) sentinel,
        // read as "no rows" below.
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
            // Rows: [joint_index, time_s, x, y, z], grouped ascending by
            // joint_index, ascending time within a joint.
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rotation_tracks"),
            label: "Rotation Tracks",
            ty: ParamType::Table,
            // Rows: [joint_index, time_s, x, y, z, w].
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_tracks"),
            label: "Scale Tracks",
            ty: ParamType::Table,
            // Rows: [joint_index, time_s, x, y, z].
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    composition_notes: "gltf_import.rs builds all six Tables at import time from GltfMaterialInfo::skin, sorted ascending by joint_index. Wire `joint_matrices` into node.skin_mesh's `matrices` input (BufferGather — a joint-index lookup, not coincident with skin_mesh's per-vertex dispatch). Unwired `progress` follows the default beat-drive; wire node.lfo (Saw) for a performer-controlled loop.",
    examples: [],
    picker: { label: "glTF Skeleton Pose", category: Driver },
    summary: "Poses an imported glTF character's skeleton and outputs the joint matrices a Skin Mesh node needs to deform it. Wire progress to a beat or LFO to animate the pose.",
    category: Control,
    role: Source,
    aliases: ["skeleton pose", "joint palette", "skin pose", "rig pose"],
    boundary_reason: NonGpu,
}

/// Same default-beat-drive formula as `gltf_animation_source::default_progress`
/// (D3) — duplicated rather than shared across two small CPU primitives
/// with no other coupling; both are independently gate-tested against the
/// identical formula.
fn default_progress(time: FrameTime, duration_s: f32, rate: f32) -> f32 {
    let beats = time.beats.0 as f32;
    let seconds = time.seconds.0 as f32;
    let beats_per_second = if seconds.abs() > 1e-6 { beats / seconds } else { 2.0 };
    let clip_beats = (duration_s * beats_per_second).max(1e-6);
    let raw = beats * rate / clip_beats;
    raw.rem_euclid(1.0)
}

fn table_or_empty(v: Option<&ParamValue>) -> Option<&TableData> {
    match v {
        Some(ParamValue::Table(t)) => Some(t.as_ref()),
        _ => None,
    }
}

/// Row range `[start, end)` within `table` whose leading `joint_index`
/// column equals `joint`, assuming rows are grouped ascending by that
/// column (the `gltf_import.rs` emission contract). One linear scan per
/// call; `table` has at most a few thousand rows across all joints for
/// the documented stress case (BrainStem), so a handful of these scans
/// per frame stays well inside the 20ms hot-path budget.
fn joint_row_range(table: Option<&TableData>, joint: usize) -> (usize, usize) {
    let Some(table) = table else { return (0, 0) };
    let n = table.row_count();
    let mut start = None;
    let mut end = n;
    for i in 0..n {
        let row = table.row(i).unwrap();
        let idx = row[0].round() as i64;
        if idx == joint as i64 {
            if start.is_none() {
                start = Some(i);
            }
        } else if start.is_some() {
            end = i;
            break;
        }
    }
    match start {
        Some(s) => (s, end),
        None => (0, 0),
    }
}

/// Sample a `[joint_index, time_s, x, y, z]` row range (columns 1..4) at
/// `t`, lerping between bracketing keyframes and holding boundary values
/// outside the range — identical clamp semantics to
/// `gltf_animation_source::sample_vec3_track`.
fn sample_vec3_range(table: Option<&TableData>, range: (usize, usize), t: f32, default: [f32; 3]) -> [f32; 3] {
    let (start, end) = range;
    let Some(table) = table else { return default };
    let n = end.saturating_sub(start);
    if n == 0 {
        return default;
    }
    let row = |i: usize| -> [f32; 3] {
        let r = table.row(start + i).unwrap();
        [r[2], r[3], r[4]]
    };
    let time = |i: usize| -> f32 { table.row(start + i).unwrap()[1] };
    if n == 1 {
        return row(0);
    }
    let (first_t, last_t) = (time(0), time(n - 1));
    if t <= first_t {
        return row(0);
    }
    if t >= last_t {
        return row(n - 1);
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if time(mid) <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (t0, t1) = (time(lo), time(hi));
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    let (a, b) = (row(lo), row(hi));
    [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
}

/// Same as [`sample_vec3_range`] but for a `[joint_index, time_s, x, y,
/// z, w]` quaternion range (columns 1..5), slerping between keyframes.
fn sample_quat_range(table: Option<&TableData>, range: (usize, usize), t: f32) -> [f32; 4] {
    let (start, end) = range;
    let default = [0.0, 0.0, 0.0, 1.0];
    let Some(table) = table else { return default };
    let n = end.saturating_sub(start);
    if n == 0 {
        return default;
    }
    let row = |i: usize| -> [f32; 4] {
        let r = table.row(start + i).unwrap();
        [r[2], r[3], r[4], r[5]]
    };
    let time = |i: usize| -> f32 { table.row(start + i).unwrap()[1] };
    if n == 1 {
        return row(0);
    }
    let (first_t, last_t) = (time(0), time(n - 1));
    if t <= first_t {
        return row(0);
    }
    if t >= last_t {
        return row(n - 1);
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if time(mid) <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (t0, t1) = (time(lo), time(hi));
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    slerp(row(lo), row(hi), f)
}

fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let dot0 = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let (b, dot) = if dot0 < 0.0 { ([-b[0], -b[1], -b[2], -b[3]], -dot0) } else { (b, dot0) };
    if dot > 0.9995 {
        let lerped = [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
            a[3] + (b[3] - a[3]) * t,
        ];
        return normalize_quat(lerped);
    }
    let theta_0 = dot.clamp(-1.0, 1.0).acos();
    let theta = theta_0 * t;
    let sin_theta_0 = theta_0.sin();
    let s0 = (theta_0 - theta).sin() / sin_theta_0;
    let s1 = theta.sin() / sin_theta_0;
    [
        a[0] * s0 + b[0] * s1,
        a[1] * s0 + b[1] * s1,
        a[2] * s0 + b[2] * s1,
        a[3] * s0 + b[3] * s1,
    ]
}

fn normalize_quat(q: [f32; 4]) -> [f32; 4] {
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if len < 1e-12 { [0.0, 0.0, 0.0, 1.0] } else { [q[0] / len, q[1] / len, q[2] / len, q[3] / len] }
}

/// Read joint `j`'s `[joint_index, m0..m15]` row (column-major 4x4),
/// falling back to identity when no row exists for that joint.
fn mat4_from_table(table: Option<&TableData>, joint: usize) -> Mat4 {
    let Some(table) = table else { return MAT4_IDENTITY };
    for i in 0..table.row_count() {
        let row = table.row(i).unwrap();
        if row[0].round() as i64 == joint as i64 && row.len() >= 17 {
            let mut m = MAT4_IDENTITY;
            for col in 0..4 {
                for r in 0..4 {
                    m[col][r] = row[1 + col * 4 + r];
                }
            }
            return m;
        }
    }
    MAT4_IDENTITY
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
        let progress = match ctx.inputs.scalar("progress") {
            Some(ParamValue::Float(f)) => f,
            _ => default_progress(ctx.time, duration_s, rate),
        };
        let t = progress.rem_euclid(1.0) * duration_s;

        let joint_count = match ctx.params.get("joint_count") {
            Some(ParamValue::Float(f)) => (f.round().max(0.0) as usize).min(MAX_JOINTS),
            _ => 0,
        };
        if joint_count == 0 {
            return;
        }

        let parent_table = table_or_empty(ctx.params.get("joint_parent_table"));
        let root_world_table = table_or_empty(ctx.params.get("joint_root_world_table"));
        let inverse_bind_table = table_or_empty(ctx.params.get("inverse_bind_table"));
        let translation_tracks = table_or_empty(ctx.params.get("translation_tracks"));
        let rotation_tracks = table_or_empty(ctx.params.get("rotation_tracks"));
        let scale_tracks = table_or_empty(ctx.params.get("scale_tracks"));

        let mut parent = [-1i32; MAX_JOINTS];
        let mut root_world = [MAT4_IDENTITY; MAX_JOINTS];
        let mut inverse_bind = [MAT4_IDENTITY; MAX_JOINTS];
        let mut local = [MAT4_IDENTITY; MAX_JOINTS];
        let mut world: [Option<Mat4>; MAX_JOINTS] = [None; MAX_JOINTS];

        if let Some(table) = parent_table {
            for i in 0..table.row_count() {
                let row = table.row(i).unwrap();
                let j = row[0].round() as usize;
                if j < joint_count && row.len() >= 2 {
                    parent[j] = row[1].round() as i32;
                }
            }
        }

        for j in 0..joint_count {
            root_world[j] = mat4_from_table(root_world_table, j);
            inverse_bind[j] = mat4_from_table(inverse_bind_table, j);
            let tr = sample_vec3_range(translation_tracks, joint_row_range(translation_tracks, j), t, [0.0, 0.0, 0.0]);
            let rot = sample_quat_range(rotation_tracks, joint_row_range(rotation_tracks, j), t);
            let sc = sample_vec3_range(scale_tracks, joint_row_range(scale_tracks, j), t, [1.0, 1.0, 1.0]);
            local[j] = mat4_from_trs(tr, rot, sc);
        }

        let mut skin_matrices = Vec::with_capacity(joint_count);
        // `j` indexes THREE independent slices (`parent`/`local`/`root_world` via
        // `resolve_world`'s memoization, plus `inverse_bind` here) — an
        // `.iter().enumerate()` restructure would need all three zipped anyway,
        // which is less readable than the explicit index.
        #[allow(clippy::needless_range_loop)]
        for j in 0..joint_count {
            let w = resolve_world(j, &parent[..joint_count], &local[..joint_count], &root_world[..joint_count], &mut world[..joint_count], 0);
            let m = mat4_mul(&w, &inverse_bind[j]);
            skin_matrices.push(JointMatrix { c0: m[0], c1: m[1], c2: m[2], c3: m[3] });
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_progress_input_and_joint_matrix_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        assert_eq!(GltfSkeletonPose::TYPE_ID, "node.gltf_skeleton_pose");
        assert_eq!(GltfSkeletonPose::INPUTS.len(), 1);
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

    fn track_table(rows: Vec<Vec<f32>>) -> TableData {
        TableData::new(rows).unwrap()
    }

    #[test]
    fn joint_row_range_finds_a_grouped_slice() {
        // Rows for joints 0, 0, 1, 1, 1, 3 (joint 2 has no rows).
        let table = track_table(vec![
            vec![0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 1.0, 1.0, 0.0, 0.0],
            vec![1.0, 0.0, 2.0, 0.0, 0.0],
            vec![1.0, 0.5, 3.0, 0.0, 0.0],
            vec![1.0, 1.0, 4.0, 0.0, 0.0],
            vec![3.0, 0.0, 5.0, 0.0, 0.0],
        ]);
        assert_eq!(joint_row_range(Some(&table), 0), (0, 2));
        assert_eq!(joint_row_range(Some(&table), 1), (2, 5));
        assert_eq!(joint_row_range(Some(&table), 2), (0, 0), "no rows for joint 2");
        assert_eq!(joint_row_range(Some(&table), 3), (5, 6));
    }

    #[test]
    fn sample_vec3_range_lerps_and_holds_boundaries() {
        let table = track_table(vec![vec![0.0, 0.0, 0.0, 0.0, 0.0], vec![0.0, 1.0, 10.0, 0.0, 0.0]]);
        let range = joint_row_range(Some(&table), 0);
        let mid = sample_vec3_range(Some(&table), range, 0.5, [0.0, 0.0, 0.0]);
        assert!((mid[0] - 5.0).abs() < 1e-4, "halfway lerp, got {}", mid[0]);
        let before = sample_vec3_range(Some(&table), range, -1.0, [0.0, 0.0, 0.0]);
        assert!((before[0] - 0.0).abs() < 1e-4, "holds first keyframe before range");
        let after = sample_vec3_range(Some(&table), range, 5.0, [0.0, 0.0, 0.0]);
        assert!((after[0] - 10.0).abs() < 1e-4, "holds last keyframe after range");
    }

    #[test]
    fn sample_vec3_range_falls_back_to_default_when_joint_has_no_rows() {
        let out = sample_vec3_range(None, (0, 0), 0.5, [1.0, 2.0, 3.0]);
        assert_eq!(out, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn sample_quat_range_single_row_is_the_static_bind_pose() {
        let table = track_table(vec![vec![0.0, 0.0, 0.1, 0.2, 0.3, 0.9]]);
        let range = joint_row_range(Some(&table), 0);
        let q = sample_quat_range(Some(&table), range, 0.7);
        assert_eq!(q, [0.1, 0.2, 0.3, 0.9], "single-row table returns the static value at any t");
    }

    #[test]
    fn mat4_from_table_reads_column_major_and_falls_back_to_identity() {
        let mut row = vec![2.0f32]; // joint_index = 2
        for col in 0..4 {
            for r in 0..4 {
                row.push(if col == r { 1.0 } else { 0.0 });
            }
        }
        row[1 + 3 * 4] = 7.0; // c3.x -> translation x = 7
        let table = track_table(vec![row]);
        let m = mat4_from_table(Some(&table), 2);
        assert_eq!(m[3][0], 7.0, "translation column read correctly");
        assert_eq!(mat4_from_table(Some(&table), 5), MAT4_IDENTITY, "no row for joint 5 -> identity");
        assert_eq!(mat4_from_table(None, 0), MAT4_IDENTITY, "no table -> identity");
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
}
