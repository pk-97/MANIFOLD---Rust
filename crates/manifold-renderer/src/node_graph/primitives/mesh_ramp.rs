//! `node.mesh_ramp` — per-vertex growth-mask weights from a spatial axis
//! sweep. Source of weights, NOT a deformer (MESH_DEFORM_AND_CURVE_GEOMETRY_
//! DESIGN.md D2): reads an `Array<MeshVertex>`'s positions and emits an
//! `Array<f32>` weights buffer (one weight per vertex) for any deformer's
//! optional `weights` input.
//!
//! Per vertex: `m = measure(pos - origin)` along the chosen `axis` (signed
//! X/Y/Z coordinate, `Radial XZ` cylindrical radius, or full `Distance`
//! magnitude), normalized to `t = clamp((m - bound_min) / (bound_max -
//! bound_min), 0, 1)`, then `w = 1 - smoothstep(phase, phase + feather, t)`,
//! optionally inverted. Sweeping `phase` from 0 toward 1 (e.g. driven by a
//! beat ramp) moves the falling edge of the mask across the mesh — the
//! growth mechanism every deformer's `weights` port composes with.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const RAMP_AXES: &[&str] = &["X", "Y", "Z", "Radial XZ", "Distance"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`axis`
/// Enum→u32, `origin_x/y/z` f32, `phase`/`feather`/`bound_min`/`bound_max` f32,
/// `invert` Bool→u32), then the codegen-injected `dispatch_count` element count,
/// padded to a 16-byte multiple. 10 words + 2 pad = 48 bytes. Matches the
/// `standalone_for_spec::<MeshRamp>()` Params struct field-for-field.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshRampUniforms {
    axis: u32,
    origin_x: f32,
    origin_y: f32,
    origin_z: f32,
    phase: f32,
    feather: f32,
    bound_min: f32,
    bound_max: f32,
    invert: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: MeshRamp,
    type_id: "node.mesh_ramp",
    purpose: "Compute a per-vertex growth-mask weight from a spatial axis sweep over an Array<MeshVertex>. Source of weights, not a deformer: m = measure(pos - origin) along `axis` (X/Y/Z signed coordinate, Radial XZ cylindrical radius, or full Distance magnitude), t = clamp((m - bound_min) / (bound_max - bound_min), 0, 1), w = 1 - smoothstep(phase, phase + feather, t), optionally inverted. Wire the `weights` output into any deformer's optional weights port and sweep `phase` (e.g. from a beat ramp) so the effect grows progressively across the mesh instead of applying uniformly everywhere.",
    inputs: {
        in: Array(MeshVertex) required,
        origin_x: ScalarF32 optional,
        origin_y: ScalarF32 optional,
        origin_z: ScalarF32 optional,
        phase: ScalarF32 optional,
        feather: ScalarF32 optional,
        bound_min: ScalarF32 optional,
        bound_max: ScalarF32 optional,
    },
    outputs: {
        weights: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1), // Y
            range: Some((0.0, (RAMP_AXES.len() - 1) as f32)),
            enum_values: RAMP_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("origin_x"),
            label: "Origin X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("origin_y"),
            label: "Origin Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("origin_z"),
            label: "Origin Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("phase"),
            label: "Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("feather"),
            label: "Feather",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("bound_min"),
            label: "Bound Min",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("bound_max"),
            label: "Bound Max",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("invert"),
            label: "Invert",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Reach for this whenever a deformer needs to grow/reveal progressively rather than apply uniformly everywhere — wire `weights` into node.push_along_normals (or any future deform atom's weights port) and sweep `phase` from a beat ramp so the effect walks up the mesh over bars. `axis=Radial XZ` grows outward from a vertical origin line (stems, columns); `axis=Distance` grows outward from a point in all directions (spherical blooms). Not a deformer itself — pair with a consumer that reads `weights`.",
    examples: ["Breathe"],
    picker: { label: "Mesh Ramp", category: Atom },
    summary: "Turns a mesh's own positions into a growth mask — a value from 0 to 1 per vertex that sweeps across the mesh along an axis. Feeds any deformer's weight input to make effects grow progressively.",
    category: Geometry3D,
    role: Source,
    aliases: ["mesh ramp", "growth mask", "gradient weights", "reveal mask"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mesh_ramp_body.wgsl"),
}

impl Primitive for MeshRamp {
    /// Output `weights` is one f32 per input vertex — inherits capacity
    /// from `in`.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "weights" {
            return None;
        }
        input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(v)) => (*v).min((RAMP_AXES.len() - 1) as u32),
            _ => 1,
        };
        let origin_x = ctx.scalar_or_param("origin_x", 0.0);
        let origin_y = ctx.scalar_or_param("origin_y", 0.0);
        let origin_z = ctx.scalar_or_param("origin_z", 0.0);
        let phase = ctx.scalar_or_param("phase", 0.0);
        let feather = ctx.scalar_or_param("feather", 0.1);
        let bound_min = ctx.scalar_or_param("bound_min", 0.0);
        let bound_max = ctx.scalar_or_param("bound_max", 1.0);
        let invert = matches!(ctx.params.get("invert"), Some(ParamValue::Bool(true)));

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("weights") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let in_count = (src.size / vertex_size) as u32;
        let out_count = (dst.size / 4) as u32;
        let count = in_count.min(out_count);
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (design D#10): the runtime kernel is generated from
            // `wgsl_body` so this atom fuses in the graph compiler. mesh_ramp.wgsl
            // is retained only as the gpu_tests parity oracle. Bindings match:
            // uniform(0), buf_in(1, MeshVertex read), buf_weights(2, f32 write).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.mesh_ramp standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.mesh_ramp",
            )
        });

        let uniforms = MeshRampUniforms {
            axis,
            origin_x,
            origin_y,
            origin_z,
            phase,
            feather,
            bound_min,
            bound_max,
            invert: u32::from(invert),
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
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.mesh_ramp",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn mesh_ramp_declares_mesh_in_and_weights_out() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(MeshRamp::TYPE_ID, "node.mesh_ramp");

        let in_port = MeshRamp::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        for name in ["origin_x", "origin_y", "origin_z", "phase", "feather", "bound_min", "bound_max"] {
            let port = MeshRamp::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(MeshRamp::OUTPUTS.len(), 1);
        assert_eq!(MeshRamp::OUTPUTS[0].name, "weights");
        assert_eq!(MeshRamp::OUTPUTS[0].ty, PortType::Array(f32_layout));
    }

    #[test]
    fn mesh_ramp_axis_and_invert_are_not_port_shadowed() {
        for name in ["axis", "invert"] {
            let has_port = MeshRamp::INPUTS.iter().any(|p| p.name == name);
            assert!(!has_port, "{name} is an enum/bool — must not be port-shadowed");
        }
    }

    #[test]
    fn mesh_ramp_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = MeshRamp::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "weights", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = MeshRamp::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mesh_ramp");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. Behavioral tests dispatch the GENERATED
    //! standalone kernel (the shipping runtime artifact, built by
    //! `standalone_for_spec::<MeshRamp>()` from mesh_ramp_body.wgsl) and compare
    //! against a hand-written Rust reference of the committed formula.
    //! (The generated-vs-hand-kernel parity test against `mesh_ramp.wgsl`
    //! was deleted 2026-07-20, W1-B, migration scaffolding retired.)
    use super::*;

    fn mk_vertex(y: f32) -> MeshVertex {
        MeshVertex {
            position: [0.0, y, 0.0],
            _pad0: 0.0,
            normal: [0.0, 1.0, 0.0],
            _pad1: 0.0,
            uv: [0.0, 0.0],
            _pad2: [0.0, 0.0],
        }
    }

    /// The generated standalone kernel (the shipping runtime path).
    fn generated_wgsl() -> String {
        crate::node_graph::freeze::codegen::standalone_for_spec::<MeshRamp>()
            .expect("mesh_ramp buffer codegen")
    }

    /// Hand-reference: identical formula to the WGSL kernel, f32 math.
    fn expected_weight(m: f32, bound_min: f32, bound_max: f32, phase: f32, feather: f32, invert: bool) -> f32 {
        let denom = (bound_max - bound_min).max(1e-6);
        let t = ((m - bound_min) / denom).clamp(0.0, 1.0);
        let edge0 = phase;
        let edge1 = phase + feather.max(1e-6);
        let x = ((t - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
        let smoothstep = x * x * (3.0 - 2.0 * x);
        let w = 1.0 - smoothstep;
        if invert { 1.0 - w } else { w }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_ramp(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        vertices: &[MeshVertex],
        axis: u32,
        origin: [f32; 3],
        phase: f32,
        feather: f32,
        bound_min: f32,
        bound_max: f32,
        invert: bool,
    ) -> Vec<f32> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "mesh-ramp-test",
        );
        let src = device.create_buffer_shared(std::mem::size_of_val(vertices) as u64);
        unsafe {
            src.write(0, bytemuck::cast_slice(vertices));
        }
        let dst = device.create_buffer_shared((vertices.len() * 4) as u64);

        let uniforms = MeshRampUniforms {
            axis,
            origin_x: origin[0],
            origin_y: origin[1],
            origin_z: origin[2],
            phase,
            feather,
            bound_min,
            bound_max,
            invert: u32::from(invert),
            dispatch_count: vertices.len() as u32,
            _pad0: 0,
            _pad1: 0,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &src, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &dst, offset: 0 },
        ];
        let mut enc = device.create_encoder("mesh-ramp-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(vertices.len() as u32).div_ceil(256), 1, 1],
            "mesh-ramp-test",
        );
        enc.commit_and_wait_completed();

        let ptr = dst.mapped_ptr().expect("shared weights buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const f32, vertices.len()) }.to_vec()
    }


    #[test]
    fn axis_y_ramp_matches_hand_formula_and_is_monotonic() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let ys = [0.0f32, 1.0, 2.0, 3.0];
        let vertices: Vec<MeshVertex> = ys.iter().map(|&y| mk_vertex(y)).collect();

        let (phase, feather, bound_min, bound_max) = (0.0f32, 0.5f32, 0.0f32, 3.0f32);
        let got = dispatch_ramp(
            &device, &gen_wgsl, &vertices, 1, [0.0, 0.0, 0.0], phase, feather, bound_min, bound_max, false,
        );

        let expected: Vec<f32> = ys
            .iter()
            .map(|&y| expected_weight(y, bound_min, bound_max, phase, feather, false))
            .collect();

        for i in 0..ys.len() {
            assert!(
                (got[i] - expected[i]).abs() < 1e-4,
                "vertex {i}: got={} expected={}",
                got[i],
                expected[i]
            );
        }

        // Hand-computed anchors (§4 invariant: "monotonic ramp along its
        // axis... hand-computed expected at a couple of vertices").
        assert!((got[0] - 1.0).abs() < 1e-5, "v0 should be fully weighted, got {}", got[0]);
        assert!(
            (got[1] - 0.259_259_3).abs() < 1e-4,
            "v1 hand-computed 0.2592593, got {}",
            got[1]
        );
        assert!(got[2].abs() < 1e-5, "v2 should be fully decayed, got {}", got[2]);
        assert!(got[3].abs() < 1e-5, "v3 should be fully decayed, got {}", got[3]);

        // Monotonic non-increasing along the sweep axis.
        for i in 1..got.len() {
            assert!(
                got[i] <= got[i - 1] + 1e-6,
                "ramp not monotonic at {i}: {} > {}",
                got[i],
                got[i - 1]
            );
        }
    }

    #[test]
    fn invert_flips_the_mask() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let vertices: Vec<MeshVertex> = [0.0f32, 3.0].iter().map(|&y| mk_vertex(y)).collect();
        let (phase, feather, bound_min, bound_max) = (0.0f32, 0.5f32, 0.0f32, 3.0f32);

        let normal = dispatch_ramp(
            &device, &gen_wgsl, &vertices, 1, [0.0, 0.0, 0.0], phase, feather, bound_min, bound_max, false,
        );
        let inverted = dispatch_ramp(
            &device, &gen_wgsl, &vertices, 1, [0.0, 0.0, 0.0], phase, feather, bound_min, bound_max, true,
        );

        for i in 0..vertices.len() {
            assert!(
                (normal[i] + inverted[i] - 1.0).abs() < 1e-4,
                "invert should be 1 - w at {i}: normal={} inverted={}",
                normal[i],
                inverted[i]
            );
        }
    }
}
