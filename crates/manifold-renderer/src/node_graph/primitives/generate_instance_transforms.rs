//! `node.arrange_copies` — emit an
//! `Array<InstanceTransform>` filled with a procedural layout.
//!
//! Phase B of `BUFFER_PORT_PLAN`. The instance-side companion
//! to `node.grid_mesh` — produces N transforms in one
//! of grid / ring / spiral / random patterns, paired with
//! `node.render_copies` to draw N copies of a base
//! mesh. Unlocks NestedCubes, DigitalPlants, and any future
//! "many small objects in a pattern" generator.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const INSTANCE_LAYOUTS: &[&str] = &["Grid", "Ring", "Spiral", "Random"];

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`max_capacity` Int → i32 [allocation-only, the shader ignores it but it
/// occupies a uniform word], `active_count` Int → i32 [the inactive threshold],
/// `layout` Enum → u32, `seed` Int → i32, then the f32 extents / base_scale /
/// rotations), then the codegen-injected `dispatch_count` (= output capacity,
/// the guard). 12 words = 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceUniforms {
    max_capacity: i32,
    active_count: i32,
    layout: u32,
    seed: i32,
    extent_x: f32,
    extent_y: f32,
    extent_z: f32,
    base_scale: f32,
    rot_x: f32,
    rot_y: f32,
    rot_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: GenerateInstanceTransforms,
    type_id: "node.arrange_copies",
    purpose: "Emit an Array<InstanceTransform> filled with one of four procedural layouts (slots beyond active_count zero out): Grid — side = ceil(active_count^(1/3)), 3D index (cx,cy,cz) = (i mod side, (i/side) mod side, i/side^2), position = ((cx/(side-1) - 0.5)*extent_x, (cy/(side-1) - 0.5)*extent_y, (cz/(side-1) - 0.5)*extent_z). Ring — t = i/active_count, theta = t*2π, position = (cos(theta)*extent_x/2, 0, sin(theta)*extent_z/2). Spiral — t = i/active_count, theta = t*2π*4 (4 turns), r = t, position = (cos(theta)*r*extent_x/2, (t-0.5)*extent_y, sin(theta)*r*extent_z/2). Random — wang-hash of (index, seed) per axis, position = (hash-0.5)*extent per axis, uniform within the extent box. Scale is uniform `base_scale`; rotation is uniform (rot_x, rot_y, rot_z) applied identically to every instance. Pair with node.render_copies to draw N copies of a base mesh. The unlock for NestedCubes / DigitalPlants-shaped graphs.",
    inputs: {},
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(65_536.0),
            range: Some((1.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(64.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("layout"),
            label: "Layout",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: INSTANCE_LAYOUTS,
        },
        ParamDef {
            name: Cow::Borrowed("seed"),
            label: "Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("extent_x"),
            label: "Extent X",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("extent_y"),
            label: "Extent Y",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("extent_z"),
            label: "Extent Z",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("base_scale"),
            label: "Base Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rot_x"),
            label: "Rotation X",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rot_y"),
            label: "Rotation Y",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rot_z"),
            label: "Rotation Z",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "max_capacity is chain-build allocation ceiling — pre-allocates max_capacity × 32 bytes. active_count is a free slider. Rotation params apply to every instance uniformly — for per-instance varying rotation, write a downstream transform primitive that perturbs `rot_pad`.",
    examples: [],
    picker: { label: "Arrange Copies", category: Atom },
    summary: "Lays out a field of copies in a grid, ring, spiral, or random spread, giving each one a position to render at. Pair it with Render Copies.",
    category: Geometry3D,
    role: Source,
    aliases: ["arrange copies", "generate instance transforms", "instance layout", "scatter", "place"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/generate_instance_transforms_body.wgsl"),
}

impl Primitive for GenerateInstanceTransforms {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Allocation-only param — not used by the shader, but the generated
        // uniform lays out every PARAM, so pack it (the body ignores it).
        let max_capacity = match ctx.params.get("max_capacity") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => 65_536,
        };
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Float(n)) => n.round().max(0_f32) as u32,
            _ => 64,
        };
        let layout = match ctx.params.get("layout") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let seed = match ctx.params.get("seed") {
            Some(ParamValue::Float(n)) => n.round() as u32,
            _ => 0,
        };
        let extent_x = match ctx.params.get("extent_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4.0,
        };
        let extent_y = match ctx.params.get("extent_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4.0,
        };
        let extent_z = match ctx.params.get("extent_z") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4.0,
        };
        let base_scale = match ctx.params.get("base_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let rot_x = match ctx.params.get("rot_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let rot_y = match ctx.params.get("rot_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let rot_z = match ctx.params.get("rot_z") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let Some(out_buf) = ctx.outputs.array("instances") else {
            return;
        };
        let item_size = std::mem::size_of::<InstanceTransform>() as u64;
        let capacity = (out_buf.size / item_size) as u32;
        let active_count = active_count.min(capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer source
            // path; self-contained wang_hash). generate_instance_transforms.wgsl
            // is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.arrange_copies standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.arrange_copies",
            )
        });

        let uniforms = InstanceUniforms {
            max_capacity,
            active_count: active_count as i32,
            layout,
            seed: seed as i32,
            extent_x,
            extent_y,
            extent_z,
            base_scale,
            rot_x,
            rot_y,
            rot_z,
            dispatch_count: capacity,
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
            "node.arrange_copies",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn generate_instance_transforms_declares_zero_inputs_and_instance_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(
            GenerateInstanceTransforms::TYPE_ID,
            "node.arrange_copies"
        );
        assert!(GenerateInstanceTransforms::INPUTS.is_empty());
        assert_eq!(GenerateInstanceTransforms::OUTPUTS.len(), 1);
        assert_eq!(GenerateInstanceTransforms::OUTPUTS[0].name, "instances");
        assert_eq!(
            GenerateInstanceTransforms::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn layout_enum_has_four_options() {
        let layout_param = GenerateInstanceTransforms::PARAMS
            .iter()
            .find(|p| p.name == "layout")
            .expect("layout param");
        assert_eq!(layout_param.ty, ParamType::Enum);
        assert_eq!(layout_param.enum_values.len(), 4);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GenerateInstanceTransforms::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.arrange_copies");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain SOURCE parity oracle (freeze §12) — generate_instance_
    //! transforms had no GPU test. The generated kernel (self-contained
    //! wang_hash; active_count is a param; two-count inactive collapse) must
    //! reproduce the hand kernel transform-for-transform across ALL four layouts,
    //! including the inactive-slot zeroing. Deterministic math/hash on-GPU both
    //! ways → bit-identical.
    use super::*;

    fn dispatch_git(wgsl: &str, capacity: u32, uniform: &[u8]) -> Vec<InstanceTransform> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "git-oracle");
        let out_buf = device.create_buffer_shared(capacity as u64 * 32);
        let mut enc = device.create_encoder("git-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &out_buf, offset: 0 },
            ],
            [capacity.div_ceil(64), 1, 1],
            "git-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice =
            unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, capacity as usize) };
        slice.to_vec()
    }

    #[test]
    fn generated_instance_transforms_match_hand_kernel_all_layouts() {
        const CAPACITY: u32 = 16;
        let active = 10u32; // < capacity → exercises inactive collapse
        let seed = 42u32;
        let (ex, ey, ez) = (4.0f32, 3.0f32, 5.0f32);
        let base_scale = 1.25f32;
        let (rx, ry, rz) = (0.1f32, 0.2f32, 0.3f32);

        for layout in 0u32..4u32 {
            // Hand layout: active_count(u32), capacity(u32), layout(u32), seed(u32),
            //   extent_x/y/z, base_scale, rot_x/y/z, pad.
            let mut hand = Vec::new();
            hand.extend_from_slice(&active.to_le_bytes());
            hand.extend_from_slice(&CAPACITY.to_le_bytes());
            hand.extend_from_slice(&layout.to_le_bytes());
            hand.extend_from_slice(&seed.to_le_bytes());
            for v in [ex, ey, ez, base_scale, rx, ry, rz] {
                hand.extend_from_slice(&v.to_le_bytes());
            }
            hand.extend_from_slice(&[0u8; 4]);

            // Generated layout: max_capacity(i32), active_count(i32), layout(u32),
            //   seed(i32), extent_x/y/z, base_scale, rot_x/y/z, dispatch_count(u32).
            let mut gen_bytes = Vec::new();
            gen_bytes.extend_from_slice(&65_536i32.to_le_bytes());
            gen_bytes.extend_from_slice(&(active as i32).to_le_bytes());
            gen_bytes.extend_from_slice(&layout.to_le_bytes());
            gen_bytes.extend_from_slice(&(seed as i32).to_le_bytes());
            for v in [ex, ey, ez, base_scale, rx, ry, rz] {
                gen_bytes.extend_from_slice(&v.to_le_bytes());
            }
            gen_bytes.extend_from_slice(&CAPACITY.to_le_bytes());

            let hand_wgsl = include_str!("shaders/generate_instance_transforms.wgsl");
            let gen_wgsl =
                crate::node_graph::freeze::codegen::standalone_for_spec::<GenerateInstanceTransforms>()
                    .expect("generate_instance_transforms buffer codegen");

            let from_hand = dispatch_git(hand_wgsl, CAPACITY, &hand);
            let from_gen = dispatch_git(&gen_wgsl, CAPACITY, &gen_bytes);

            for i in 0..CAPACITY as usize {
                for c in 0..4 {
                    assert!(
                        (from_hand[i].pos_scale[c] - from_gen[i].pos_scale[c]).abs() < 1e-6,
                        "layout {layout} slot {i} pos_scale[{c}]: hand={} gen={}",
                        from_hand[i].pos_scale[c],
                        from_gen[i].pos_scale[c]
                    );
                    assert!(
                        (from_hand[i].rot_pad[c] - from_gen[i].rot_pad[c]).abs() < 1e-6,
                        "layout {layout} slot {i} rot_pad[{c}]"
                    );
                }
            }
        }
    }
}
