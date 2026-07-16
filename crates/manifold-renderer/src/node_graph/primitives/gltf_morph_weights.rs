//! `node.gltf_morph_weights` — samples a parsed glTF object's per-target
//! morph-weight keyframe tracks at a live `progress` (0..1) and emits the
//! current per-target weight vector (`Array(f32)`, one weight per target)
//! `node.morph_targets_blend` reads via a `BufferGather` lookup.
//!
//! GLTF_ANIMATION_DESIGN.md A3: a sibling of `node.gltf_skeleton_pose`
//! (same "sampling is graph-native, no parallel player subsystem"
//! doctrine, same binary-search+lerp sampler shape) — simpler than joint
//! posing because a morph weight is a single scalar per target, not a
//! composed TRS matrix: no parent-chain walk, just one Table lookup per
//! target per frame.
//!
//! Per-frame algorithm: for each target `i`, sample `weight_tracks`' rows
//! matching `target_index == i` (grouped ascending by target_index, the
//! SAME `gltf_import.rs` emission contract `node.gltf_skeleton_pose`'s
//! Tables use) at the wrapped clip time `t`, lerping between bracketing
//! keyframes and holding boundary values outside the range. A target with
//! no animated channel gets a single static row (its authored
//! `mesh.weights[i]`, never a silent 0.0 — `MorphPrimitivesTest.glb`'s
//! `mesh.weights = [0.5]` with no animation is the documented case this
//! guards).

use std::borrow::Cow;

use crate::node_graph::effect_node::{EffectNodeContext, FrameTime};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue, TableData};
use crate::node_graph::primitive::Primitive;

/// Maximum morph targets this primitive will sample in one frame —
/// generous past the spec-typical few-to-a-dozen (`MorphStressTest.glb`,
/// the documented A3 stress case, ships 8); bounds the per-frame scratch
/// without an unbounded heap allocation.
const MAX_TARGETS: usize = 64;

crate::primitive! {
    name: GltfMorphWeights,
    type_id: "node.gltf_morph_weights",
    purpose: "Samples a parsed glTF object's per-target morph-weight keyframe tracks (or static authored weight, for an unanimated target) at a live `progress` (0..1) and emits the current per-target weight vector as Array(f32), one weight per target in target order. Wire the output straight into node.morph_targets_blend's `weights` input. LINEAR interpolation only (A1/A2/A3 scope). `progress` port-shadowed with the same default beat-drive as node.gltf_animation_source/node.gltf_skeleton_pose: wrap(beats*rate/clip_beats), always wrapping into [0,1), never clamping.",
    inputs: {
        progress: ScalarF32 optional,
    },
    outputs: {
        weights: Array(f32),
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
            name: Cow::Borrowed("target_count"),
            label: "Target Count",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, MAX_TARGETS as f32)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("weight_tracks"),
            label: "Weight Tracks",
            ty: ParamType::Table,
            // Rows: [target_index, time_s, weight], grouped ascending by
            // target_index, ascending time within a target. An unanimated
            // target gets a single static row (time_s = 0.0).
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    composition_notes: "gltf_import.rs builds weight_tracks at import time from GltfObjectMorph plus the resolved node.gltf_skeleton_pose. Wire `weights` into node.morph_targets_blend's `weights` input (BufferGather — a target-index lookup, not coincident with morph_targets_blend's per-vertex dispatch). Unwired `progress` follows the default beat-drive; wire node.lfo (Saw) for a performer-controlled loop.",
    examples: [],
    picker: { label: "glTF Morph Weights", category: Driver },
    summary: "Samples an imported glTF asset's morph-target weight animation and outputs the per-target weight vector a Morph Targets Blend node needs. Wire progress to a beat or LFO to animate the blend.",
    category: Control,
    role: Source,
    aliases: ["morph weights", "blend shape weights", "morph target weights"],
    boundary_reason: NonGpu,
}

/// Same default-beat-drive formula as `gltf_animation_source::default_progress`
/// / `gltf_skeleton_pose::default_progress` (D3) — duplicated rather than
/// shared across small CPU primitives with no other coupling; each is
/// independently gate-tested against the identical formula.
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

/// Row range `[start, end)` within `table` whose leading `target_index`
/// column equals `target`, assuming rows are grouped ascending by that
/// column — identical shape to `gltf_skeleton_pose::joint_row_range`.
fn target_row_range(table: Option<&TableData>, target: usize) -> (usize, usize) {
    let Some(table) = table else { return (0, 0) };
    let n = table.row_count();
    let mut start = None;
    let mut end = n;
    for i in 0..n {
        let row = table.row(i).unwrap();
        let idx = row[0].round() as i64;
        if idx == target as i64 {
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

/// Sample a `[target_index, time_s, weight]` row range (column 2) at `t`,
/// lerping between bracketing keyframes and holding boundary values
/// outside the range — same clamp semantics as
/// `gltf_skeleton_pose::sample_vec3_range`, scalar instead of vec3.
fn sample_scalar_range(table: Option<&TableData>, range: (usize, usize), t: f32, default: f32) -> f32 {
    let (start, end) = range;
    let Some(table) = table else { return default };
    let n = end.saturating_sub(start);
    if n == 0 {
        return default;
    }
    let value = |i: usize| -> f32 { table.row(start + i).unwrap()[2] };
    let time = |i: usize| -> f32 { table.row(start + i).unwrap()[1] };
    if n == 1 {
        return value(0);
    }
    let (first_t, last_t) = (time(0), time(n - 1));
    if t <= first_t {
        return value(0);
    }
    if t >= last_t {
        return value(n - 1);
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
    let (a, b) = (value(lo), value(hi));
    a + (b - a) * f
}

impl Primitive for GltfMorphWeights {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "weights" {
            return None;
        }
        match params.get("target_count") {
            Some(ParamValue::Float(f)) => Some(f.round().clamp(0.0, MAX_TARGETS as f32) as u32),
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

        let target_count = match ctx.params.get("target_count") {
            Some(ParamValue::Float(f)) => (f.round().max(0.0) as usize).min(MAX_TARGETS),
            _ => 0,
        };
        if target_count == 0 {
            return;
        }

        let weight_tracks = table_or_empty(ctx.params.get("weight_tracks"));

        let mut weights = Vec::with_capacity(target_count);
        for i in 0..target_count {
            let range = target_row_range(weight_tracks, i);
            weights.push(sample_scalar_range(weight_tracks, range, t, 0.0));
        }

        let Some(out_buf) = ctx.outputs.array("weights") else {
            return;
        };
        let capacity = (out_buf.size / std::mem::size_of::<f32>() as u64) as usize;
        let n = weights.len().min(capacity);
        if n == 0 {
            return;
        }
        // Safety: shared-memory MTLBuffer pre-bound by the chain build;
        // write count clamped to the buffer capacity; sequential executor
        // on the content thread means no concurrent writer.
        unsafe {
            out_buf.write(0, bytemuck::cast_slice(&weights[..n]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_progress_input_and_f32_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        assert_eq!(GltfMorphWeights::TYPE_ID, "node.gltf_morph_weights");
        assert_eq!(GltfMorphWeights::INPUTS.len(), 1);
        assert_eq!(GltfMorphWeights::INPUTS[0].name, "progress");
        assert!(!GltfMorphWeights::INPUTS[0].required);
        assert_eq!(GltfMorphWeights::INPUTS[0].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(GltfMorphWeights::OUTPUTS.len(), 1);
        assert_eq!(GltfMorphWeights::OUTPUTS[0].name, "weights");
        assert_eq!(GltfMorphWeights::OUTPUTS[0].ty, PortType::Array(ArrayType::of_known::<f32>()));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GltfMorphWeights::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gltf_morph_weights");
    }

    fn track_table(rows: Vec<Vec<f32>>) -> TableData {
        TableData::new(rows).unwrap()
    }

    #[test]
    fn target_row_range_finds_a_grouped_slice() {
        // Rows for targets 0, 0, 1, 1, 1, 3 (target 2 has no rows).
        let table = track_table(vec![
            vec![0.0, 0.0, 0.0],
            vec![0.0, 1.0, 1.0],
            vec![1.0, 0.0, 2.0],
            vec![1.0, 0.5, 3.0],
            vec![1.0, 1.0, 4.0],
            vec![3.0, 0.0, 5.0],
        ]);
        assert_eq!(target_row_range(Some(&table), 0), (0, 2));
        assert_eq!(target_row_range(Some(&table), 1), (2, 5));
        assert_eq!(target_row_range(Some(&table), 2), (0, 0), "no rows for target 2");
        assert_eq!(target_row_range(Some(&table), 3), (5, 6));
    }

    #[test]
    fn sample_scalar_range_lerps_and_holds_boundaries() {
        let table = track_table(vec![vec![0.0, 0.0, 0.0], vec![0.0, 1.0, 10.0]]);
        let range = target_row_range(Some(&table), 0);
        let mid = sample_scalar_range(Some(&table), range, 0.5, 0.0);
        assert!((mid - 5.0).abs() < 1e-4, "halfway lerp, got {mid}");
        let before = sample_scalar_range(Some(&table), range, -1.0, 0.0);
        assert!((before - 0.0).abs() < 1e-4, "holds first keyframe before range");
        let after = sample_scalar_range(Some(&table), range, 5.0, 0.0);
        assert!((after - 10.0).abs() < 1e-4, "holds last keyframe after range");
    }

    /// `MorphPrimitivesTest.glb`'s documented case: a static
    /// `mesh.weights = [0.5]` with no animation channel — one row, held
    /// at any `t`, never defaulting to 0.0.
    #[test]
    fn single_row_static_weight_is_held_at_any_t() {
        let table = track_table(vec![vec![0.0, 0.0, 0.5]]);
        let range = target_row_range(Some(&table), 0);
        let w = sample_scalar_range(Some(&table), range, 0.73, 0.0);
        assert_eq!(w, 0.5, "single-row table returns the static value at any t");
    }

    #[test]
    fn sample_scalar_range_falls_back_to_default_when_target_has_no_rows() {
        let out = sample_scalar_range(None, (0, 0), 0.5, 0.25);
        assert_eq!(out, 0.25);
    }
}
