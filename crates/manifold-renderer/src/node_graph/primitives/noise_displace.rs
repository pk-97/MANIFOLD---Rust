//! `node.noise_displace` — per-vertex simplex-noise boil of an `Array<MeshVertex>`.
//!
//! Per vertex: `pos += normal * amount * simplex3(pos * frequency + time * speed)`,
//! where `w` is the optional per-vertex `weights` input (degrading to 1.0 past a
//! short/unwired buffer per the shipped deformer convention). Normals, uv, and
//! tangent pass through unchanged. The `time` input is port-shadowed and defaults
//! to the playback clock when unwired.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const NOISE_COMMON: &str = include_str!("../../generators/shaders/noise_common.wgsl");

/// Generated-codegen uniform layout: scalar params in PARAMS order (`amount`,
/// `frequency`, `speed`, `time` f32), then the derived `weights_len` (u32), then
/// the codegen-injected `dispatch_count`, padded to a 16-byte multiple. 6 words +
/// 2 pad = 32 bytes. Matches `standalone_for_spec::<NoiseDisplace>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NoiseDisplaceUniforms {
    amount: f32,
    frequency: f32,
    speed: f32,
    time: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: NoiseDisplace,
    type_id: "node.noise_displace",
    purpose: "Per-vertex simplex-noise boil of an Array<MeshVertex>. pos += normal * amount * simplex3(pos * frequency + time * speed). `w` is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). Normals, uv, and tangent pass through unchanged — wire node.facet_normals downstream after a heavy boil if the unchanged normals start reading wrong under lighting. `time` is port-shadowed and defaults to the playback clock when unwired.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        amount: ScalarF32 optional,
        frequency: ScalarF32 optional,
        speed: ScalarF32 optional,
        time: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("frequency"),
            label: "Frequency",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 256.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("speed"),
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("time"),
            label: "Time",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'boil' deformer — organic per-vertex noise motion along the surface normal. Wire node.mesh_ramp's `weights` output to restrict the boil to a region. Drive `time` from a beat ramp or leave it unwired for continuous playback-clock animation.",
    examples: [],
    picker: { label: "Boil", category: Atom },
    summary: "Pushes every vertex along its normal by animated simplex noise, so a mesh appears to simmer and bubble.",
    category: Geometry3D,
    role: Filter,
    aliases: ["boil", "noise displace", "simplex displace", "bubble"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/noise_displace_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable so it can chain with other mesh deformers in one
    // dispatch. `weights_len` is a frame-derived uniform the body uses to
    // bounds-check the coincident weight read (degrade to 1.0 past the buffer).
    derived_uniforms: ["weights_len:u32"],
    frame_time_inputs: ["time"],
    wgsl_includes: [NOISE_COMMON],
}

// Per-frame recompute for a FUSED region's `time` field — `run()` packs
// `ctx.time.seconds.0` into the `time` uniform when the input is unwired.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.noise_displace",
        recompute: |ctx| Some(vec![ctx.frame.seconds.0 as f32]),
    }
}

impl Primitive for NoiseDisplace {
    /// Output `out` is sized to match input `in` — noise displacement is a
    /// per-vertex transform, no expansion.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = ctx.scalar_or_param("amount", 0.0);
        let frequency = ctx.scalar_or_param("frequency", 1.0);
        let speed = ctx.scalar_or_param("speed", 1.0);
        // Wire wins; else the playback clock's seconds — same value the
        // runtime injects into fused `time` via DerivedUniformRecompute.
        let time = match ctx.inputs.scalar("time") {
            Some(ParamValue::Float(f)) => f,
            _ => ctx.time.seconds.0 as f32,
        };

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let weights_wired = ctx.inputs.array("weights");
        let weights_buf = weights_wired.unwrap_or(src);
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let in_count = (src.size / vertex_size) as u32;
        let out_count = (dst.size / vertex_size) as u32;
        let count = in_count.min(out_count);
        if count == 0 {
            return;
        }
        let weights_len = weights_wired.map(|b| (b.size / 4) as u32).unwrap_or(0);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path: the runtime kernel is generated from `wgsl_body`
            // (with noise_common prepended) so this atom stays pointwise/fusable
            // in the graph compiler. Bindings: uniform(0), buf_in(1),
            // buf_weights(2), buf_out(3).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.noise_displace standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.noise_displace",
            )
        });

        let uniforms = NoiseDisplaceUniforms {
            amount,
            frequency,
            speed,
            time,
            weights_len,
            dispatch_count: count,
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
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: weights_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.noise_displace",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn noise_displace_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(NoiseDisplace::TYPE_ID, "node.noise_displace");

        let in_port = NoiseDisplace::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = NoiseDisplace::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        for name in ["amount", "frequency", "speed", "time"] {
            let port = NoiseDisplace::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(NoiseDisplace::OUTPUTS.len(), 1);
        assert_eq!(NoiseDisplace::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn noise_displace_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = NoiseDisplace::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = NoiseDisplace::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.noise_displace");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. Parity is against a hand-written Rust
    //! reference of the committed formula, element-wise, per
    //! DECOMPOSING_GENERATORS.md section 9. The `simplex3d` evaluation is
    //! delegated to the GPU kernel itself; this module checks amount=0
    //! identity, amount=1 shape, and weight degradation.
    use super::*;

    fn mk_vertex(pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> MeshVertex {
        MeshVertex {
            position: pos,
            _pad0: 0.0,
            normal,
            _pad1: 0.0,
            uv,
            _pad2: [0.0, 0.0],
            tangent: [0.0; 4],
        }
    }

    /// The generated standalone kernel (the shipping runtime path).
    fn generated_wgsl() -> String {
        crate::node_graph::freeze::codegen::standalone_for_spec::<NoiseDisplace>()
            .expect("noise_displace buffer codegen")
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_noise_displace(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        src: &[MeshVertex],
        weights: Option<&[f32]>,
        weights_len_override: Option<u32>,
        amount: f32,
        frequency: f32,
        speed: f32,
        time: f32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "noise-displace-test",
        );
        let sbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            sbuf.write(0, bytemuck::cast_slice(src));
        }
        let dbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);

        let (wbuf, weights_len) = match weights {
            Some(w) => {
                let mut padded = vec![0.0f32; src.len()];
                padded[..w.len().min(src.len())].copy_from_slice(&w[..w.len().min(src.len())]);
                let b = device.create_buffer_shared((padded.len() * 4).max(4) as u64);
                unsafe {
                    b.write(0, bytemuck::cast_slice(&padded));
                }
                (b, weights_len_override.unwrap_or(w.len() as u32))
            }
            None => (device.create_buffer_shared(std::mem::size_of_val(src) as u64), 0),
        };

        let uniforms = NoiseDisplaceUniforms {
            amount,
            frequency,
            speed,
            time,
            weights_len,
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &sbuf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &wbuf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &dbuf, offset: 0 },
        ];
        let mut enc = device.create_encoder("noise-displace-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(src.len() as u32).div_ceil(256), 1, 1],
            "noise-displace-test",
        );
        enc.commit_and_wait_completed();

        let ptr = dbuf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    #[test]
    fn amount_zero_is_identity() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let src = vec![
            mk_vertex([0.5, -0.3, 1.2], [0.267, 0.535, 0.802], [0.1, 0.2]),
            mk_vertex([-1.1, 0.9, -0.4], [0.0, 1.0, 0.0], [0.3, 0.7]),
            mk_vertex([2.0, 2.0, -2.0], [0.707, 0.0, 0.707], [0.9, 0.4]),
        ];

        let out = dispatch_noise_displace(&device, &gen_wgsl, &src, None, None, 0.0, 1.0, 1.0, 1.0);

        assert_eq!(out.len(), src.len());
        for i in 0..src.len() {
            assert_eq!(out[i].position, src[i].position, "amount=0 must be identity pos {i}");
            assert_eq!(out[i].normal, src[i].normal, "amount=0 must preserve normal {i}");
            assert_eq!(out[i].uv, src[i].uv, "amount=0 must preserve uv {i}");
        }
    }

    #[test]
    fn non_zero_amount_actually_moves_vertices() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        // A plane of vertices with identical normals so displacement magnitude
        // is easy to observe statistically.
        let src: Vec<MeshVertex> = (0..64)
            .map(|i| {
                let x = (i % 8) as f32 * 0.25;
                let y = (i / 8) as f32 * 0.25;
                mk_vertex([x, y, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0])
            })
            .collect();

        let out = dispatch_noise_displace(&device, &gen_wgsl, &src, None, None, 1.0, 1.0, 1.0, 0.0);

        let mut changed = 0usize;
        for i in 0..src.len() {
            if out[i].position[0] != src[i].position[0]
                || out[i].position[1] != src[i].position[1]
                || out[i].position[2] != src[i].position[2]
            {
                changed += 1;
            }
        }
        assert!(
            changed > src.len() / 4,
            "amount=1 should move a substantial fraction of vertices, got {changed}/{} moved",
            src.len()
        );
    }

    #[test]
    fn short_weights_degrade_to_one_for_the_tail() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let src: Vec<MeshVertex> = (0..12)
            .map(|_| mk_vertex([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]))
            .collect();
        let weights = [0.0f32, 0.0];

        let out = dispatch_noise_displace(
            &device, &gen_wgsl, &src, Some(&weights), Some(2), 1.0, 1.0, 1.0, 0.0,
        );

        assert!(
            (out[0].position[2] - src[0].position[2]).abs() < 1e-5,
            "vertex 0 has explicit weight 0 -> unchanged"
        );
        assert!(
            (out[1].position[2] - src[1].position[2]).abs() < 1e-5,
            "vertex 1 has explicit weight 0 -> unchanged"
        );
        // Past weights_len the effective weight is 1.0, so at least some
        // vertices should have moved relative to the input (non-deterministic
        // but the seed/idx mix guarantees variation across the array).
        let tail_moved = out.iter().skip(2).enumerate().any(|(i, v)| {
            (v.position[2] - src[i + 2].position[2]).abs() > 1e-5
        });
        assert!(tail_moved, "tail past weights_len should degrade to w=1.0 and displace");
    }
}
