//! `node.fractal_noise_per_copy` — sample fractal Brownian motion
//! (multi-octave 3D simplex) at each UV in an `Array<vec2<f32>>`,
//! emit `Array<f32>`.
//!
//! Per-instance counterpart to `node.noise` — which samples
//! per-pixel into a Texture2D. This primitive samples per buffer
//! slot into an Array<f32>, the right shape for driving per-instance
//! state in mesh-instancing pipelines (petal displacement on a
//! torus, low-frequency density variation across a particle field).
//!
//! The shader's FBM loop matches `noise_common.wgsl::fbm()` byte-
//! for-byte; with `octaves = 5`, `lacunarity = 1.5`, `gain = 0.8`
//! (the defaults) the output is bit-identical to the legacy fbm.
//! Other settings extend the family — fewer octaves for cheaper
//! computation, larger lacunarity for sparser cells, etc.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`scale`,
/// `z`, `offset_x`, `offset_y`, `octaves` Int → i32, `lacunarity`, `gain`) then
/// the codegen-injected `dispatch_count` (= element count, the guard). 8 words =
/// 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    scale: f32,
    z: f32,
    offset_x: f32,
    offset_y: f32,
    octaves: i32,
    lacunarity: f32,
    gain: f32,
    dispatch_count: u32,
}

const NOISE_COMMON: &str = include_str!("../../generators/shaders/noise_common.wgsl");

crate::primitive! {
    name: FbmPerInstance,
    type_id: "node.fractal_noise_per_copy",
    purpose: "Sample fractal Brownian motion (multi-octave 3D simplex) at each UV in an Array<vec2<f32>>, emit Array<f32>. Per-instance counterpart to node.noise. For each idx: out[idx] = fbm(vec3(uv * scale + offset, z), octaves, lacunarity, gain). The internal loop matches noise_common.wgsl::fbm byte-for-byte; with the defaults (octaves=5, lacunarity=1.5, gain=0.8) the output is bit-identical to the legacy fbm — DigitalPlants's petal-noise pass relies on this. Port-shadow on scale / z / offset_* so the noise field can be animated from time and LFO wires.",
    inputs: {
        uv: Array([f32; 2]) required,
        scale: ScalarF32 optional,
        z: ScalarF32 optional,
        offset_x: ScalarF32 optional,
        offset_y: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("z"),
            label: "Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_x"),
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_y"),
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("octaves"),
            label: "Octaves",
            ty: ParamType::Int,
            default: ParamValue::Float(5.0),
            range: Some((1.0, 12.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("lacunarity"),
            label: "Lacunarity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.5),
            range: Some((1.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("gain"),
            label: "Gain",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Defaults (5 / 1.5 / 0.8) reproduce the legacy noise_common::fbm exactly. Octaves > 1 stacks 3D simplex samples at scaling frequencies; total energy is normalised by Σ amp so the output range stays within roughly [-1, 1]. octaves / lacunarity / gain are structural and not port-shadowed (changing them mid-frame is rarely the move); scale / z / offset_* are port-shadowed for time- and LFO-driven animation. Output capacity follows the input `uv` array.",
    examples: [],
    picker: { label: "Fractal Noise (per copy)", category: Atom },
    summary: "Gives every copy its own fractal-noise value, a smooth random number per copy you can drive size, colour, or motion with.",
    category: Particles2D,
    role: Filter,
    aliases: ["fractal noise", "fbm per instance", "fbm", "per copy", "variation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/fbm_per_instance_body.wgsl"),
    wgsl_includes: [NOISE_COMMON],
}

impl Primitive for FbmPerInstance {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "uv")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = ctx.scalar_or_param("scale", 1.0);
        let z = ctx.scalar_or_param("z", 0.0);
        let offset_x = ctx.scalar_or_param("offset_x", 0.0);
        let offset_y = ctx.scalar_or_param("offset_y", 0.0);
        let octaves = match ctx.params.get("octaves") {
            Some(ParamValue::Float(n)) => n.round().clamp(1.0, 12.0) as u32,
            _ => 5,
        };
        let lacunarity = match ctx.params.get("lacunarity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.5,
        };
        let gain = match ctx.params.get("gain") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.8,
        };

        let Some(uv_buf) = ctx.inputs.array("uv") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let vec2_size = std::mem::size_of::<[f32; 2]>() as u64;
        let f32_size = std::mem::size_of::<f32>() as u64;
        let in_capacity = (uv_buf.size / vec2_size) as u32;
        let out_capacity = (out_buf.size / f32_size) as u32;
        let count = in_capacity.min(out_capacity);
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path; noise_common prepended via wgsl_includes for
            // simplex3d). fbm_per_instance.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.fractal_noise_per_copy standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.fractal_noise_per_copy",
            )
        });

        let uniforms = Uniforms {
            scale,
            z,
            offset_x,
            offset_y,
            octaves: octaves as i32,
            lacunarity,
            gain,
            dispatch_count: count,
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
                    buffer: uv_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.fractal_noise_per_copy",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fbm_per_instance_declares_vec2_in_and_f32_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();
        let f32_layout = ArrayType::of_known::<f32>();
        assert_eq!(FbmPerInstance::TYPE_ID, "node.fractal_noise_per_copy");
        let uv_in = FbmPerInstance::INPUTS
            .iter()
            .find(|p| p.name == "uv")
            .expect("uv input must exist");
        assert!(uv_in.required);
        assert_eq!(uv_in.ty, PortType::Array(vec2_layout));
        assert_eq!(FbmPerInstance::OUTPUTS.len(), 1);
        assert_eq!(FbmPerInstance::OUTPUTS[0].name, "out");
        assert_eq!(FbmPerInstance::OUTPUTS[0].ty, PortType::Array(f32_layout));
    }

    #[test]
    fn fbm_per_instance_defaults_match_legacy_noise_common() {
        let by_name: std::collections::HashMap<&str, &ParamDef> = FbmPerInstance::PARAMS
            .iter()
            .map(|p| (p.name.as_ref(), p))
            .collect();
        let oct = by_name.get("octaves").expect("octaves param missing");
        let lac = by_name.get("lacunarity").expect("lacunarity param missing");
        let gain = by_name.get("gain").expect("gain param missing");
        match (&oct.default, &lac.default, &gain.default) {
            (ParamValue::Float(o), ParamValue::Float(l), ParamValue::Float(g)) => {
                assert_eq!(*o, 5.0, "5 octaves matches noise_common.wgsl::fbm");
                assert_eq!(*l, 1.5, "lacunarity 1.5 matches legacy fbm");
                assert_eq!(*g, 0.8, "gain 0.8 matches legacy fbm");
            }
            _ => panic!("default values must be Float"),
        }
    }

    #[test]
    fn fbm_per_instance_has_port_shadow_inputs_for_realtime_params() {
        use crate::node_graph::ports::{PortType, ScalarType};
        for name in ["scale", "z", "offset_x", "offset_y"] {
            let port = FbmPerInstance::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} is port-shadow, must be optional");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        // Structural params (octaves/lacunarity/gain) intentionally
        // have NO port-shadow inputs — they are tuning knobs, not
        // real-time control surfaces.
        for name in ["octaves", "lacunarity", "gain"] {
            assert!(
                FbmPerInstance::INPUTS.iter().all(|p| p.name != name),
                "{name} must not be port-shadowed (structural-only)",
            );
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FbmPerInstance::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fractal_noise_per_copy");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain parity oracle (freeze §12) — fbm_per_instance had no GPU
    //! test. The generated kernel (noise_common prepended for simplex3d, bare
    //! Array(f32) output) must reproduce the hand kernel value-for-value. Same
    //! simplex3d + fbm loop on-GPU → bit-identical. Validates the single-channel
    //! `array<f32>` element path (no struct).
    use super::*;

    fn dispatch_fbm(wgsl: &str, uvs: &[[f32; 2]], uniform: &[u8], count: u32) -> Vec<f32> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "fbm-oracle");
        let uv_buf = device.create_buffer_shared(std::mem::size_of_val(uvs) as u64);
        let out_buf = device.create_buffer_shared(count as u64 * 4);
        unsafe {
            uv_buf.write(0, bytemuck::cast_slice(uvs));
        }
        let mut enc = device.create_encoder("fbm-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &uv_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &out_buf, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "fbm-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const f32, count as usize) };
        slice.to_vec()
    }

    #[test]
    fn generated_fbm_matches_hand_kernel() {
        let uvs: [[f32; 2]; 5] = [[0.1, 0.2], [0.5, 0.6], [0.9, 0.3], [0.0, 1.0], [0.33, 0.77]];
        let n = uvs.len() as u32;
        let (scale, z, ox, oy) = (4.0f32, 0.5f32, 1.0f32, -2.0f32);
        let (octaves, lac, gain) = (5u32, 1.5f32, 0.8f32);

        // Hand layout: count(u32), scale, z, offset_x, offset_y, octaves(u32), lacunarity, gain.
        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        for v in [scale, z, ox, oy] {
            hand.extend_from_slice(&v.to_le_bytes());
        }
        hand.extend_from_slice(&octaves.to_le_bytes());
        hand.extend_from_slice(&lac.to_le_bytes());
        hand.extend_from_slice(&gain.to_le_bytes());

        // Generated layout: scale, z, offset_x, offset_y, octaves(i32), lacunarity, gain, dispatch_count(u32).
        let mut gen_bytes = Vec::new();
        for v in [scale, z, ox, oy] {
            gen_bytes.extend_from_slice(&v.to_le_bytes());
        }
        gen_bytes.extend_from_slice(&(octaves as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&lac.to_le_bytes());
        gen_bytes.extend_from_slice(&gain.to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());

        let hand_wgsl =
            format!("{}\n{}", NOISE_COMMON, include_str!("shaders/fbm_per_instance.wgsl"));
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<FbmPerInstance>()
            .expect("fbm_per_instance buffer codegen");
        assert!(gen_wgsl.contains("read_write> buf_out: array<f32>"), "bare f32 output array");

        let from_hand = dispatch_fbm(&hand_wgsl, &uvs, &hand, n);
        let from_gen = dispatch_fbm(&gen_wgsl, &uvs, &gen_bytes, n);

        for i in 0..n as usize {
            assert!(
                (from_hand[i] - from_gen[i]).abs() < 1e-6,
                "slot {i}: hand={} gen={}",
                from_hand[i],
                from_gen[i]
            );
        }
    }
}
