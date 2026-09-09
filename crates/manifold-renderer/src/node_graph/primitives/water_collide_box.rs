//! `node.water_collide_box` — S5 solver stage: particle collision against
//! the translating cube and the static basin.
//!
//! Closes the grid-resolution leakage after G2P advection (design step 6):
//! particles inside the cube's fixed-half-extents AABB are projected out
//! along the minimum-penetration axis to the face (post-projection
//! penetration 0, inside the 0.1*h acceptance), and the into-surface normal
//! component of the RELATIVE velocity (v − collider_velocity) is removed —
//! free-slip tangential, design section 6. The static basin faces mirror
//! `node.mpm_grid_velocity`'s boundary path exactly: same geometry, same
//! free-slip rule, so the particle-level projection stays consistent with
//! the grid-level one.
//!
//! The collider translation arrives on a `Transform` wire (the accepted
//! collider from `node.water_collider_motion`, which the displayed cube
//! also consumes); rotation/scale are display-only — the collision
//! geometry is the axis-aligned box with the configured half extents.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 5 and 6;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::{DEFAULT_STEP_DT, WaterParticle};

/// Default cube half extents (design section 8: a 0.5 m cube).
pub const CUBE_HALF: [f32; 3] = [0.25, 0.25, 0.25];

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`step_dt`, the three cube half extents, the six basin bounds), then the
/// derived `collider` / `collider_velocity` vec3s as three consecutive f32
/// fields each (buffer-path vec3 packing, packed per dispatch by `run()`
/// from the Transform / ScalarVec3 wires), then the codegen-injected
/// `dispatch_count`. 10 + 6 + 1 = 17 words -> 3 pads = 80 bytes.
/// Field order must match the generated WGSL `Params` exactly.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CollideBoxUniforms {
    pub step_dt: f32,
    pub cube_half_x: f32,
    pub cube_half_y: f32,
    pub cube_half_z: f32,
    pub basin_min_x: f32,
    pub basin_min_y: f32,
    pub basin_min_z: f32,
    pub basin_max_x: f32,
    pub basin_max_y: f32,
    pub basin_max_z: f32,
    pub collider_x: f32,
    pub collider_y: f32,
    pub collider_z: f32,
    pub collider_velocity_x: f32,
    pub collider_velocity_y: f32,
    pub collider_velocity_z: f32,
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}

crate::primitive! {
    name: WaterCollideBox,
    type_id: "node.water_collide_box",
    purpose: "Project Live Water particles against the translating cube collider and the static basin (design step 6). Particles inside the cube's fixed-half-extents AABB are pushed out along the minimum-penetration axis to the face (post-projection penetration 0, within the 0.1*h acceptance), and the into-surface normal component of the relative velocity (v - collider_velocity) is removed — free-slip tangential, design section 6. The basin faces use the exact rule and geometry of node.mpm_grid_velocity's boundary path so particle-level projection closes the grid-resolution leakage consistently. The collider translation arrives on a Transform wire — the accepted collider emitted by node.water_collider_motion, which the displayed cube consumes — so collision and display can never diverge. Rotation/scale are display-only. Inactive slots pass through; density and affine state are untouched.",
    inputs: {
        in: Array(WaterParticle) required,
        collider: Transform required,
        collider_velocity: ScalarVec3 optional,
        step_dt: ScalarF32 optional,
    },
    outputs: {
        out: Array(WaterParticle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("step_dt"),
            label: "Substep dt",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_STEP_DT),
            range: Some((1.0e-5, 1.0e-2)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cube_half_x"),
            label: "Cube Half X",
            ty: ParamType::Float,
            default: ParamValue::Float(CUBE_HALF[0]),
            range: Some((0.01, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cube_half_y"),
            label: "Cube Half Y",
            ty: ParamType::Float,
            default: ParamValue::Float(CUBE_HALF[1]),
            range: Some((0.01, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cube_half_z"),
            label: "Cube Half Z",
            ty: ParamType::Float,
            default: ParamValue::Float(CUBE_HALF[2]),
            range: Some((0.01, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_min_x"),
            label: "Basin Min X",
            ty: ParamType::Float,
            default: ParamValue::Float(crate::node_graph::primitives::BASIN_MIN[0]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_min_y"),
            label: "Basin Min Y",
            ty: ParamType::Float,
            default: ParamValue::Float(crate::node_graph::primitives::BASIN_MIN[1]),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_min_z"),
            label: "Basin Min Z",
            ty: ParamType::Float,
            default: ParamValue::Float(crate::node_graph::primitives::BASIN_MIN[2]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_max_x"),
            label: "Basin Max X",
            ty: ParamType::Float,
            default: ParamValue::Float(crate::node_graph::primitives::BASIN_MAX[0]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_max_y"),
            label: "Basin Max Y",
            ty: ParamType::Float,
            default: ParamValue::Float(crate::node_graph::primitives::BASIN_MAX[1]),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_max_z"),
            label: "Basin Max Z",
            ty: ParamType::Float,
            default: ParamValue::Float(crate::node_graph::primitives::BASIN_MAX[2]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Last stage of the repeated water region body, after node.mpm_gather_advect: `mpm_gather_advect -> water_collide_box -> water_validate -> water_commit`. Wire `collider` from node.water_collider_motion's `transform` output (the accepted collider — the same value the displayed cube consumes) and `collider_velocity` from its `velocity` output. Basin defaults match node.mpm_grid_velocity's proof basin. The output aliases the input wire (pure per-element projection).",
    examples: [],
    picker: { label: "Water Collide Box", category: Atom },
    summary: "Pushes water particles out of the moving cube and the basin walls, matching the grid's boundary rule at particle resolution.",
    category: Particles3D,
    role: Filter,
    aliases: ["water collide", "cube collision", "water collision", "collide box"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/water_collide_box_body.wgsl"),
    input_access: [Coincident],
    derived_uniforms: ["collider:vec3", "collider_velocity:vec3"],
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
}

impl Primitive for WaterCollideBox {
    /// The projection is a pure per-element read-modify-write — the output
    /// aliases the input wire.
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "in")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let step_dt = ctx.scalar_or_param("step_dt", DEFAULT_STEP_DT);
        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(collider) = ctx.inputs.transform("collider") else {
            return;
        };
        let collider_velocity = match ctx.inputs.scalar("collider_velocity") {
            Some(ParamValue::Vec3(v)) => v,
            _ => [0.0; 3],
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let particle_size = std::mem::size_of::<WaterParticle>() as u64;
        let capacity = (in_buf.size.min(out_buf.size) / particle_size) as u32;
        if capacity == 0 {
            return;
        }

        let read = |name: &str, default: f32| match ctx.params.get(name) {
            Some(ParamValue::Float(f)) => *f,
            _ => default,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel
            // is generated from the `wgsl_body` so the atom participates in
            // freeze fusion.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.water_collide_box standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.water_collide_box",
            )
        });

        let uniforms = CollideBoxUniforms {
            step_dt,
            cube_half_x: read("cube_half_x", CUBE_HALF[0]),
            cube_half_y: read("cube_half_y", CUBE_HALF[1]),
            cube_half_z: read("cube_half_z", CUBE_HALF[2]),
            basin_min_x: read("basin_min_x", crate::node_graph::primitives::BASIN_MIN[0]),
            basin_min_y: read("basin_min_y", crate::node_graph::primitives::BASIN_MIN[1]),
            basin_min_z: read("basin_min_z", crate::node_graph::primitives::BASIN_MIN[2]),
            basin_max_x: read("basin_max_x", crate::node_graph::primitives::BASIN_MAX[0]),
            basin_max_y: read("basin_max_y", crate::node_graph::primitives::BASIN_MAX[1]),
            basin_max_z: read("basin_max_z", crate::node_graph::primitives::BASIN_MAX[2]),
            collider_x: collider.pos[0],
            collider_y: collider.pos[1],
            collider_z: collider.pos[2],
            collider_velocity_x: collider_velocity[0],
            collider_velocity_y: collider_velocity[1],
            collider_velocity_z: collider_velocity[2],
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // uniform(0), in(1), out(2).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.water_collide_box",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn collide_box_declares_particle_in_transform_collider() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let particle_layout = ArrayType::of_known::<WaterParticle>();
        assert_eq!(WaterCollideBox::TYPE_ID, "node.water_collide_box");
        assert_eq!(WaterCollideBox::INPUTS[0].name, "in");
        assert_eq!(WaterCollideBox::INPUTS[0].ty, PortType::Array(particle_layout));
        assert_eq!(WaterCollideBox::INPUTS[1].name, "collider");
        assert_eq!(WaterCollideBox::INPUTS[1].ty, PortType::Transform);
        assert_eq!(WaterCollideBox::INPUTS[2].name, "collider_velocity");
        assert_eq!(
            WaterCollideBox::INPUTS[2].ty,
            PortType::Scalar(ScalarType::Vec3)
        );
        assert_eq!(WaterCollideBox::OUTPUTS.len(), 1);
        assert_eq!(WaterCollideBox::OUTPUTS[0].name, "out");
        assert_eq!(WaterCollideBox::OUTPUTS[0].ty, PortType::Array(particle_layout));
    }

    #[test]
    fn collide_box_registers_and_aliases() {
        let prim = WaterCollideBox::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.water_collide_box");
        assert_eq!(node.aliased_array_io(), &[("in", "out")]);
    }

    #[test]
    fn collide_box_codegen_binds_coincident_input_and_derived() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<WaterCollideBox>()
            .expect("node.water_collide_box standalone codegen");
        assert!(wgsl.contains("struct Element"));
        assert!(wgsl.contains("collider_velocity"));
        assert!(wgsl.contains("vec3<f32>"));
    }
}
