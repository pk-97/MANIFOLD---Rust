//! `node.scene_array` — emit a linear `Array<InstanceTransform>` along one
//! axis, for scene-loop instancing (SCENE_LOOP_DESIGN.md D2,
//! SCENE_MODIFIER_FRAMEWORK P4 jitter).
//!
//! One instance per copy, translated `i * cell_size` along the chosen axis.
//! The same node feeds ALL object groups — copy count changes are one param
//! write, not N. Optional per-instance jitter (rotation + scale from a
//! deterministic hash of the instance index — no time dependence, trivially
//! wrap-safe per SCENE_LOOP INV-3). Source atom on the freeze codegen path.
//!
//! The `out` buffer is sized for `count`'s FULL range (8), never the current
//! value: capacity is fixed at plan pre-allocation while `count` is a live
//! card-row write (the Scene Loop "Copies" row, and the Stride row's coupled
//! count secondary) — sizing by the value made every live count write inert
//! until a structural rebuild (BUG-757c). The body masks slots at or beyond
//! the live count to zero-scale, so surplus capacity draws nothing.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::{Primitive, PrimitiveSpec};

pub const AXIS_LABELS: &[&str] = &["+X", "-X", "+Y", "-Y", "+Z", "-Z"];

const NOISE_COMMON: &str = include_str!("../../generators/shaders/noise_common.wgsl");

/// Generated-codegen uniform layout. Params in PARAMS order:
/// count (Int→i32), axis (Enum→u32), cell_size (f32), jitter_seed (Int→i32),
/// jitter_amount (f32), then dispatch_count (u32), padded to 16 bytes.
/// 8 words = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneArrayUniforms {
    count: i32,
    axis: u32,
    cell_size: f32,
    jitter_seed: i32,
    jitter_amount: f32,
    dispatch_count: u32,
    _pad: u32,
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
    pub count: u32,
    pub axis: u32,
    pub cell_size: f32,
    pub jitter_seed: u32,
    pub jitter_amount: f32,
    pub rebuild_epoch: u64,
}

crate::primitive! {
    name: SceneArray,
    type_id: "node.scene_array",
    purpose: "Linear Array<InstanceTransform> along one axis for scene-loop instancing. count copies, each translated i * cell_size along axis (+X/-X/+Y/-Y/+Z/-Z). The same node feeds ALL object groups — copy count changes are one param write, not N. Optional per-instance jitter (rotation ±jitter_amount rad per axis, scale 1 ± jitter_amount/2) from a deterministic hash of the instance index mixed with jitter_seed — no time dependence, trivially wrap-safe. Source atom on the freeze codegen path.",
    inputs: {},
    outputs: {
        out: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("count"),
            label: "Count",
            ty: ParamType::Int,
            default: ParamValue::Float(3.0),
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
        // ── SCENE_MODIFIER_FRAMEWORK P4 jitter. Deterministic per-instance
        // rotation/scale from a hash of the INSTANCE INDEX mixed with the
        // seed (WGSL body) — no time dependence, so the array is identical
        // every frame and trivially wrap-safe (SCENE_LOOP INV-3). Zero
        // amount keeps the identity-TRS behaviour byte-identical to P3.
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
    composition_notes: "Source atom — no inputs. The out buffer is sized for count's full range (8), never the current value: buffer capacity is fixed at plan pre-allocation, so a value-sized buffer made every live count write inert until a rebuild (BUG-757c — the Scene Loop 'Copies' row and the Stride coupling both write count live). The body masks slots beyond the live count to zero-scale. The same cell_size value feeds both this node and node.loop_camera — the plan builder computes it once from scene_bounds so camera travel per loop equals instance spacing by construction (SCENE_LOOP_DESIGN D4). The Scene Loop card's Jitter row writes jitter_amount; jitter_seed stays an internal re-roll knob the plan stamps at 0.",
    examples: [],
    picker: { label: "Scene Array", category: Atom },
    summary: "Lays out copies in a line along one axis, spacing them evenly for a looping flythrough.",
    category: Geometry3D,
    role: Source,
    aliases: ["scene array", "instance line", "loop copies"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/scene_array_body.wgsl"),
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
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        // BUG-757c: size for count's FULL range, never the current value.
        // Buffer capacity is fixed at plan pre-allocation, and count is a
        // live card-row write (Scene Loop "Copies" / the Stride coupling's
        // secondary) applied in place with no rebuild — a value-sized buffer
        // made every live count write inert. The body masks slots at or
        // beyond the live count to zero-scale, so the surplus capacity
        // renders nothing.
        let range_max = SceneArray::PARAMS
            .iter()
            .find(|p| p.name == "count")
            .and_then(|p| p.range)
            .map(|(_, max)| max as u32)?;
        Some(range_max)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let count = match ctx.params.get("count") {
            Some(ParamValue::Float(n)) => (*n).round().clamp(1.0, 8.0) as u32,
            _ => 3,
        };
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 4, // +Z
        };
        let cell_size = match ctx.params.get("cell_size") {
            Some(ParamValue::Float(f)) => *f,
            _ => 10.0,
        };
        let jitter_seed = ctx.params.get("jitter_seed").and_then(|v| v.as_u32_clamped(0)).unwrap_or(0);
        let jitter_amount = ctx.scalar_or_param("jitter_amount", 0.0).clamp(0.0, 1.0);

        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let item_size = std::mem::size_of::<InstanceTransform>() as u64;
        let capacity = (out_buf.size / item_size) as u32;
        let count = count.min(capacity);

        // INV-RTI4 stasis: every frame this node's output depends on is in
        // the key — skip the rewrite when unchanged and declare the output
        // unchanged, so the slot's write generation (and with it the RT
        // accel key) holds across static frames. First evaluate after
        // wiring, and every param write, misses the cache by construction.
        let stasis = SceneArrayStasisKey {
            count,
            axis,
            cell_size,
            jitter_seed,
            jitter_amount,
            rebuild_epoch: ctx.rebuild_epoch,
        };
        if self.stasis_key == Some(stasis) {
            ctx.mark_outputs_unchanged();
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.scene_array standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.scene_array",
            )
        });

        let uniforms = SceneArrayUniforms {
            count: count as i32,
            axis,
            cell_size,
            jitter_seed: jitter_seed as i32,
            jitter_amount,
            dispatch_count: capacity,
            _pad: 0,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scene_array_declares_zero_inputs_and_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(SceneArray::TYPE_ID, "node.scene_array");
        assert!(SceneArray::INPUTS.is_empty());
        assert_eq!(SceneArray::OUTPUTS.len(), 1);
        assert_eq!(SceneArray::OUTPUTS[0].name, "out");
        assert_eq!(SceneArray::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn scene_array_has_five_params() {
        let names: Vec<&str> = SceneArray::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["count", "axis", "cell_size", "jitter_seed", "jitter_amount"]
        );
    }

    #[test]
    fn axis_enum_has_six_options() {
        let axis_param = SceneArray::PARAMS
            .iter()
            .find(|p| p.name == "axis")
            .expect("axis param");
        assert_eq!(axis_param.ty, ParamType::Enum);
        assert_eq!(axis_param.enum_values.len(), 6);
    }

    /// BUG-757c regression, the capacity layer of "the Copies row does
    /// nothing": count is a LIVE card-row write (Scene Loop "Copies", and
    /// the Stride row's coupled secondary), but an Array<T> buffer's
    /// capacity is fixed at plan pre-allocation — sizing the buffer by the
    /// count VALUE made every live write inert until a structural rebuild
    /// (the card showed the new value; the frame never changed). The buffer
    /// must be sized for count's full range so the live count uniform, not
    /// the allocation, decides how many instances draw.
    #[test]
    fn output_capacity_is_count_range_max_not_current_value() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = SceneArray::new();
        let range_max = SceneArray::PARAMS
            .iter()
            .find(|p| p.name == "count")
            .and_then(|p| p.range)
            .map(|(_, max)| max as u32)
            .expect("count declares a range");

        // Any live count value — including the apply-time default — must
        // resolve to the same capacity.
        for count in [1.0, 3.0, 8.0] {
            let mut params = ParamValues::default();
            params.insert(
                std::borrow::Cow::Borrowed("count"),
                ParamValue::Float(count),
            );
            assert_eq!(
                Primitive::array_output_capacity(&prim, "out", &params, &[]),
                Some(range_max),
                "capacity must not follow the count value (live count={count})"
            );
        }

        let params = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &params, &[]),
            None,
            "a nonexistent port carries no capacity"
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SceneArray::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scene_array");
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

    /// CPU oracle: compute the expected InstanceTransform array for given
    /// params. Mirrors scene_array_body.wgsl exactly — axis translation,
    /// then the index-hash jitter branch (identity TRS when amount == 0).
    fn cpu_scene_array(count: u32, axis: u32, cell_size: f32) -> Vec<InstanceTransform> {
        cpu_scene_array_jitter(count, axis, cell_size, 0, 0.0)
    }

    fn cpu_scene_array_jitter(
        count: u32,
        axis: u32,
        cell_size: f32,
        jitter_seed: u32,
        jitter_amount: f32,
    ) -> Vec<InstanceTransform> {
        (0..count)
            .map(|i| {
                let t = i as f32 * cell_size;
                let mut pos_scale = [0.0f32; 4];
                let mut rot_pad = [0.0f32; 4];
                pos_scale[3] = 1.0; // unit scale
                match axis {
                    0 => pos_scale[0] = t, // +X
                    1 => pos_scale[0] = -t, // -X
                    2 => pos_scale[1] = t, // +Y
                    3 => pos_scale[1] = -t, // -Y
                    4 => pos_scale[2] = t, // +Z
                    5 => pos_scale[2] = -t, // -Z
                    _ => pos_scale[2] = t,
                }
                if jitter_amount > 0.0 {
                    let k = i.wrapping_mul(3).wrapping_add(jitter_seed.wrapping_mul(7919));
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
        count: u32,
        axis: u32,
        cell_size: f32,
        jitter_seed: u32,
        jitter_amount: f32,
    ) -> Vec<InstanceTransform> {
        let capacity = count;
        let out_buf = device.create_buffer_shared(capacity as u64 * 32);
        let mut enc = device.create_encoder("scene_array_test");
        let uniforms = SceneArrayUniforms {
            count: count as i32,
            axis,
            cell_size,
            jitter_seed: jitter_seed as i32,
            jitter_amount,
            dispatch_count: capacity,
            _pad: 0,
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

    #[test]
    fn scene_array_matches_cpu_all_axes() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<SceneArray>()
            .expect("scene_array codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, crate::node_graph::freeze::codegen::ENTRY, "scene_array_test");

        for axis in 0u32..6u32 {
            let count = 5u32;
            let cell_size = 7.5f32;
            let gpu_data = dispatch(&device, &pipeline, count, axis, cell_size, 0, 0.0);
            let expected = cpu_scene_array(count, axis, cell_size);
            assert_matches_cpu(&gpu_data, &expected, "axis {axis}");
        }
    }

    /// BUG-757c value proof: the buffer is sized for count's full range while
    /// the live count decides how many instances are real. With an
    /// 8-capacity buffer and count=3, slots 0..3 match the CPU oracle and
    /// slots 3..8 are zero-scale collapse-to-a-point elements (invisible in
    /// render_scene's instance draw) — the pre-fix body wrote a full-strength
    /// transform into every slot, so a value-sized buffer was the only thing
    /// keeping surplus slots off screen.
    #[test]
    fn scene_array_masks_slots_beyond_live_count() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<SceneArray>()
            .expect("scene_array codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, crate::node_graph::freeze::codegen::ENTRY, "scene_array_test");

        let capacity = 8u32;
        let count = 3u32;
        let out_buf = device.create_buffer_shared(capacity as u64 * 32);
        let mut enc = device.create_encoder("scene_array_mask_test");
        let uniforms = SceneArrayUniforms {
            count: count as i32,
            axis: 4, // +Z
            cell_size: 10.0,
            jitter_seed: 0,
            jitter_amount: 0.0,
            dispatch_count: capacity,
            _pad: 0,
        };
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &out_buf, offset: 0 },
            ],
            [capacity.div_ceil(256), 1, 1],
            "scene_array_mask_test",
        );
        enc.commit_and_wait_completed();

        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let gpu_data =
            unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, capacity as usize) };

        let expected = cpu_scene_array(count, 4, 10.0);
        assert_matches_cpu(&gpu_data[..count as usize], &expected, "live count slots");

        for (i, t) in gpu_data[count as usize..].iter().enumerate() {
            assert!(
                t.pos_scale == [0.0; 4] && t.rot_pad == [0.0; 4],
                "surplus slot {} (index {}) must be zero-scale, got pos_scale={:?}",
                i,
                count as usize + i,
                t.pos_scale
            );
        }
    }

    /// P4 jitter value proof: with jitter_amount > 0, the GPU array matches
    /// the CPU-computed hash oracle exactly — rotation ±amount rad per axis,
    /// scale 1 ± amount/2, keyed by (index, seed). Two seeds must disagree
    /// (the seed re-rolls), and amount 0 must stay identity TRS.
    #[test]
    fn scene_array_jitter_matches_cpu_hash_oracle() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<SceneArray>()
            .expect("scene_array codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, crate::node_graph::freeze::codegen::ENTRY, "scene_array_test");

        for (seed, amount) in [(0u32, 0.6f32), (7, 1.0), (1234, 0.25)] {
            let count = 8u32;
            let gpu_data = dispatch(&device, &pipeline, count, 4, 10.0, seed, amount);
            let expected = cpu_scene_array_jitter(count, 4, 10.0, seed, amount);
            assert_matches_cpu(&gpu_data, &expected, "seed {seed} amount {amount}");

            // Jitter is live: rotation is nonzero at full amount.
            if amount == 1.0 {
                assert!(
                    gpu_data.iter().any(|t| t.rot_pad[0].abs() > 1e-3),
                    "jitter at amount 1.0 must produce nonzero rotation"
                );
            }
        }

        // Determinism + seed sensitivity on the CPU oracle (the GPU half is
        // proven above): same seed → identical, different seed → different.
        // Field-wise (InstanceTransform carries no Debug/PartialEq).
        let same = |a: &[InstanceTransform], b: &[InstanceTransform]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| {
                    x.pos_scale == y.pos_scale && x.rot_pad == y.rot_pad
                })
        };
        let a = cpu_scene_array_jitter(4, 4, 10.0, 0, 1.0);
        let b = cpu_scene_array_jitter(4, 4, 10.0, 0, 1.0);
        let c = cpu_scene_array_jitter(4, 4, 10.0, 1, 1.0);
        assert!(same(&a, &b), "same seed must re-roll identically");
        assert!(
            !same(&a, &c),
            "a different seed must change the instance transforms"
        );

        // Zero amount is byte-identical to the no-jitter oracle.
        let zero = cpu_scene_array_jitter(4, 4, 10.0, 99, 0.0);
        let plain = cpu_scene_array(4, 4, 10.0);
        assert!(same(&zero, &plain), "amount 0 must keep identity TRS");
    }
}
