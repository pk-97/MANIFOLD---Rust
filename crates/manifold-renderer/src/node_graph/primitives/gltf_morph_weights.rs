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

use super::gltf_anim_shared::{
    LOOP_MODES, LoopMode, TriggerLatch, clip_duration, resolve_progress, row_range_for_compound_key,
    sample_scalar_range,
};
use crate::node_graph::effect_node::EffectNodeContext;
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
        clip_index: ScalarF32 optional,
        trigger_count: ScalarF32 optional,
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
            // Rows: [clip_index, target_index, time_s, weight] (A4:
            // clip_index prepended for D4 multi-clip selection), grouped
            // ascending by (clip_index, target_index), ascending time within
            // a (clip, target) block. An unanimated target gets a single
            // static row (time_s = 0.0) per clip.
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "gltf_import.rs builds weight_tracks at import time from GltfObjectMorph plus the resolved node.gltf_skeleton_pose. Wire `weights` into node.morph_targets_blend's `weights` input (BufferGather — a target-index lookup, not coincident with morph_targets_blend's per-vertex dispatch). Unwired `progress` follows the default beat-drive; wire node.lfo (Saw) for a performer-controlled loop.",
    examples: [],
    picker: { label: "glTF Morph Weights", category: Driver },
    summary: "Samples an imported glTF asset's morph-target weight animation and outputs the per-target weight vector a Morph Targets Blend node needs. Wire progress to a beat or LFO to animate the blend.",
    category: Control,
    role: Source,
    aliases: ["morph weights", "blend shape weights", "morph target weights"],
    boundary_reason: NonGpu,
    extra_fields: {
        trigger_latch: TriggerLatch = TriggerLatch::new(),
    },
}

fn table_or_empty(v: Option<&ParamValue>) -> Option<&TableData> {
    match v {
        Some(ParamValue::Table(t)) => Some(t.as_ref()),
        _ => None,
    }
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
            let range = row_range_for_compound_key(weight_tracks, clip_index, i);
            weights.push(sample_scalar_range(weight_tracks, range, 2, 3, t, 0.0));
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
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_progress_input_and_f32_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        assert_eq!(GltfMorphWeights::TYPE_ID, "node.gltf_morph_weights");
        assert_eq!(GltfMorphWeights::INPUTS.len(), 3);
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

    /// Prepends `clip_index = 0` to each `[target_index, time_s, weight]`
    /// row (pre-A4 test shape).
    fn clip0_track_table(rows: Vec<Vec<f32>>) -> TableData {
        track_table(rows.into_iter().map(|r| [0.0].into_iter().chain(r).collect()).collect())
    }

    #[test]
    fn target_row_range_finds_a_grouped_slice() {
        // Rows for targets 0, 0, 1, 1, 1, 3 (target 2 has no rows), clip 0.
        let table = clip0_track_table(vec![
            vec![0.0, 0.0, 0.0],
            vec![0.0, 1.0, 1.0],
            vec![1.0, 0.0, 2.0],
            vec![1.0, 0.5, 3.0],
            vec![1.0, 1.0, 4.0],
            vec![3.0, 0.0, 5.0],
        ]);
        assert_eq!(row_range_for_compound_key(Some(&table), 0, 0), (0, 2));
        assert_eq!(row_range_for_compound_key(Some(&table), 0, 1), (2, 5));
        assert_eq!(row_range_for_compound_key(Some(&table), 0, 2), (0, 0), "no rows for target 2");
        assert_eq!(row_range_for_compound_key(Some(&table), 0, 3), (5, 6));
    }

    #[test]
    fn sample_scalar_range_lerps_and_holds_boundaries() {
        let table = clip0_track_table(vec![vec![0.0, 0.0, 0.0], vec![0.0, 1.0, 10.0]]);
        let range = row_range_for_compound_key(Some(&table), 0, 0);
        let mid = sample_scalar_range(Some(&table), range, 2, 3, 0.5, 0.0);
        assert!((mid - 5.0).abs() < 1e-4, "halfway lerp, got {mid}");
        let before = sample_scalar_range(Some(&table), range, 2, 3, -1.0, 0.0);
        assert!((before - 0.0).abs() < 1e-4, "holds first keyframe before range");
        let after = sample_scalar_range(Some(&table), range, 2, 3, 5.0, 0.0);
        assert!((after - 10.0).abs() < 1e-4, "holds last keyframe after range");
    }

    /// `MorphPrimitivesTest.glb`'s documented case: a static
    /// `mesh.weights = [0.5]` with no animation channel — one row, held
    /// at any `t`, never defaulting to 0.0.
    #[test]
    fn single_row_static_weight_is_held_at_any_t() {
        let table = clip0_track_table(vec![vec![0.0, 0.0, 0.5]]);
        let range = row_range_for_compound_key(Some(&table), 0, 0);
        let w = sample_scalar_range(Some(&table), range, 2, 3, 0.73, 0.0);
        assert_eq!(w, 0.5, "single-row table returns the static value at any t");
    }

    #[test]
    fn sample_scalar_range_falls_back_to_default_when_target_has_no_rows() {
        let out = sample_scalar_range(None, (0, 0), 2, 3, 0.5, 0.25);
        assert_eq!(out, 0.25);
    }

    #[test]
    fn is_trigger_latch_flag_is_set() {
        let prim = GltfMorphWeights::new();
        let node: &dyn EffectNode = &prim;
        assert!(node.is_trigger_latch());
    }
}
