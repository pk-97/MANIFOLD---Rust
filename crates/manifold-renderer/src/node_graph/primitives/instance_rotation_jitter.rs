//! `node.rotation_jitter` — add hash-driven per-instance
//! rotation jitter to each InstanceTransform's `rot_pad.xyz`.
//! Positions and per-instance scale pass through unchanged.
//!
//! Per idx:
//! ```text
//! rx = (hash_u32(idx * 3 + 0) - 0.5) * amplitude
//! ry = (hash_u32(idx * 3 + 1) - 0.5) * amplitude
//! rz = (hash_u32(idx * 3 + 2) - 0.5) * amplitude
//! rot_pad.xyz += (rx, ry, rz)
//! ```
//!
//! Bit-exact with the legacy DigitalPlants per-instance rotation
//! hash (amplitude = 0.2, range [-0.1, 0.1]) when the input rot is
//! zero — same `hash_u32` source from `noise_common.wgsl`, same
//! `idx*3 + {0,1,2}` keys. ADD semantics rather than OVERWRITE so
//! pre-existing rotation from upstream survives the jitter (the
//! legacy compute pass had no prior rotation to preserve — the
//! decomposition gives every primitive the more general contract).

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the `amplitude` param (f32) then the
/// codegen-injected `dispatch_count` (= element count, the guard), padded to 16
/// bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    amplitude: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

const NOISE_COMMON: &str = include_str!("../../generators/shaders/noise_common.wgsl");

crate::primitive! {
    name: InstanceRotationJitter,
    type_id: "node.rotation_jitter",
    purpose: "Add hash-driven per-instance Euler-rotation jitter to each InstanceTransform's rot_pad.xyz; positions and scale pass through. For each idx: rx/ry/rz = (hash_u32(idx*3+{0,1,2}) - 0.5) · amplitude. ADD semantics — pre-existing rotation from upstream is preserved and perturbed. Generic across any instanced field that wants visual density via non-uniform per-cube orientation. Reproduces the legacy DigitalPlants per-instance rotation hash bit-exactly when amplitude = 0.2.",
    inputs: {
        instances: Array(InstanceTransform) required,
        amplitude: ScalarF32 optional,
    },
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amplitude"),
            label: "Amplitude",
            ty: ParamType::Float,
            default: ParamValue::Float(0.2),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows the `instances` input. The amplitude factor matches the legacy DigitalPlants convention: 0.2 yields a [-0.1, 0.1] radians range across each axis. The hash uses idx*3 + {0,1,2} keys so each axis is decorrelated; the sequence is deterministic per index (re-rendering the same chain gives identical jitter). ADD semantics preserve any upstream rotation — emitting a uniform pose from a wrap and then jittering it is the typical composition.",
    examples: [],
    picker: { label: "Rotation Jitter", category: Atom },
    summary: "Adds a random twist to each copy's rotation, so a field of copies face slightly different ways instead of lining up.",
    category: Particles2D,
    role: Filter,
    aliases: ["rotation jitter", "instance rotation jitter", "random rotation", "twist"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/instance_rotation_jitter_body.wgsl"),
    wgsl_includes: [NOISE_COMMON],
}

impl Primitive for InstanceRotationJitter {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "instances" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "instances")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amplitude = ctx.scalar_or_param("amplitude", 0.2);

        let Some(in_inst_buf) = ctx.inputs.array("instances") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("instances") else {
            return;
        };

        let inst_size = std::mem::size_of::<InstanceTransform>() as u64;
        let in_cap = (in_inst_buf.size / inst_size) as u32;
        let out_cap = (out_buf.size / inst_size) as u32;
        let count = in_cap.min(out_cap);
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path; noise_common prepended via wgsl_includes for
            // hash_u32). instance_rotation_jitter.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.rotation_jitter standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.rotation_jitter",
            )
        });

        let uniforms = Uniforms {
            amplitude,
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
                    buffer: in_inst_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.rotation_jitter",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn instance_rotation_jitter_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let inst_layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(InstanceRotationJitter::TYPE_ID, "node.rotation_jitter");

        let inst_in = InstanceRotationJitter::INPUTS
            .iter()
            .find(|p| p.name == "instances")
            .unwrap();
        assert!(inst_in.required);
        assert_eq!(inst_in.ty, PortType::Array(inst_layout));

        let amp = InstanceRotationJitter::INPUTS
            .iter()
            .find(|p| p.name == "amplitude")
            .unwrap();
        assert!(!amp.required);
        assert_eq!(amp.ty, PortType::Scalar(ScalarType::F32));

        assert_eq!(InstanceRotationJitter::OUTPUTS.len(), 1);
        assert_eq!(InstanceRotationJitter::OUTPUTS[0].name, "instances");
        assert_eq!(
            InstanceRotationJitter::OUTPUTS[0].ty,
            PortType::Array(inst_layout),
        );
    }

    #[test]
    fn instance_rotation_jitter_default_amplitude_matches_legacy() {
        let amp = InstanceRotationJitter::PARAMS
            .iter()
            .find(|p| p.name == "amplitude")
            .unwrap();
        match &amp.default {
            ParamValue::Float(f) => assert_eq!(
                *f, 0.2,
                "0.2 reproduces legacy DigitalPlants per-instance rot",
            ),
            _ => panic!("amplitude default must be Float"),
        }
    }

    #[test]
    fn instance_rotation_jitter_output_follows_instances_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = InstanceRotationJitter::new();
        let params = ParamValues::default();
        let inputs = [("instances", 160_000_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "instances", &params, &inputs),
            Some(160_000),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = InstanceRotationJitter::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.rotation_jitter");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain parity oracle (freeze §12) — instance_rotation_jitter had no
    //! GPU test. The generated kernel (noise_common prepended for hash_u32,
    //! same-named in/out `instances` disambiguated to buf_out_instances) must
    //! reproduce the hand kernel instance-for-instance: jittered rot.xyz, position
    //! + scale + rot.w passed through. Integer hash on-GPU both ways → bit-exact.
    use super::*;

    fn dispatch_rotjitter(
        wgsl: &str,
        instances: &[InstanceTransform],
        uniform: &[u8],
        count: u32,
    ) -> Vec<InstanceTransform> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "rotjitter-oracle");
        let in_buf = device.create_buffer_shared(std::mem::size_of_val(instances) as u64);
        let out_buf = device.create_buffer_shared(std::mem::size_of_val(instances) as u64);
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(instances));
        }
        let mut enc = device.create_encoder("rotjitter-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &in_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &out_buf, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "rotjitter-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice =
            unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, instances.len()) };
        slice.to_vec()
    }

    #[test]
    fn generated_rotjitter_matches_hand_kernel() {
        let instances = [
            InstanceTransform { pos_scale: [0.2, 0.3, 0.1, 1.5], rot_pad: [0.1, 0.2, 0.3, 0.0] },
            InstanceTransform { pos_scale: [-0.4, 0.6, -0.2, 2.0], rot_pad: [0.4, -0.1, 0.0, 0.0] },
            InstanceTransform { pos_scale: [0.7, -0.5, 0.4, 0.8], rot_pad: [-0.2, 0.3, 0.1, 0.0] },
            InstanceTransform { pos_scale: [0.0, 0.0, 0.0, 1.0], rot_pad: [0.0, 0.0, 0.0, 0.0] },
        ];
        let n = instances.len() as u32;
        let amplitude = 0.2f32;

        // Hand layout: count(u32), amplitude(f32), pad, pad.
        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&amplitude.to_le_bytes());
        hand.extend_from_slice(&[0u8; 8]);

        // Generated layout: amplitude(f32), dispatch_count(u32), pad, pad.
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&amplitude.to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());
        gen_bytes.extend_from_slice(&[0u8; 8]);

        let hand_wgsl =
            format!("{}\n{}", NOISE_COMMON, include_str!("shaders/instance_rotation_jitter.wgsl"));
        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<InstanceRotationJitter>()
                .expect("instance_rotation_jitter buffer codegen");

        let from_hand = dispatch_rotjitter(&hand_wgsl, &instances, &hand, n);
        let from_gen = dispatch_rotjitter(&gen_wgsl, &instances, &gen_bytes, n);

        for i in 0..instances.len() {
            for c in 0..4 {
                assert!(
                    (from_hand[i].rot_pad[c] - from_gen[i].rot_pad[c]).abs() < 1e-6,
                    "instance {i} rot_pad[{c}]: hand={} gen={}",
                    from_hand[i].rot_pad[c],
                    from_gen[i].rot_pad[c]
                );
                assert!(
                    (from_hand[i].pos_scale[c] - from_gen[i].pos_scale[c]).abs() < 1e-6,
                    "instance {i} pos_scale[{c}] passthrough"
                );
            }
        }
    }
}
