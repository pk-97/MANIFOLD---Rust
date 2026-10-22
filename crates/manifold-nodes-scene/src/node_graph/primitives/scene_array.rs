//! `node.scene_array` — camera-windowed `Array<InstanceTransform>` generator
//! for the endless corridor (SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN.md D1/D4/D5/D6).
//!
//! Slot `w` ↔ corridor cell `c = base_cell − behind + w`; the cell's transform
//! is a translation `c · cell_size` along `axis` plus deterministic jitter
//! keyed on the Euclidean `c mod pattern_length`. `base_cell` and the live
//! window (`behind`, `ahead`) resolve each frame from the optional `camera:
//! Camera` input: every cell the camera's view reaches, whichever way it
//! looks (`window`). Unwired, the corridor runs from the origin (base_cell 0,
//! behind 8, ahead 22). Capacity is the constant [`WINDOW_CAPACITY`] (32),
//! never a param: the old live `count` sized buffers at plan time and made
//! every live write inert (BUG-757c), and the window derives from the
//! camera's view, so there is nothing left for a count to decide.
//!
//! Wrap purity is arithmetic, not a coupling (D3/D4): travel per loop is
//! `patterns_per_loop · pattern_length` cells and the jitter keys on the same
//! `cell mod pattern_length`, so any integer pair is pure by construction —
//! the jitter_period-divides-stride coupling (BUG-jvlq) stops existing.
//!
//! INV-RTI4 stasis (D6): the key carries `use_camera` because the unwired run
//! and a wired camera parked at cell 0 with far ≥ 20·cell produce identical
//! key fields with different intended content — without it the buffer goes
//! stale on (re)wire. Camera motion inside a cell changes no key field, so a
//! moving camera rewrites the buffer only on cell-boundary crossings.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::camera::{Camera, CameraMode};

pub const AXIS_LABELS: &[&str] = &["+X", "-X", "+Y", "-Y", "+Z", "-Z"];

const NOISE_COMMON: &str = manifold_node_engine::gpu::shader_sources::NOISE_COMMON_WGSL;

/// Fixed output capacity — the whole corridor concept (D5: vertex/descriptor
/// cost flat regardless of speed). Value-level constant, never a param
/// (BUG-757c class: a capacity that follows a live value deadens the card row).
pub const WINDOW_CAPACITY: u32 = 32;

/// Cells kept on the side the camera faces away from — shadow-map
/// look-behind (D1; ~16 object-depths under the D4 gap rule).
const BEHIND: u32 = 8;

/// Live cells either side of the camera's own: BEHIND + ahead + 1 ≤
/// WINDOW_CAPACITY - 1 (D5). A view reaching past this many cells shows the
/// corridor end beyond them — stated, not hidden.
const REACH: u32 = WINDOW_CAPACITY - 2; // 30

/// Unwired-camera default window span: corridor from the origin (D1 run()).
const STANDALONE_AHEAD: u32 = REACH - BEHIND; // 22

/// Cells past the visible span on each side, so a cell whose contents reach
/// into view from next door exists.
const MARGIN: f32 = 2.0;

/// The widest output the side view is sized for: the camera carries no
/// aspect, so the window assumes one this wide (4:1 covers LED walls). A
/// wider output may show the corridor end at its side edges.
const WIDEST_ASPECT: f32 = 4.0;

/// The live window around the wired camera: its cell, and how many cells
/// behind and ahead of it along `axis` exist. The span covers every cell the
/// camera's view reaches up to its far plane, whichever way it looks (an
/// orbit camera looking back down the corridor sees as far as a flyer
/// looking ahead), plus [`MARGIN`] cells each side, and at least [`BEHIND`]
/// cells on the side it faces away from for the shadow maps. Past [`REACH`]
/// cells in all, the side seen further is cut first; a camera that sees far
/// both ways splits the cells evenly.
fn window(cam: &Camera, axis: u32, cell_size: f32) -> (i32, u32, u32) {
    let (k, sign) = match axis {
        0 => (0, 1.0),
        1 => (0, -1.0),
        2 => (1, 1.0),
        3 => (1, -1.0),
        4 => (2, 1.0),
        _ => (2, -1.0),
    };
    // The camera's place and view along the corridor, in cells.
    let at = sign * cam.pos[k] / cell_size;
    let forward = sign * cam.fwd[k];
    let far = cam.far / cell_size;
    let across = |v: [f32; 3]| v[k].abs();
    // How far along the axis the view reaches, each way, from the eye to
    // the far plane's corners.
    let (back, front) = match cam.mode {
        CameraMode::Perspective { fov_y } => {
            let up = (fov_y * 0.5).tan();
            let spread = up * WIDEST_ASPECT * across(cam.right) + up * across(cam.up);
            (far * (spread - forward).max(0.0), far * (forward + spread).max(0.0))
        }
        CameraMode::Orthographic { half_height } => {
            let lateral = half_height * (WIDEST_ASPECT * across(cam.right) + across(cam.up)) / cell_size;
            (lateral + far * (-forward).max(0.0), lateral + far * forward.max(0.0))
        }
    };
    let base = at.floor();
    // Whole cells, counted from the camera's own, NaN and overflow safe.
    let cells = |x: f32| if x.is_nan() { 0 } else { x.clamp(0.0, REACH as f32) as u32 };
    let behind_seen = cells(base - (at - back).floor() + MARGIN);
    let ahead_seen = cells((at + front).floor() - base + MARGIN);
    let looks_ahead = forward >= 0.0;
    let (toward, away) = if looks_ahead { (ahead_seen, behind_seen) } else { (behind_seen, ahead_seen) };
    let (toward, away) = fit(toward, away.max(BEHIND));
    let (behind, ahead) = if looks_ahead { (away, toward) } else { (toward, away) };
    let base = if base.is_nan() { 0 } else { base.clamp(i32::MIN as f32, i32::MAX as f32) as i32 };
    (base, behind, ahead)
}

/// Fit the cells wanted toward and away from the view into [`REACH`]: the
/// larger side gives way first, never the away side below [`BEHIND`], and
/// two sides both past half share it evenly.
fn fit(toward: u32, away: u32) -> (u32, u32) {
    if toward + away <= REACH {
        return (toward, away);
    }
    let half = REACH / 2;
    if toward >= half && away >= half {
        (REACH - half, half)
    } else if toward > away {
        (REACH - away, away)
    } else {
        (toward, REACH - toward)
    }
}

/// Generated-codegen uniform layout: params in PARAMS order —
/// pattern_length (Int→i32), axis (Enum→u32), cell_size (f32),
/// jitter_seed (Int→i32), jitter_amount (f32) — then the derived fields
/// base_cell (i32), behind (u32), ahead (u32), use_camera (u32), then the
/// codegen-injected dispatch_count (= output capacity), then the codegen's
/// 16-byte-alignment pad. 12 words = 48 bytes (BUG-gx6i: the proof caught
/// this struct missing the pad — the WGSL Params is 48 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneArrayUniforms {
    pattern_length: i32,
    axis: u32,
    cell_size: f32,
    jitter_seed: i32,
    jitter_amount: f32,
    base_cell: i32,
    behind: u32,
    ahead: u32,
    use_camera: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

/// INV-RTI4 (RT_INSTANCING_DESIGN.md) producer stasis: the kernel's FULL
/// input tuple — every resolved value that feeds the uniforms, compared
/// fixed-size on the hot path. When unchanged, `run` skips the buffer
/// rewrite and declares `mark_outputs_unchanged`, so the output slot's
/// write generation HOLDS — which is exactly what lets the RT accel key
/// hold across static frames ("static instances are free"). `rebuild_epoch`
/// folds in the executor lifetime (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md
/// D6): a state-carrying rebuild resets generation counters, and a stale
/// key from the old executor must never match the new one's low numbers.
#[derive(Clone, Copy, PartialEq)]
pub struct SceneArrayStasisKey {
    pub use_camera: bool,
    pub base_cell: i32,
    pub behind: u32,
    pub ahead: u32,
    pub pattern_length: u32,
    pub axis: u32,
    pub cell_size: f32,
    pub jitter_seed: u32,
    pub jitter_amount: f32,
    pub rebuild_epoch: u64,
}

manifold_node_engine::primitive! {
    name: SceneArray,
    type_id: "node.scene_array",
    purpose: "Camera-windowed Array<InstanceTransform> for the endless corridor (SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN.md). Slot w maps to corridor cell c = base_cell - BEHIND + w; the cell's transform is a translation c * cell_size along axis (+X/-X/+Y/-Y/+Z/-Z) plus optional deterministic jitter (rotation +/-jitter_amount rad per axis, scale 1 +/- jitter_amount/2) keyed on the Euclidean (c mod pattern_length) mixed with jitter_seed. The optional camera: Camera input drives the window each frame: base_cell = floor(axis component of camera.pos / cell_size), and the window spans every cell the camera's view reaches up to its far plane, ahead, behind and across (whichever way it looks), plus a margin, keeping a few cells on the side it faces away from for the shadow maps; past the buffer the side seen further is cut first. Every cell that can render exists in the buffer, so no edge hole opens. Unwired, the corridor runs from the origin (base_cell 0, behind 8, ahead 22). Output capacity is the constant 32 (WINDOW_CAPACITY), never a param — surplus slots mask to zero-scale. Wrap purity is arithmetic: the loop camera travels patterns_per_loop * pattern_length cells per loop and the jitter keys on the same cell mod pattern_length, so any integer pair is pure (the jitter_period-divides-stride coupling is gone). The same node feeds ALL object groups. Pointwise atom on the freeze codegen path.",
    inputs: {
        // D2: the camera arrives as one Camera port — the atom picks the axis
        // component from its own axis param, so axis and position can never
        // desync. Unwired: standalone corridor from the origin.
        camera: Camera optional,
    },
    outputs: {
        out: Array(InstanceTransform),
    },
    params: [
        // How many distinct cells before the pattern repeats (card row
        // "Pattern", P2 surface). Also the jitter period — ONE index drives
        // both the cell's place in the pattern and its jitter, so variation
        // and wrap safety can no longer be set independently (D4).
        ParamDef {
            name: Cow::Borrowed("pattern_length"),
            label: "Pattern",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((1.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(4), // +Z
            range: None,
            enum_values: AXIS_LABELS,
        },
        ParamDef {
            name: Cow::Borrowed("cell_size"),
            label: "Cell Size",
            ty: ParamType::Float,
            default: ParamValue::Float(10.0),
            range: Some((0.01, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("jitter_seed"),
            label: "Jitter Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 32767.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("jitter_amount"),
            label: "Jitter",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Optional camera: Camera port drives the window (D2) — wire node.loop_camera.out here for the corridor; the atom resolves the axis component itself so axis and position cannot desync. Unwired, the corridor runs from the origin (base_cell 0, behind 8, ahead 22). Output capacity is the constant 32, never a param: the buffer is fixed at plan pre-allocation (BUG-757c), and the live window span derives from what the wired camera sees, every way it looks — the body masks slots at or beyond behind+ahead+1 to zero-scale. The same cell_size value feeds both this node and node.loop_camera — the plan builder computes it once from scene_bounds so camera travel per loop equals instance spacing by construction. pattern_length is both the pattern period and the jitter period (D4); the loop camera's travel (patterns_per_loop * pattern_length cells) is an integer multiple of it, so the wrap is pure for any integers. jitter_seed stays an internal re-roll knob the plan stamps at 0.",
    examples: [],
    picker: { label: "Scene Array", category: Atom },
    summary: "Generates the corridor of instances around the camera — cells repeat by pattern, forever.",
    category: Geometry3D,
    role: Source,
    aliases: ["scene array", "instance line", "loop copies", "corridor"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/scene_array_body.wgsl"),
    derived_uniforms: [
        "base_cell:i32",
        "behind:u32",
        "ahead:u32",
        "use_camera:u32",
    ],
    wgsl_includes: [NOISE_COMMON],
    extra_fields: {
        // INV-RTI4 stasis cache — see `SceneArrayStasisKey` and `run`.
        stasis_key: Option<SceneArrayStasisKey> = None,
    },
}

impl Primitive for SceneArray {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        // INV-EC2: the capacity is the constant WINDOW_CAPACITY for any
        // params — it was never allowed to follow a live value (BUG-757c),
        // and the corridor has no count-shaped param left to derive it from
        // (the window span is camera-derived, D5).
        Some(WINDOW_CAPACITY)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pattern_length = ctx
            .params
            .get("pattern_length")
            .and_then(|v| v.as_u32_clamped(1))
            .unwrap_or(1)
            .clamp(1, 8);
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 4, // +Z
        };
        let cell_size = ctx.param_f32("cell_size", 10.0);

        let jitter_seed = ctx.params.get("jitter_seed").and_then(|v| v.as_u32_clamped(0)).unwrap_or(0);
        let jitter_amount = ctx.scalar_or_param("jitter_amount", 0.0).clamp(0.0, 1.0);

        // D2/D5: the wired camera drives the window. The atom picks the axis
        // component from its own axis param, so axis and position cannot
        // desync. Unwired: the standalone corridor from the origin.
        let cam = ctx.inputs.camera("camera");
        let use_camera = cam.is_some();
        let (base_cell, behind, ahead) = match cam {
            Some(c) => window(&c, axis, cell_size),
            None => (0, BEHIND, STANDALONE_AHEAD),
        };

        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let item_size = std::mem::size_of::<InstanceTransform>() as u64;
        let capacity = (out_buf.size / item_size) as u32;

        // INV-RTI4 stasis: every frame-varying input is in the key — params,
        // the resolved window (base_cell/behind/ahead), AND use_camera (D6: the
        // unwired run and a wired camera parked at cell 0 with far >=
        // 20*cell collide on every other field; without use_camera the
        // buffer would go stale on (re)wire). Camera motion inside a cell
        // changes none of them → no rewrite → the RT accel key holds.
        let stasis = SceneArrayStasisKey {
            use_camera,
            base_cell,
            behind,
            ahead,
            pattern_length,
            axis,
            cell_size,
            jitter_seed,
            jitter_amount,
            rebuild_epoch: ctx.rebuild_epoch,
        };
        let content_unchanged = self.stasis_key == Some(stasis);
        if content_unchanged && ctx.outputs_retained() {
            ctx.mark_outputs_unchanged();
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);

        let uniforms = SceneArrayUniforms {
            pattern_length: pattern_length as i32,
            axis,
            cell_size,
            jitter_seed: jitter_seed as i32,
            jitter_amount,
            base_cell,
            behind,
            ahead,
            use_camera: use_camera as u32,
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.scene_array",
        );
        self.stasis_key = Some(stasis);
        if content_unchanged { ctx.mark_output_content_unchanged(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INV-EC2: output capacity is the constant WINDOW_CAPACITY for ANY
    /// params — the value-level constant the buffer is pre-allocated at
    /// (BUG-757c class: a capacity that followed a live value deadened the
    /// card row; the corridor has no count-shaped param left at all).
    #[test]
    fn output_capacity_is_window_capacity_not_param_derived() {
        use manifold_node_engine::exec::effect_node::ParamValues;
        let prim = SceneArray::new();
        assert_eq!(WINDOW_CAPACITY, 32);

        for pattern_length in [1.0, 3.0, 8.0] {
            let mut params = ParamValues::default();
            params.insert(
                std::borrow::Cow::Borrowed("pattern_length"),
                ParamValue::Float(pattern_length),
            );
            assert_eq!(
                Primitive::array_output_capacity(&prim, "out", &params, &[]),
                Some(WINDOW_CAPACITY),
                "capacity must be the constant 32 (live pattern_length={pattern_length})"
            );
        }

        let params = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &[]),
            Some(WINDOW_CAPACITY),
            "capacity must be the constant 32 even with no params at all"
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &params, &[]),
            None,
            "a nonexistent port carries no capacity"
        );
    }

    /// BUG-0jqrh: the live window covers what the camera sees whichever way
    /// it looks down the corridor, and keeps the shadow look-behind on the
    /// side it faces away from. A flyer looking ahead keeps the D5 window:
    /// BEHIND cells back, ceil(far/cell) + 2 ahead.
    #[test]
    fn window_covers_every_way_the_camera_looks() {
        use std::f32::consts::{FRAC_PI_2, PI};
        let cell = 10.0;
        let at = [0.0, 0.0, 35.0];
        // yaw PI looks along +Z, yaw 0 along -Z.
        let ahead = Camera::from_pos_euler(at, PI, 0.0, 0.0, 0.8, 0.05, 40.0);
        assert_eq!(window(&ahead, 4, cell), (3, BEHIND, 6));
        let back = Camera::from_pos_euler(at, 0.0, 0.0, 0.0, 0.8, 0.05, 40.0);
        assert_eq!(window(&back, 4, cell), (3, 6, BEHIND));
        // Seeing past the window: the seen side takes what the shadows leave.
        let far_back = Camera { far: 10_000.0, ..back };
        assert_eq!(window(&far_back, 4, cell), (3, REACH - BEHIND, BEHIND));
        // Looking across a long corridor: both ways, evenly.
        let across = Camera::from_pos_euler(at, FRAC_PI_2, 0.0, 0.0, 0.8, 0.05, 10_000.0);
        let (_, behind, ahead_cells) = window(&across, 4, cell);
        assert_eq!((behind, ahead_cells), (REACH / 2, REACH / 2));
        // A -Z corridor counts its cells the other way.
        assert_eq!(window(&back, 5, cell).0, -4);
        // Absurd values still fit the capacity.
        for far in [f32::INFINITY, f32::NAN, 0.0] {
            let (_, behind, ahead_cells) = window(&Camera { far, ..back }, 4, cell);
            assert!(behind + ahead_cells < WINDOW_CAPACITY, "far {far}");
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    /// Bit-exact Rust port of `hash_u32` in
    /// generators/shaders/noise_common.wgsl — the CPU oracle for the
    /// jitter hash (same wrapping arithmetic, same f32 division).
    fn hash_u32(n: u32) -> f32 {
        let mut x = n;
        x ^= x >> 16;
        x = x.wrapping_mul(0x45d9f3b);
        x ^= x >> 16;
        x = x.wrapping_mul(0x45d9f3b);
        x ^= x >> 16;
        x as f32 / 4294967295.0
    }

    /// CPU oracle: the expected window for the given params, mirroring
    /// scene_array_body.wgsl field-for-field. Slot w maps to cell
    /// `c = base_cell - behind + w`; slots at or beyond behind+ahead+1 are
    /// zero-scale. The jitter key is the Euclidean `c rem pattern_length`
    /// (Rust rem_euclid == the body's ((c % p) + p) % p — WGSL % truncates,
    /// so the body's fixup is load-bearing, D4 review finding 1).
    fn cpu_scene_array_window(
        pattern_length: u32,
        axis: u32,
        cell_size: f32,
        jitter_seed: u32,
        jitter_amount: f32,
        base_cell: i32,
        behind: u32,
        ahead: u32,
        capacity: u32,
    ) -> Vec<InstanceTransform> {
        (0..capacity)
            .map(|w| {
                if w > behind + ahead {
                    return InstanceTransform { pos_scale: [0.0; 4], rot_pad: [0.0; 4] };
                }
                let c = base_cell - behind as i32 + w as i32;
                let t = c as f32 * cell_size;
                let mut pos_scale = [0.0f32; 4];
                let mut rot_pad = [0.0f32; 4];
                pos_scale[3] = 1.0; // unit scale
                match axis {
                    0 => pos_scale[0] = t,  // +X
                    1 => pos_scale[0] = -t, // -X
                    2 => pos_scale[1] = t,  // +Y
                    3 => pos_scale[1] = -t, // -Y
                    4 => pos_scale[2] = t,  // +Z
                    5 => pos_scale[2] = -t, // -Z
                    _ => pos_scale[2] = t,
                }
                if jitter_amount > 0.0 {
                    let j = c.rem_euclid(pattern_length.max(1) as i32) as u32;
                    let k = j.wrapping_mul(3).wrapping_add(jitter_seed.wrapping_mul(7919));
                    rot_pad[0] = (hash_u32(k) - 0.5) * 2.0 * jitter_amount;
                    rot_pad[1] = (hash_u32(k + 1) - 0.5) * 2.0 * jitter_amount;
                    rot_pad[2] = (hash_u32(k + 2) - 0.5) * 2.0 * jitter_amount;
                    pos_scale[3] = 1.0 + (hash_u32(k + 3) - 0.5) * jitter_amount;
                }
                InstanceTransform { pos_scale, rot_pad }
            })
            .collect()
    }

    fn dispatch(
        device: &manifold_gpu::GpuDevice,
        pipeline: &manifold_gpu::GpuComputePipeline,
        pattern_length: u32,
        axis: u32,
        cell_size: f32,
        jitter_seed: u32,
        jitter_amount: f32,
        base_cell: i32,
        behind: u32,
        ahead: u32,
    ) -> Vec<InstanceTransform> {
        let capacity = WINDOW_CAPACITY;
        let out_buf = device.create_buffer_shared(capacity as u64 * 32);
        let mut enc = device.create_encoder("scene_array_test");
        let uniforms = SceneArrayUniforms {
            pattern_length: pattern_length as i32,
            axis,
            cell_size,
            jitter_seed: jitter_seed as i32,
            jitter_amount,
            base_cell,
            behind,
            ahead,
            use_camera: 1,
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
        };
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &out_buf, offset: 0 },
            ],
            [capacity.div_ceil(256), 1, 1],
            "scene_array_test",
        );
        enc.commit_and_wait_completed();

        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let gpu_data =
            unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, capacity as usize) };
        gpu_data.to_vec()
    }

    fn assert_matches_cpu(gpu_data: &[InstanceTransform], expected: &[InstanceTransform], ctx: &str) {
        assert_eq!(gpu_data.len(), expected.len(), "{ctx}: length mismatch");
        for (i, (g, e)) in gpu_data.iter().zip(expected.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (g.pos_scale[c] - e.pos_scale[c]).abs() < 1e-6,
                    "{ctx} slot {i} pos_scale[{c}]: gpu={} expected={}",
                    g.pos_scale[c],
                    e.pos_scale[c]
                );
                assert!(
                    (g.rot_pad[c] - e.rot_pad[c]).abs() < 1e-6,
                    "{ctx} slot {i} rot_pad[{c}]: gpu={} expected={}",
                    g.rot_pad[c],
                    e.rot_pad[c]
                );
            }
        }
    }

    fn field_eq(a: &InstanceTransform, b: &InstanceTransform) -> bool {
        a.pos_scale == b.pos_scale && a.rot_pad == b.rot_pad
    }

    /// Window placement: GPU vs the CPU oracle at multiple base_cells
    /// INCLUDING negative (the plan's home = -cell/2 puts base_cell = -1 at
    /// phase 0, so every real window straddles cell zero), plus an explicit
    /// spot-check that slot 8 — the first slot, w = behind — sits exactly at
    /// the base cell's position.
    #[test]
    fn scene_array_window_placement_matches_cpu_including_negative_base() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<SceneArray>()
            .expect("scene_array codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, manifold_node_engine::freeze::codegen::ENTRY, "scene_array_test");

        for base_cell in [0i32, 5, -1, -7, -22] {
            let gpu_data = dispatch(&device, &pipeline, 3, 4, 10.0, 0, 0.0, base_cell, BEHIND, STANDALONE_AHEAD);
            let expected = cpu_scene_array_window(3, 4, 10.0, 0, 0.0, base_cell, BEHIND, STANDALONE_AHEAD, WINDOW_CAPACITY);
            assert_matches_cpu(&gpu_data, &expected, "base {base_cell}");

            // Slot 8 (w = behind) is the base cell: pos z = c * cell_size.
            let c = base_cell;
            assert!(
                (gpu_data[BEHIND as usize].pos_scale[2] - c as f32 * 10.0).abs() < 1e-6,
                "slot 8 must sit at the base cell {c}, got {}",
                gpu_data[BEHIND as usize].pos_scale[2]
            );
            // Slot 0 is `behind` cells behind the base cell.
            assert!(
                (gpu_data[0].pos_scale[2] - (c - BEHIND as i32) as f32 * 10.0).abs() < 1e-6,
                "slot 0 must sit at cell {}, got {}",
                c - BEHIND as i32,
                gpu_data[0].pos_scale[2]
            );
        }
    }

    /// BUG-757c mask, corridor edition: with a short window (ahead = 4, as a
    /// far = 2*cell camera would resolve), slots 0..13 are live and match
    /// the CPU oracle; slots 13..32 are zero-scale collapse-to-a-point
    /// elements — invisible in the main pass and the shadow passes alike.
    #[test]
    fn scene_array_masks_slots_beyond_the_window() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<SceneArray>()
            .expect("scene_array codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, manifold_node_engine::freeze::codegen::ENTRY, "scene_array_test");

        let ahead = 4u32;
        let gpu_data = dispatch(&device, &pipeline, 1, 4, 10.0, 0, 0.0, 0, BEHIND, ahead);
        let expected = cpu_scene_array_window(1, 4, 10.0, 0, 0.0, 0, BEHIND, ahead, WINDOW_CAPACITY);
        assert_matches_cpu(&gpu_data, &expected, "window mask");

        let live = (BEHIND + ahead + 1) as usize;
        for (i, t) in gpu_data[live..].iter().enumerate() {
            assert!(
                t.pos_scale == [0.0; 4] && t.rot_pad == [0.0; 4],
                "surplus slot {} (index {}) must be zero-scale, got pos_scale={:?}",
                i,
                live + i,
                t.pos_scale
            );
        }
    }

    /// D4 jitter value proof: with jitter_amount > 0 the GPU window matches
    /// the CPU-computed hash oracle exactly, keyed on the Euclidean
    /// (cell rem pattern_length) of the SIGNED cell index. The window
    /// deliberately straddles cell zero (base_cell = -9 → cells -17..13) —
    /// the region where a truncated mod would hash cell -1 as 0xFFFFFFFF.
    /// Two seeds must disagree; amount 0 must stay identity TRS.
    #[test]
    fn scene_array_jitter_matches_cpu_rem_euclid_oracle() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<SceneArray>()
            .expect("scene_array codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, manifold_node_engine::freeze::codegen::ENTRY, "scene_array_test");

        let period = 4u32;
        for (seed, amount) in [(0u32, 0.6f32), (7, 1.0), (1234, 0.25)] {
            let base_cell = -9i32; // cells -17..+13 cross zero twice
            let gpu_data = dispatch(&device, &pipeline, period, 4, 10.0, seed, amount, base_cell, BEHIND, STANDALONE_AHEAD);
            let expected = cpu_scene_array_window(period, 4, 10.0, seed, amount, base_cell, BEHIND, STANDALONE_AHEAD, WINDOW_CAPACITY);
            assert_matches_cpu(&gpu_data, &expected, "seed {seed} amount {amount}");

            if amount == 1.0 {
                assert!(
                    gpu_data.iter().any(|t| t.rot_pad[0].abs() > 1e-3),
                    "jitter at amount 1.0 must produce nonzero rotation"
                );
            }

            // Cells with the same (c rem P) carry the same rotation/scale —
            // inside one window the residue classes repeat and must agree
            // field-for-field (the wrap-parity mechanism itself).
            let rot_of = |c: i32| -> [f32; 4] {
                let j = c.rem_euclid(period as i32) as u32;
                let k = j.wrapping_mul(3).wrapping_add(seed.wrapping_mul(7919));
                [
                    (hash_u32(k) - 0.5) * 2.0 * amount,
                    (hash_u32(k + 1) - 0.5) * 2.0 * amount,
                    (hash_u32(k + 2) - 0.5) * 2.0 * amount,
                    0.0,
                ]
            };
            for (w, transform) in gpu_data
                .iter()
                .take((BEHIND + STANDALONE_AHEAD) as usize)
                .enumerate()
            {
                let c = base_cell - BEHIND as i32 + w as i32;
                assert_eq!(
                    transform.rot_pad,
                    rot_of(c),
                    "slot {w} (cell {c}) must hash its Euclidean residue"
                );
            }
        }

        // CPU-side seed sensitivity (the GPU half is proven above): same seed
        // re-rolls identically, different seed changes the transforms.
        let same = |a: &[InstanceTransform], b: &[InstanceTransform]| {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| field_eq(x, y))
        };
        let a = cpu_scene_array_window(4, 4, 10.0, 0, 1.0, -3, BEHIND, STANDALONE_AHEAD, WINDOW_CAPACITY);
        let b = cpu_scene_array_window(4, 4, 10.0, 0, 1.0, -3, BEHIND, STANDALONE_AHEAD, WINDOW_CAPACITY);
        let c = cpu_scene_array_window(4, 4, 10.0, 1, 1.0, -3, BEHIND, STANDALONE_AHEAD, WINDOW_CAPACITY);
        assert!(same(&a, &b), "same seed must re-roll identically");
        assert!(!same(&a, &c), "a different seed must change the instance transforms");

        // Zero amount is byte-identical to the no-jitter oracle.
        let zero = cpu_scene_array_window(4, 4, 10.0, 99, 0.0, -3, BEHIND, STANDALONE_AHEAD, WINDOW_CAPACITY);
        let plain = cpu_scene_array_window(4, 4, 10.0, 0, 0.0, -3, BEHIND, STANDALONE_AHEAD, WINDOW_CAPACITY);
        assert!(same(&zero, &plain), "amount 0 must keep identity TRS");
    }

    /// D8.1 — the enforcing wrap-purity gate (review finding 1): the window
    /// at phase 0 equals the window at phase ~1 translated by exactly K·P
    /// cells — slot-by-slot field equality, over NEGATIVE base_cells (the
    /// plan's home = -cell/2 puts base_cell = -1 at phase 0) and P ∈ 2..8.
    /// A truncated-mod jitter hash fails here for every P ≥ 2 while the
    /// pixel gate stays blind (the minimal parity graph has no geometry in
    /// the camera's own cell) — this is the gate that catches that class.
    ///
    /// RED-FIRST verified (P1 brief): with the body's mod temporarily keyed
    /// on the truncated WGSL % (one-frame source change), this test goes RED
    /// (cell -1 hashes as 0xFFFFFFFF ≠ cell P-1 across the seam); with the
    /// Euclidean mod restored it is green.
    #[test]
    fn window_at_phase_0_equals_window_at_phase_1_translated_by_kp_cells() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<SceneArray>()
            .expect("scene_array codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, manifold_node_engine::freeze::codegen::ENTRY, "scene_array_test");

        for p in 2u32..=8u32 {
            for k in [1i32, 2, 3, 5, 8] {
                for base_cell in [-7i32, -1, 0, 5] {
                    let phase_0 = dispatch(&device, &pipeline, p, 4, 10.0, 7, 0.8, base_cell, BEHIND, STANDALONE_AHEAD);
                    // Phase ~1: the camera has travelled exactly K·P cells, so
                    // base_cell has advanced by K·P.
                    let phase_1 = dispatch(&device, &pipeline, p, 4, 10.0, 7, 0.8, base_cell + k * p as i32, BEHIND, STANDALONE_AHEAD);
                    for (w, (a, b)) in phase_0.iter().zip(phase_1.iter()).enumerate() {
                        // Surplus slots mask to zero-scale in BOTH windows —
                        // no translation demand there.
                        if w as u32 > BEHIND + STANDALONE_AHEAD {
                            assert_eq!(a.pos_scale, [0.0; 4], "slot {w} must be masked at phase 0");
                            assert_eq!(b.pos_scale, [0.0; 4], "slot {w} must be masked at phase ~1");
                            continue;
                        }
                        // The window at phase ~1 IS the window at phase 0
                        // translated by exactly K·P cells: jitter (keyed on
                        // the Euclidean residue) and scale must be field-
                        // identical, and the axis position must shift by the
                        // exact travel. A truncated-mod jitter hash fails
                        // the rot/scale equality for every P ≥ 2.
                        assert_eq!(
                            a.rot_pad,
                            b.rot_pad,
                            "P={p} K={k} base={base_cell}: slot {w} rotation differs across the seam \
                             (cell {} vs {} — jitter must key on the Euclidean residue)",
                            base_cell - BEHIND as i32 + w as i32,
                            base_cell + k * p as i32 - BEHIND as i32 + w as i32
                        );
                        assert_eq!(
                            a.pos_scale[3],
                            b.pos_scale[3],
                            "P={p} K={k} base={base_cell}: slot {w} scale differs across the seam"
                        );
                        assert_eq!(
                            a.pos_scale[0], b.pos_scale[0],
                            "P={p} K={k} base={base_cell}: slot {w} x must be unchanged by a +Z travel"
                        );
                        assert_eq!(
                            a.pos_scale[1], b.pos_scale[1],
                            "P={p} K={k} base={base_cell}: slot {w} y must be unchanged by a +Z travel"
                        );
                        let travel = k as f32 * p as f32 * 10.0;
                        assert!(
                            (b.pos_scale[2] - a.pos_scale[2] - travel).abs() < 1e-4,
                            "P={p} K={k} base={base_cell}: slot {w} z must translate by exactly \
                             K·P·cell = {travel}, got delta {}",
                            b.pos_scale[2] - a.pos_scale[2]
                        );
                    }
                }
            }
        }

        // Vacuity guard: different pattern lengths genuinely change the
        // window content (a constant buffer would pass the equality above
        // trivially).
        let p2 = dispatch(&device, &pipeline, 2, 4, 10.0, 7, 0.8, -1, BEHIND, STANDALONE_AHEAD);
        let p5 = dispatch(&device, &pipeline, 5, 4, 10.0, 7, 0.8, -1, BEHIND, STANDALONE_AHEAD);
        assert!(
            p2.iter().zip(p5.iter()).any(|(a, b)| !field_eq(a, b)),
            "pattern_length 2 vs 5 must change the window content"
        );
    }
}
