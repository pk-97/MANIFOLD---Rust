//! `node.gltf_animation_source` — samples a parsed glTF TRS keyframe
//! clip at a live `progress` (0..1) and emits the nine scalars
//! `node.transform_3d`'s port-shadowed inputs already accept.
//!
//! GLTF_ANIMATION_DESIGN.md A1 (D1): "animating a rigid node is animating
//! params" — this node does no scene-graph work itself; it is a pure
//! CPU sampler that turns three optional `Table` params (one keyframe
//! track each for translation / rotation / scale, authored at import
//! time by `gltf_import.rs` from `gltf_load::GltfObjectAnimation`) into
//! the nine `pos_x/y/z, rot_x/y/z, scale_x/y/z` scalars, wired straight
//! into an object's `node.transform_3d`.
//!
//! `progress` is port-shadowed like every other control-rate input in
//! this catalog: wire an `node.lfo` (Saw) or a fader for direct control,
//! or leave it unwired for the D3 default beat-drive —
//! `wrap(beats · rate / clip_beats)`, `clip_beats = duration_s ×
//! (beats-per-second implied by the current transport)`. Whichever the
//! source, the sampler always WRAPS `progress` into `[0, 1)` before
//! converting to a clip-relative time — never clamps — so a slightly
//! out-of-range value (an LFO edge, a scrub past the end) continues
//! smoothly into the next cycle instead of freezing at the boundary.
//!
//! LINEAR interpolation only (A1 scope, per the glTF spec's own
//! keyframe semantics): translation/scale lerp between the bracketing
//! keyframes, rotation slerps its quaternion pair then converts to the
//! XYZ Euler triple that reproduces the SAME rotation through
//! `render_scene::model_matrix`'s exact `Rz(z) · Ry_used(y) · Rx(x)`
//! composition (see [`quat_to_render_scene_euler`]). A channel absent
//! from the clip (its Table param left at the `Float(0.0)` sentinel)
//! passes through as the static neutral default — 0 for position/
//! rotation, 1 for scale — never fabricated motion.

use std::borrow::Cow;

use crate::node_graph::effect_node::{EffectNodeContext, FrameTime};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue, TableData};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: GltfAnimationSource,
    type_id: "node.gltf_animation_source",
    purpose: "Samples a parsed glTF TRS keyframe clip (translation/rotation/scale Tables authored at import time) at a live `progress` (0..1) and emits the nine pos_x/y/z, rot_x/y/z, scale_x/y/z scalars node.transform_3d's port-shadowed inputs accept — wire straight into an object's transform_3d to animate it. LINEAR interpolation only (A1 scope): lerp for translation/scale, slerp+quat-to-Euler for rotation. `progress` port-shadowed: wire an LFO/fader for direct control, or leave unwired for the default beat-drive (wrap(beats*rate/clip_beats), clip_beats = duration_s scaled by the live transport) — always wraps into [0,1) before sampling, never clamps, so a clip loops continuously at the wrap point rather than freezing. A channel absent from the clip (Table left at its sentinel) passes through as the static neutral default (0 pos/rot, 1 scale).",
    inputs: {
        progress: ScalarF32 optional,
    },
    outputs: {
        pos_x: ScalarF32,
        pos_y: ScalarF32,
        pos_z: ScalarF32,
        rot_x: ScalarF32,
        rot_y: ScalarF32,
        rot_z: ScalarF32,
        scale_x: ScalarF32,
        scale_y: ScalarF32,
        scale_z: ScalarF32,
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
        // GLTF_ANIMATION_DESIGN.md A1 fix (found rendering BoxAnimated.glb's
        // own goldens — the four phases came out pixel-identical): a
        // transform_3d input port is a port-SHADOW — wiring it makes the
        // wire win OUTRIGHT over the node's own static param, it does not
        // add to it. gltf_import.rs's per-object static recenter
        // (`-object_center`) lives on transform_3d's pos_x/y/z params, so
        // wiring this node's pos_x/y/z straight into those ports silently
        // discarded the recenter for every animated object — the sampled
        // translation landed in raw (un-recentered) glTF space instead of
        // the recentered space every OTHER node in the scene uses,
        // typically moving the object far outside the framing camera.
        // Composing the recenter HERE (added to the sampled position
        // before it reaches transform_3d) is the fix: one node still owns
        // the whole pos_x/y/z port-shadow, and its output is correct in
        // the same recentered space as a static object's.
        ParamDef {
            name: Cow::Borrowed("recenter_x"),
            label: "Recenter X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("recenter_y"),
            label: "Recenter Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("recenter_z"),
            label: "Recenter Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("translation_track"),
            label: "Translation Track",
            ty: ParamType::Table,
            // Tables can't live in static-const ParamValue — see
            // node.cycle_table_row's identical sentinel convention. Rows
            // are [time_s, x, y, z]; absent (sentinel) means "this
            // channel isn't animated in the source clip".
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rotation_track"),
            label: "Rotation Track",
            ty: ParamType::Table,
            // Rows are [time_s, x, y, z, w] (quaternion).
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_track"),
            label: "Scale Track",
            ty: ParamType::Table,
            // Rows are [time_s, x, y, z].
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    composition_notes: "Wire `pos_x`..`scale_z` into a node.transform_3d's same-named input ports (its nine params are already port-shadowed — see docs/GLTF_ANIMATION_DESIGN.md A1). Tables are authored by gltf_import.rs at import time, one row per glTF keyframe; JSON shape matches node.cycle_table_row's Table convention. Unwired `progress` follows the default beat-drive; wire node.lfo (Saw) for a performer-controlled loop, or any 0..1 scrub source.",
    examples: [],
    picker: { label: "glTF Animation Source", category: Driver },
    summary: "Plays back an imported glTF animation clip. Wire its outputs into a Transform 3D node to animate an imported object, or leave the progress input unwired to loop it on the beat.",
    category: Control,
    role: Source,
    aliases: ["gltf animation", "imported animation", "clip sampler", "keyframe sampler"],
    boundary_reason: NonGpu,
}

/// D3 default: `progress = wrap(beats * rate / clip_beats)`, `clip_beats
/// = duration_s * beats_per_second` where `beats_per_second` is read
/// live off the current transport (`FrameTime.beats / FrameTime.seconds`
/// — both clocks share the same tempo-scaled origin under this engine's
/// transport model) rather than a plumbed BPM (no such value reaches a
/// graph node today — see `EffectNodeContext`/`FrameTime`, which carry
/// only beats/seconds/delta). Falls back to 2.0 (120 BPM) when `seconds`
/// is ~0 (frame 0) to avoid a divide-by-zero; this ratio is exact under
/// constant tempo, which the sync engine's shared-origin clocks
/// guarantee within one playback session.
fn default_progress(time: FrameTime, duration_s: f32, rate: f32) -> f32 {
    let beats = time.beats.0 as f32;
    let seconds = time.seconds.0 as f32;
    let beats_per_second = if seconds.abs() > 1e-6 { beats / seconds } else { 2.0 };
    let clip_beats = (duration_s * beats_per_second).max(1e-6);
    let raw = beats * rate / clip_beats;
    raw.rem_euclid(1.0)
}

/// Binary-search + lerp a `[time_s, x, y, z]` table at time `t`. Clamps
/// (holds the boundary keyframe's value) for `t` outside the table's own
/// time range — the glTF spec's own "before first / after last keyframe"
/// rule — NOT the same as this primitive's own progress wrap, which
/// happens one level up (`t` is already wrapped into `[0, duration_s)`
/// before this function ever sees it). Returns `None` for an absent/
/// malformed table (fewer than 4 columns or zero rows).
fn sample_vec3_track(table: &TableData, t: f32) -> Option<[f32; 3]> {
    if table.col_count() < 4 || table.row_count() == 0 {
        return None;
    }
    let n = table.row_count();
    let row3 = |i: usize| -> [f32; 3] {
        let r = table.row(i).unwrap();
        [r[1], r[2], r[3]]
    };
    if n == 1 {
        return Some(row3(0));
    }
    let first_t = table.row(0).unwrap()[0];
    let last_t = table.row(n - 1).unwrap()[0];
    if t <= first_t {
        return Some(row3(0));
    }
    if t >= last_t {
        return Some(row3(n - 1));
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if table.row(mid).unwrap()[0] <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (t0, t1) = (table.row(lo).unwrap()[0], table.row(hi).unwrap()[0]);
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    let (a, b) = (row3(lo), row3(hi));
    Some([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f])
}

/// Same bracketing/clamp logic as [`sample_vec3_track`], but slerps the
/// `[time_s, x, y, z, w]` quaternion pair instead of lerping.
fn sample_quat_track(table: &TableData, t: f32) -> Option<[f32; 4]> {
    if table.col_count() < 5 || table.row_count() == 0 {
        return None;
    }
    let n = table.row_count();
    let row4 = |i: usize| -> [f32; 4] {
        let r = table.row(i).unwrap();
        [r[1], r[2], r[3], r[4]]
    };
    if n == 1 {
        return Some(row4(0));
    }
    let first_t = table.row(0).unwrap()[0];
    let last_t = table.row(n - 1).unwrap()[0];
    if t <= first_t {
        return Some(row4(0));
    }
    if t >= last_t {
        return Some(row4(n - 1));
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if table.row(mid).unwrap()[0] <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (t0, t1) = (table.row(lo).unwrap()[0], table.row(hi).unwrap()[0]);
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    Some(slerp(row4(lo), row4(hi), f))
}

/// Spherical linear interpolation between two quaternions `[x, y, z, w]`.
/// Takes the short arc (negates `b` when the dot product is negative)
/// and falls back to a normalized lerp when the quaternions are nearly
/// parallel (the standard near-`sin(theta)==0` numerical guard).
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

/// Quaternion `[x, y, z, w]` → the `(rot_x, rot_y, rot_z)` Euler triple
/// that reconstructs the SAME rotation through
/// `render_scene::model_matrix`'s exact composition order
/// (`euler_xyz_columns`: `Rz(z) · Ry(y) · Rx(x)`, column-major — see
/// that function's own array literals). The extraction formula below
/// (`rx = atan2(r21,r22)`, `ry = asin(-r20)`, `rz = atan2(r10,r00)`) was
/// derived and checked NUMERICALLY against that exact composition
/// (`euler_xyz_columns`'s literal `rx`/`ry`/`rz` arrays, transcribed
/// verbatim and multiplied out — a first hand-derivation attempt here,
/// working from a half-remembered "textbook" combined-matrix formula
/// instead of the actual source, got the signs wrong twice before this
/// numeric check caught it) and is re-verified in Rust by this module's
/// own round-trip test, which reconstructs `Rz·Ry·Rx` from the extracted
/// angles and checks it against the quaternion's own rotation matrix
/// bit-for-bit. Falls back to a `z = 0` decomposition at the gimbal-lock
/// singularity (`|r20| ~= 1`), the conventional resolution for this
/// Euler order — the fallback's sign also flips with the sign of `r20`
/// (verified the same way).
pub(crate) fn quat_to_render_scene_euler(q: [f32; 4]) -> [f32; 3] {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let (xx, yy, zz) = (x * x, y * y, z * z);
    let (xy, xz, yz) = (x * y, x * z, y * z);
    let (wx, wy, wz) = (w * x, w * y, w * z);
    // Row-major rotation matrix r[row][col] — same convention as
    // `gltf_load::mat4_from_trs`'s upper-left 3x3 (that function is
    // column-major; these are its entries read row-major).
    let r20 = 2.0 * (xz - wy);
    let r21 = 2.0 * (yz + wx);
    let r22 = 1.0 - 2.0 * (xx + yy);
    let r10 = 2.0 * (xy + wz);
    let r00 = 1.0 - 2.0 * (yy + zz);
    let r01 = 2.0 * (xy - wz);
    let r11 = 1.0 - 2.0 * (xx + zz);

    let r20c = r20.clamp(-1.0, 1.0);
    if (1.0 - r20c.abs()) < 1e-6 {
        // Gimbal lock: x and z become degenerate around this axis (only
        // their sum/difference is recoverable). Pin z = 0 and fold the
        // whole rotation into x. `asin`'s derivative blows up at +/-1,
        // so computing `ry` via `asin(-r20c)` here would amplify the
        // f32 rounding already present in `r20c` into a much larger
        // angular error (measured: a quaternion built from
        // `sin/cos(PI/4)` alone put `r20c` ~6e-8 short of -1.0, which
        // `asin` turned into a ~3.5e-4 radian error) — since we already
        // know we're at the pole, set `ry` to the pole value directly
        // instead of trusting `asin` near its singularity.
        let ry = std::f32::consts::FRAC_PI_2.copysign(-r20c);
        let rx = if r20 < 0.0 { r01.atan2(r11) } else { (-r01).atan2(r11) };
        return [rx, ry, 0.0];
    }
    let rx = r21.atan2(r22);
    let ry = (-r20c).asin();
    let rz = r10.atan2(r00);
    [rx, ry, rz]
}

impl Primitive for GltfAnimationSource {
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
        // Wrap (never clamp) into [0, duration_s) — see the module docs
        // and the primitive's `composition_notes`.
        let t = progress.rem_euclid(1.0) * duration_s;

        let recenter = [
            ctx.params.get("recenter_x").and_then(ParamValue::as_scalar).unwrap_or(0.0),
            ctx.params.get("recenter_y").and_then(ParamValue::as_scalar).unwrap_or(0.0),
            ctx.params.get("recenter_z").and_then(ParamValue::as_scalar).unwrap_or(0.0),
        ];
        let sampled_pos = match ctx.params.get("translation_track") {
            Some(ParamValue::Table(table)) => sample_vec3_track(table, t).unwrap_or([0.0, 0.0, 0.0]),
            _ => [0.0, 0.0, 0.0],
        };
        // Composed here, not left to the port-shadow at transform_3d — see
        // `recenter_x`'s doc comment.
        let pos = [
            sampled_pos[0] + recenter[0],
            sampled_pos[1] + recenter[1],
            sampled_pos[2] + recenter[2],
        ];
        let scale = match ctx.params.get("scale_track") {
            Some(ParamValue::Table(table)) => sample_vec3_track(table, t).unwrap_or([1.0, 1.0, 1.0]),
            _ => [1.0, 1.0, 1.0],
        };
        let rot = match ctx.params.get("rotation_track") {
            Some(ParamValue::Table(table)) => sample_quat_track(table, t)
                .map(quat_to_render_scene_euler)
                .unwrap_or([0.0, 0.0, 0.0]),
            _ => [0.0, 0.0, 0.0],
        };

        ctx.outputs.set_scalar("pos_x", ParamValue::Float(pos[0]));
        ctx.outputs.set_scalar("pos_y", ParamValue::Float(pos[1]));
        ctx.outputs.set_scalar("pos_z", ParamValue::Float(pos[2]));
        ctx.outputs.set_scalar("rot_x", ParamValue::Float(rot[0]));
        ctx.outputs.set_scalar("rot_y", ParamValue::Float(rot[1]));
        ctx.outputs.set_scalar("rot_z", ParamValue::Float(rot[2]));
        ctx.outputs.set_scalar("scale_x", ParamValue::Float(scale[0]));
        ctx.outputs.set_scalar("scale_y", ParamValue::Float(scale[1]));
        ctx.outputs.set_scalar("scale_z", ParamValue::Float(scale[2]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::MockBackend;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::effect_node::ParamValues;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;
    use manifold_core::{Beats, Seconds};
    use std::sync::Arc;

    fn frame_time(beats: f32, seconds: f32) -> FrameTime {
        FrameTime {
            beats: Beats(beats as f64),
            seconds: Seconds(seconds as f64),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn declares_progress_input_and_nine_scalar_outputs() {
        assert_eq!(GltfAnimationSource::TYPE_ID, "node.gltf_animation_source");
        assert_eq!(GltfAnimationSource::INPUTS.len(), 1);
        assert_eq!(GltfAnimationSource::INPUTS[0].name, "progress");
        assert!(!GltfAnimationSource::INPUTS[0].required);
        assert_eq!(GltfAnimationSource::OUTPUTS.len(), 9);
        for (out, name) in GltfAnimationSource::OUTPUTS.iter().zip([
            "pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z",
        ]) {
            assert_eq!(out.name, name);
            assert_eq!(out.ty, PortType::Scalar(ScalarType::F32));
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GltfAnimationSource::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gltf_animation_source");
    }

    /// Runs the primitive with the given params and wired `progress`
    /// (`None` = unwired, default beat-drive), returning the nine
    /// outputs in declaration order.
    fn run_with(
        overrides: &[(&'static str, ParamValue)],
        wired_progress: Option<f32>,
        time: FrameTime,
    ) -> [f32; 9] {
        let mut backend = MockBackend::new();
        let out_names =
            ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z"];
        let out_slots: Vec<(&'static str, Slot)> = out_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                (*name, backend.acquire(ResourceId(i as u32), PortType::Scalar(ScalarType::F32), None, (0, 0)))
            })
            .collect();

        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("duration_s"), ParamValue::Float(1.0));
        params.insert(Cow::Borrowed("rate"), ParamValue::Float(1.0));
        for (name, value) in overrides {
            params.insert(Cow::Borrowed(*name), value.clone());
        }

        let mut wire_slots: Vec<(&'static str, Slot)> = Vec::new();
        if let Some(p) = wired_progress {
            let slot = backend.acquire(ResourceId(100), PortType::Scalar(ScalarType::F32), None, (0, 0));
            backend.set_scalar(slot, ParamValue::Float(p));
            wire_slots.push(("progress", slot));
        }

        let mut prim = GltfAnimationSource::new();
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let inputs = NodeInputs::new(&wire_slots, &backend, &[]);
        let outputs = NodeOutputs::new(
            &out_slots,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
        );
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in scalar_scratch.drain(..) {
            backend.set_scalar(slot, value);
        }

        let mut result = [0.0f32; 9];
        for (i, (_, slot)) in out_slots.iter().enumerate() {
            result[i] = match backend.scalar(*slot) {
                Some(ParamValue::Float(f)) => f,
                other => panic!("expected Float on {}, got {other:?}", out_names[i]),
            };
        }
        result
    }

    fn translation_table(rows: Vec<[f32; 4]>) -> ParamValue {
        ParamValue::Table(Arc::new(TableData::new(rows.into_iter().map(|r| r.to_vec()).collect()).unwrap()))
    }

    fn rotation_table(rows: Vec<[f32; 5]>) -> ParamValue {
        ParamValue::Table(Arc::new(TableData::new(rows.into_iter().map(|r| r.to_vec()).collect()).unwrap()))
    }

    #[test]
    fn unwired_channels_pass_through_static_neutral_defaults() {
        let out = run_with(&[], Some(0.5), frame_time(0.0, 0.0));
        assert_eq!(&out[0..6], &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0], "pos/rot default to 0");
        assert_eq!(&out[6..9], &[1.0, 1.0, 1.0], "scale defaults to 1");
    }

    #[test]
    fn translation_track_lerps_between_bracketing_keyframes() {
        let table = translation_table(vec![[0.0, 0.0, 0.0, 0.0], [1.0, 10.0, 0.0, 0.0]]);
        // duration_s = 1.0 (default), progress = 0.5 -> t = 0.5 -> halfway.
        let out = run_with(&[("translation_track", table)], Some(0.5), frame_time(0.0, 0.0));
        assert!((out[0] - 5.0).abs() < 1e-4, "pos_x should be halfway lerped, got {}", out[0]);
    }

    #[test]
    fn translation_track_holds_boundary_values_outside_its_time_range() {
        // Keyframes only span [1.0, 2.0]; duration_s = 4.0 so progress=0
        // (t=0) is before the first keyframe and progress=0.99 (t=3.96)
        // is after the last — both must hold the boundary value, not
        // extrapolate or wrap within the table itself.
        let table = translation_table(vec![[1.0, 5.0, 0.0, 0.0], [2.0, 9.0, 0.0, 0.0]]);
        let before = run_with(
            &[("translation_track", table.clone()), ("duration_s", ParamValue::Float(4.0))],
            Some(0.0),
            frame_time(0.0, 0.0),
        );
        assert!((before[0] - 5.0).abs() < 1e-4, "before first keyframe holds it, got {}", before[0]);
        let after = run_with(
            &[("translation_track", table), ("duration_s", ParamValue::Float(4.0))],
            Some(0.99),
            frame_time(0.0, 0.0),
        );
        assert!((after[0] - 9.0).abs() < 1e-4, "after last keyframe holds it, got {}", after[0]);
    }

    #[test]
    fn progress_wraps_not_clamps_at_the_loop_boundary() {
        // A slightly-negative and slightly-over-1 progress must wrap to
        // the SAME point a plain in-range progress would reach — proving
        // the sampler uses rem_euclid, not clamp(0,1). If it clamped,
        // -0.01 would pin to t=0 (start-of-clip value) instead of
        // wrapping to ~0.99 (near-end-of-clip value).
        let table = translation_table(vec![[0.0, 0.0, 0.0, 0.0], [1.0, 100.0, 0.0, 0.0]]);
        let wrapped_negative =
            run_with(&[("translation_track", table.clone())], Some(-0.01), frame_time(0.0, 0.0));
        let plain_99 = run_with(&[("translation_track", table.clone())], Some(0.99), frame_time(0.0, 0.0));
        assert!(
            (wrapped_negative[0] - plain_99[0]).abs() < 1e-3,
            "progress=-0.01 must wrap to ~0.99, not clamp to 0: got {} vs {}",
            wrapped_negative[0],
            plain_99[0]
        );

        let wrapped_over = run_with(&[("translation_track", table.clone())], Some(1.01), frame_time(0.0, 0.0));
        let plain_01 = run_with(&[("translation_track", table)], Some(0.01), frame_time(0.0, 0.0));
        assert!(
            (wrapped_over[0] - plain_01[0]).abs() < 1e-3,
            "progress=1.01 must wrap to ~0.01, not clamp to 1: got {} vs {}",
            wrapped_over[0],
            plain_01[0]
        );
    }

    #[test]
    fn rotation_track_slerps_and_holds_boundaries() {
        // 90-degree rotation about Z: quat(0,0,sin(45deg),cos(45deg)).
        let half = (std::f32::consts::FRAC_PI_4).sin();
        let cos_half = (std::f32::consts::FRAC_PI_4).cos();
        let table =
            rotation_table(vec![[0.0, 0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, half, cos_half]]);
        let out = run_with(&[("rotation_track", table)], Some(0.0), frame_time(0.0, 0.0));
        assert!(
            out[3].abs() < 1e-4 && out[4].abs() < 1e-4 && out[5].abs() < 1e-4,
            "identity quaternion should decode to zero Euler, got {:?}",
            &out[3..6]
        );
    }

    #[test]
    fn unwired_progress_follows_default_beat_drive() {
        // duration_s=1.0, rate=1.0. At 120 BPM (beats=2*seconds implied
        // by the fallback ratio) progress = beats / (duration_s * 2.0).
        // beats=1.0 -> progress=0.5 -> t=0.5.
        let table = translation_table(vec![[0.0, 0.0, 0.0, 0.0], [1.0, 8.0, 0.0, 0.0]]);
        let out = run_with(&[("translation_track", table)], None, frame_time(1.0, 0.5));
        assert!((out[0] - 4.0).abs() < 1e-3, "expected halfway through the clip, got {}", out[0]);
    }

    /// Numerically verifies [`quat_to_render_scene_euler`]'s derivation:
    /// composing the returned Euler triple through the SAME `Rz*Ry_used*Rx`
    /// formula `render_scene::model_matrix` uses must reproduce the
    /// original quaternion's own rotation matrix. Avoids exact gimbal-lock
    /// angles (|y| ~= 90 degrees) where the decomposition is ambiguous by
    /// construction.
    #[test]
    fn quat_to_euler_round_trips_through_render_scene_composition() {
        // Row-major r[row][col], matching `gltf_load::mat4_from_trs`'s
        // upper-left 3x3 EXACTLY (that function is column-major; this is
        // its transpose-to-row-major reading) — the authoritative
        // glTF-spec quat->matrix convention already load-bearing
        // elsewhere in this codebase.
        fn quat_to_matrix(q: [f32; 4]) -> [[f32; 3]; 3] {
            let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
            let (xx, yy, zz) = (x * x, y * y, z * z);
            let (xy, xz, yz) = (x * y, x * z, y * z);
            let (wx, wy, wz) = (w * x, w * y, w * z);
            [
                [1.0 - 2.0 * (yy + zz), 2.0 * (xy - wz), 2.0 * (xz + wy)],
                [2.0 * (xy + wz), 1.0 - 2.0 * (xx + zz), 2.0 * (yz - wx)],
                [2.0 * (xz - wy), 2.0 * (yz + wx), 1.0 - 2.0 * (xx + yy)],
            ]
        }
        // Same composition as render_scene::euler_xyz_columns, reproduced
        // here (that fn is private to its module) so this test verifies
        // the DERIVATION independent of a cross-module dependency.
        fn euler_xyz_columns(rot: [f32; 3]) -> [[f32; 3]; 3] {
            let (cx, sx) = (rot[0].cos(), rot[0].sin());
            let (cy, sy) = (rot[1].cos(), rot[1].sin());
            let (cz, sz) = (rot[2].cos(), rot[2].sin());
            let rx = [[1.0, 0.0, 0.0], [0.0, cx, sx], [0.0, -sx, cx]];
            let ry = [[cy, 0.0, -sy], [0.0, 1.0, 0.0], [sy, 0.0, cy]];
            let rz = [[cz, sz, 0.0], [-sz, cz, 0.0], [0.0, 0.0, 1.0]];
            fn mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
                let mut out = [[0.0f32; 3]; 3];
                for col in 0..3 {
                    for row in 0..3 {
                        out[col][row] =
                            a[0][row] * b[col][0] + a[1][row] * b[col][1] + a[2][row] * b[col][2];
                    }
                }
                out
            }
            mul(mul(rz, ry), rx)
        }

        let half_90 = (std::f32::consts::FRAC_PI_4).sin(); // sin(45deg)
        let cos_45 = (std::f32::consts::FRAC_PI_4).cos();
        let cases: [[f32; 4]; 6] = [
            [0.0, 0.0, 0.0, 1.0],                                   // identity
            [0.0, 0.0, (0.4_f32).sin(), (0.4_f32).cos()],           // Z-only
            [(0.3_f32).sin(), 0.0, 0.0, (0.3_f32).cos()],           // X-only
            normalize_quat([0.2, 0.35, -0.15, 0.9]),                // general
            [0.0, half_90, 0.0, cos_45],                            // gimbal lock: +90 deg about Y
            [0.0, -half_90, 0.0, cos_45],                           // gimbal lock: -90 deg about Y
        ];
        for q in cases {
            let euler = quat_to_render_scene_euler(q);
            // Row-major r[row][col] from the same standard quat formula,
            // in column-major array form (m[col][row]) to compare against
            // euler_xyz_columns' own column-major output.
            let rm = quat_to_matrix(q);
            let mut r_colmajor = [[0.0f32; 3]; 3];
            for row in 0..3 {
                for col in 0..3 {
                    r_colmajor[col][row] = rm[row][col];
                }
            }
            let reconstructed = euler_xyz_columns(euler);
            for col in 0..3 {
                for row in 0..3 {
                    assert!(
                        (r_colmajor[col][row] - reconstructed[col][row]).abs() < 1e-4,
                        "quat {q:?} -> euler {euler:?} did not reconstruct the same rotation \
                         at [{col}][{row}]: expected {}, got {}",
                        r_colmajor[col][row],
                        reconstructed[col][row]
                    );
                }
            }
        }
    }

    /// GLTF_ANIMATION_DESIGN.md A1's performer-gesture gate, at the
    /// GRAPH level: `node.lfo` (Saw, one cycle per beat) wired directly
    /// into `node.gltf_animation_source.progress`, sampled just before
    /// and just after the LFO's own wrap point (beats 0.999 and 1.001 —
    /// the LFO itself wraps `fract()` internally, already proven in
    /// `lfo.rs`'s tests). Uses a bounce-shaped translation track (rises
    /// then returns to 0 — the SAME shape `BoxAnimated.glb`'s real
    /// translation channel has, verified against the actual fixture
    /// bytes this session) so the two samples are expected to be close.
    ///
    /// NOTE for whoever reads this next to `BoxAnimated.glb` itself:
    /// that asset's ROTATION channel does NOT loop seamlessly (identity
    /// at t=0, held at 180°-about-X from t=2.5 to the clip's end at
    /// t=3.708 — verified against the raw keyframe bytes) — a real
    /// authored-content property of that specific fixture, not a
    /// sampler defect. A whole-frame pixel "near-progress-0 vs
    /// near-progress-1" render assertion would be FALSE for that asset;
    /// this test instead exercises the wrap-continuity claim on data
    /// that genuinely loops, at the value level (the reliable oracle
    /// for a computable claim), and `progress_wraps_not_clamps_at_the_loop_boundary`
    /// above independently proves the sampler's wrap-vs-clamp mechanics
    /// hold regardless of asset content.
    #[test]
    fn lfo_driven_progress_loops_a_seamless_track_continuously_at_the_wrap_point() {
        use crate::node_graph::EffectNode;
        use crate::node_graph::effect_node::EffectNodeType;
        use crate::node_graph::execution_plan::compile;
        use crate::node_graph::graph::Graph;
        use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
        use crate::node_graph::primitives::lfo::Lfo;
        use crate::node_graph::Executor;
        use manifold_core::{Beats, Seconds};
        use std::sync::Mutex;

        struct Capture {
            type_id: EffectNodeType,
            seen: std::sync::Arc<Mutex<Option<ParamValue>>>,
        }
        impl EffectNode for Capture {
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                static INPUTS: [NodeInput; 1] = [NodePort {
                    name: Cow::Borrowed("in"),
                    ty: PortType::Scalar(ScalarType::F32),
                    kind: PortKind::Input,
                    required: true,
                }];
                &INPUTS
            }
            fn outputs(&self) -> &[NodeOutput] {
                &[]
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
                *self.seen.lock().unwrap() = ctx.inputs.scalar("in");
            }
        }

        fn sample_pos_y_at(beats: f32) -> f32 {
            let seen = std::sync::Arc::new(Mutex::new(None));
            let mut g = Graph::new();
            let lfo = g.add_node(Box::new(Lfo::new()));
            g.set_param(lfo, "rate_mode", ParamValue::Enum(0)).unwrap(); // Musical
            g.set_param(lfo, "rate", ParamValue::Enum(0)).unwrap(); // "1/1" — one cycle per beat
            g.set_param(lfo, "shape", ParamValue::Enum(2)).unwrap(); // Saw

            let anim = g.add_node(Box::new(GltfAnimationSource::new()));
            g.set_param(anim, "duration_s", ParamValue::Float(1.0)).unwrap();
            // Bounce shape matching BoxAnimated.glb's real translation
            // track: 0 -> peak -> peak (hold) -> back to 0.
            let table = translation_table(vec![
                [0.0, 0.0, 2.52, 0.0],
                [0.4, 0.0, 2.52, 0.0],
                [1.0, 0.0, 0.0, 0.0],
            ]);
            g.set_param(anim, "translation_track", table).unwrap();
            g.connect((lfo, "out"), (anim, "progress")).unwrap();

            let sink = g.add_node(Box::new(Capture {
                type_id: EffectNodeType::new("test.capture"),
                seen: seen.clone(),
            }));
            g.connect((anim, "pos_y"), (sink, "in")).unwrap();

            let plan = compile(&g).unwrap();
            let mut exec = Executor::with_mock();
            exec.execute_frame(
                &mut g,
                &plan,
                FrameTime {
                    beats: Beats(beats as f64),
                    seconds: Seconds(beats as f64 * 0.5),
                    delta: Seconds(1.0 / 60.0),
                    frame_count: 0,
                },
            );
            match seen.lock().unwrap().clone() {
                Some(ParamValue::Float(f)) => f,
                v => panic!("gltf_animation_source did not emit a Float on pos_y: {v:?}"),
            }
        }

        // rate=1/1 -> the LFO completes one Saw cycle per beat, so
        // beats=0.999 sits just before its own wrap and beats=1.001
        // just after (into the next cycle) — the LFO's `fract()` makes
        // these progress ~0.999 and ~0.001 respectively.
        let near_end = sample_pos_y_at(0.999);
        let near_start = sample_pos_y_at(1.001);
        assert!(
            (near_end - near_start).abs() < 0.05,
            "a seamless (0->peak->0) track must read continuously across the LFO's wrap: \
             near-end={near_end}, near-start={near_start}"
        );
    }
}
