//! `node.gltf_morph_weights` — samples a parsed glTF object's per-target
//! morph-weight keyframe tracks at a live `progress` (0..1) and emits the
//! current per-target weight vector (`Array(f32)`, one weight per target)
//! `node.morph_targets_blend` reads via a `BufferGather` lookup.
//!
//! GLTF_ANIMATION_DESIGN.md A3: a sibling of `node.gltf_skeleton_pose`
//! (same "sampling is graph-native, no parallel player subsystem"
//! doctrine, same binary-search+lerp sampler shape) — simpler than joint
//! posing because a morph weight is a single scalar per target, not a
//! composed TRS matrix: no parent-chain walk, just one channel lookup per
//! target per frame.
//!
//! GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: keyframe payload no longer lives in
//! this node's Table params (the pre-P2 `weight_tracks` Table is DELETED).
//! Instead `path` + `target_node` select an `Arc<GltfAnimSet>` from
//! `gltf_anim_cache`'s shared, file-backed, `Weak`-held cache —
//! `target_node` is `gltf_load::GltfObjectMorph::mesh_node_index` (a
//! `weights` channel targets the mesh-owning node directly, no
//! ancestor-chain ambiguity, unlike the rigid TRS path). `static_weights`
//! stays a small Table param (D1: topology-scale, O(target_count), not
//! keyframe payload — the SAME reasoning that keeps `clip_durations`
//! declared) carrying each target's authored non-animated weight, the
//! bind-pose-equivalent fallback for a target the selected clip doesn't
//! touch.
//!
//! Per-frame algorithm: for each target `i`, sample the resolved clip's
//! `Weights` channel for `target_node` (if any) via `partition_point`
//! binary search + lerp, holding boundary values outside the range. A
//! target with no animated channel in this clip falls back to
//! `static_weights[i]` — never a silent 0.0 (`MorphPrimitivesTest.glb`'s
//! `mesh.weights = [0.5]` with no animation is the documented case this
//! guards).

use std::borrow::Cow;
use std::sync::{Arc, mpsc};

use super::gltf_anim_shared::{LOOP_MODES, LoopMode, TriggerLatch, clip_duration, resolve_progress, sample_weight_slice};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::gltf_anim_cache::{AnimSetLookup, ChannelKind, GltfAnimSet, get_or_spawn_load};
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
    purpose: "Samples a parsed glTF object's per-target morph-weight keyframe channel (read from the shared gltf_anim_cache, selected by path+target_node) at a live `progress` (0..1) and emits the current per-target weight vector as Array(f32), one weight per target in target order. Wire the output straight into node.morph_targets_blend's `weights` input. LINEAR interpolation only (A1/A2/A3 scope, unchanged). `progress` port-shadowed with the same default beat-drive as node.gltf_animation_source/node.gltf_skeleton_pose: wrap(beats*rate/clip_beats), always wrapping into [0,1), never clamping. A target absent from the selected clip falls back to its authored static_weights[i], never a fabricated 0.0.",
    inputs: {
        progress: ScalarF32 optional,
        clip_index: ScalarF32 optional,
        trigger_count: ScalarF32 optional,
    },
    outputs: {
        weights: Array(f32),
    },
    params: [
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md D1/P2: comes via
        // presetMetadata.stringBindings, same convention as
        // node.gltf_mesh_source's/node.gltf_skeleton_pose's `path`.
        // Selects the `Arc<GltfAnimSet>` this node samples from
        // `gltf_anim_cache`'s shared cache.
        ParamDef {
            name: Cow::Borrowed("path"),
            label: "File",
            ty: ParamType::String,
            default: ParamValue::Float(0.0), // String default supplied via stringBindings; this slot is never read.
            range: None,
            enum_values: &[],
        },
        // GltfObjectMorph::mesh_node_index — a `weights` channel targets
        // the mesh-owning node directly (no ancestor-chain merge, unlike
        // node.gltf_animation_source's `target_node`).
        ParamDef {
            name: Cow::Borrowed("target_node"),
            label: "Target Node",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 100_000.0)),
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
            // GLTF_ANIM_RUNTIME_V2_DESIGN.md D6: follows the file, not an
            // arbitrary A4-era cap.
            range: Some((0.0, 255.0)),
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
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md D1: NOT keyframe payload —
        // O(target_count) topology (a handful of floats, the SAME scale
        // `clip_durations` stays declared at), so it stays a graph-def
        // param rather than moving to the cache. Rows: [target_index,
        // weight] — this target's authored `mesh.weights[i]`, the
        // bind-pose-equivalent fallback for a target the selected clip's
        // Weights channel doesn't cover.
        ParamDef {
            name: Cow::Borrowed("static_weights"),
            label: "Static Weights",
            ty: ParamType::Table,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "path comes via presetMetadata.stringBindings, same convention as node.gltf_mesh_source's `path`. target_node selects which scene node's weights channel this object samples (gltf_import.rs stamps both from GltfObjectMorph::mesh_node_index). Wire `weights` into node.morph_targets_blend's `weights` input (BufferGather — a target-index lookup, not coincident with morph_targets_blend's per-vertex dispatch). Unwired `progress` follows the default beat-drive; wire node.lfo (Saw) for a performer-controlled loop.",
    examples: [],
    picker: { label: "glTF Morph Weights", category: Driver },
    summary: "Samples an imported glTF asset's morph-target weight animation and outputs the per-target weight vector a Morph Targets Blend node needs. Wire progress to a beat or LFO to animate the blend.",
    category: Control,
    role: Source,
    aliases: ["morph weights", "blend shape weights", "morph target weights"],
    boundary_reason: NonGpu,
    extra_fields: {
        trigger_latch: TriggerLatch = TriggerLatch::new(),
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: same key-gated background-load
        // shape node.gltf_skeleton_pose's P1 rewire introduced.
        last_path: String = String::new(),
        anim_set: Option<Arc<GltfAnimSet>> = None,
        pending_load: Option<mpsc::Receiver<Result<Arc<GltfAnimSet>, String>>> = None,
    },
}

fn table_or_empty(v: Option<&ParamValue>) -> Option<&TableData> {
    match v {
        Some(ParamValue::Table(t)) => Some(t.as_ref()),
        _ => None,
    }
}

/// `static_weights` Table lookup: target `i`'s authored non-animated
/// weight, or `0.0` if the table is absent/has no row for that target.
fn static_weight(table: Option<&TableData>, target_index: usize) -> f32 {
    let Some(table) = table else { return 0.0 };
    for i in 0..table.row_count() {
        let Some(row) = table.row(i) else { continue };
        if row.len() >= 2 && row[0].round() as i64 == target_index as i64 {
            return row[1];
        }
    }
    0.0
}

/// Sample every target's weight at time `t`: `clip`'s `Weights` channel for
/// `target_node` (if any) via [`sample_weight_slice`], falling back to
/// `static_weights[i]` for a target the channel doesn't cover — never a
/// fabricated 0.0. Pure and `EffectNodeContext`-free so it's directly
/// unit-testable, the same shape `gltf_skeleton_pose::sample_skeleton_pose`
/// uses.
pub(crate) fn sample_morph_weights(
    clip: Option<&crate::node_graph::gltf_anim_cache::AnimClip>,
    target_node: u32,
    static_weights: Option<&TableData>,
    target_count: usize,
    t: f32,
) -> Vec<f32> {
    let channel = clip.and_then(|c| c.weights_channel(target_node));
    let mut weights = Vec::with_capacity(target_count);
    for i in 0..target_count {
        let default = static_weight(static_weights, i);
        let w = match channel {
            Some(c) => {
                let ChannelKind::Weights { target_count: stride } = c.kind else { unreachable!() };
                sample_weight_slice(&c.times, &c.values, stride as usize, i, t, default)
            }
            None => default,
        };
        weights.push(w);
    }
    weights
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
        let duration_s_param = match ctx.params.get("duration_s") {
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

        let target_count = match ctx.params.get("target_count") {
            Some(ParamValue::Float(f)) => (f.round().max(0.0) as usize).min(MAX_TARGETS),
            _ => 0,
        };
        if target_count == 0 {
            return;
        }

        // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: `path`/`target_node` select
        // the shared `Arc<GltfAnimSet>` — same key-gated background-load
        // shape node.gltf_skeleton_pose's P1 rewire introduced. Moved
        // BEFORE duration_s/progress so a resident clip's own
        // `AnimClip::duration_s` can drive playback speed.
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };
        let target_node = match ctx.params.get("target_node") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
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
                    log::error!("node.gltf_morph_weights: {e}");
                    self.pending_load = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.gltf_morph_weights: background load channel disconnected");
                    self.pending_load = None;
                }
            }
        }

        // Nothing loaded yet (or path empty/failed) — hold the output
        // buffer's existing contents, same convention
        // `gltf_mesh_source`/`gltf_skeleton_pose` use.
        let Some(anim_set) = self.anim_set.clone() else {
            return;
        };

        let duration_s = match anim_set.clips.get(clip_index) {
            Some(c) => c.duration_s,
            None => {
                let clip_durations = table_or_empty(ctx.params.get("clip_durations"));
                clip_duration(clip_durations, clip_index, duration_s_param)
            }
        };

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

        let static_weights = table_or_empty(ctx.params.get("static_weights"));
        let clip = anim_set.clips.get(clip_index);
        let weights = sample_morph_weights(clip, target_node, static_weights, target_count, t);

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

    fn static_weights_table(rows: Vec<(usize, f32)>) -> TableData {
        TableData::new(rows.into_iter().map(|(i, w)| vec![i as f32, w]).collect()).unwrap()
    }

    #[test]
    fn static_weight_finds_the_matching_target_row() {
        let table = static_weights_table(vec![(0, 0.5), (1, 0.25)]);
        assert_eq!(static_weight(Some(&table), 0), 0.5);
        assert_eq!(static_weight(Some(&table), 1), 0.25);
        assert_eq!(static_weight(Some(&table), 5), 0.0, "no row for target 5 -> 0.0");
        assert_eq!(static_weight(None, 0), 0.0, "no table -> 0.0");
    }

    fn weights_channel(node: u32, target_count: u32, keys: &[(f32, Vec<f32>)]) -> crate::node_graph::gltf_anim_cache::Channel {
        crate::node_graph::gltf_anim_cache::Channel {
            target_node: node,
            kind: ChannelKind::Weights { target_count },
            mode: crate::node_graph::gltf_load::GltfInterp::Linear,
            times: keys.iter().map(|(t, _)| *t).collect(),
            values: keys.iter().flat_map(|(_, v)| v.clone()).collect(),
            in_tangents: Vec::new(),
            out_tangents: Vec::new(),
        }
    }

    #[test]
    fn animated_target_lerps_between_bracketing_keyframes() {
        let channel = weights_channel(0, 1, &[(0.0, vec![0.0]), (1.0, vec![1.0])]);
        let clip = crate::node_graph::gltf_anim_cache::AnimClip { duration_s: 1.0, channels: vec![channel] };
        let out = sample_morph_weights(Some(&clip), 0, None, 1, 0.5);
        assert!((out[0] - 0.5).abs() < 1e-4, "expected the halfway-lerped weight, got {}", out[0]);
    }

    /// `MorphPrimitivesTest.glb`'s documented case: a static
    /// `mesh.weights = [0.5]` with no animation channel — the
    /// `static_weights` fallback is held at any `t`, never defaulting to
    /// 0.0.
    #[test]
    fn target_with_no_channel_falls_back_to_static_weights() {
        let clip = crate::node_graph::gltf_anim_cache::AnimClip { duration_s: 1.0, channels: Vec::new() };
        let static_weights = static_weights_table(vec![(0, 0.5)]);
        let out = sample_morph_weights(Some(&clip), 0, Some(&static_weights), 1, 0.73);
        assert_eq!(out[0], 0.5, "no channel for this target -> static_weights fallback, not 0.0");
    }

    #[test]
    fn no_clip_falls_back_to_static_weights_for_every_target() {
        let static_weights = static_weights_table(vec![(0, 0.1), (1, 0.2)]);
        let out = sample_morph_weights(None, 0, Some(&static_weights), 2, 0.5);
        assert_eq!(out, vec![0.1, 0.2], "out-of-range clip_index -> every target falls back");
    }

    #[test]
    fn multi_target_channel_extracts_the_right_stride_column() {
        // 2 targets sharing one Weights channel: target 0 rises 0.0->1.0,
        // target 1 rises 0.0->0.4 — proves the stride-2 SoA extraction
        // reads the RIGHT column per target, not just the right row.
        let channel = weights_channel(0, 2, &[(0.0, vec![0.0, 0.0]), (1.0, vec![1.0, 0.4])]);
        let clip = crate::node_graph::gltf_anim_cache::AnimClip { duration_s: 1.0, channels: vec![channel] };
        let out = sample_morph_weights(Some(&clip), 0, None, 2, 0.5);
        assert!((out[0] - 0.5).abs() < 1e-4, "target 0 halfway: got {}", out[0]);
        assert!((out[1] - 0.2).abs() < 1e-4, "target 1 halfway: got {}", out[1]);
    }

    #[test]
    fn is_trigger_latch_flag_is_set() {
        let prim = GltfMorphWeights::new();
        let node: &dyn EffectNode = &prim;
        assert!(node.is_trigger_latch());
    }
}
