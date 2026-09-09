//! `node.mpm_grid_velocity` — S4 solver stage: resolve + gravity + boundary.
//!
//! Resolves the fixed-point accumulation wire into grid velocities
//! (`v_i = dequantise(momentum) / dequantise(mass) + gravity*step_dt` for
//! nonempty cells, zero for empty cells), then applies the static-basin
//! no-penetration boundary: at solid boundary nodes only the normal
//! component is removed (free-slip tangential, design step 5). The moving
//! collider boundary (design step 5, relative velocity) is S5 — this stage
//! is the static proof basin.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::{DEFAULT_STEP_DT, WaterGridCell};

/// Default static basin interior (design section 8: walls enclose the
/// default 2x0.5x2 m pool with splash room; the floor sits at the pool
/// bottom). Nodes at or outside a face are solid and get the no-penetration
/// projection. The ceiling at the domain top is never reached by any node
/// (node 63 sits at 3.9375 m < 4.0 m).
pub const BASIN_MIN: [f32; 3] = [-1.125, 0.25, -1.125];
/// Default static basin interior top — see [`BASIN_MIN`].
pub const BASIN_MAX: [f32; 3] = [1.125, 4.0, 1.125];

/// Default gravity, m/s^2 (Y up).
pub const GRAVITY: [f32; 3] = [0.0, -9.81, 0.0];

/// Cells of the default 64^3 proof domain.
pub const CELL_COUNT: u32 = 64 * 64 * 64;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`step_dt`, the six basin bounds, the allocation-only `cell_count`
/// Int -> i32), then the derived `gravity` vec3 as three consecutive f32
/// fields (buffer-path vec3 packing), then the codegen-injected
/// `dispatch_count`. 13 words + 3 pad = 64 bytes. Field order must match
/// the generated WGSL `Params` exactly — the macro PARAMS list is the
/// single source.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GridVelocityUniforms {
    pub step_dt: f32,
    pub basin_min_x: f32,
    pub basin_min_y: f32,
    pub basin_min_z: f32,
    pub basin_max_x: f32,
    pub basin_max_y: f32,
    pub basin_max_z: f32,
    pub cell_count: i32,
    pub gravity_x: f32,
    pub gravity_y: f32,
    pub gravity_z: f32,
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}

crate::primitive! {
    name: MpmGridVelocity,
    type_id: "node.mpm_grid_velocity",
    purpose: "Resolve the Live Water accumulation wire into grid velocities (design step 5): v_i = dequantised momentum / dequantised mass + gravity*step_dt for nonempty cells, zero for empty cells, written as WaterGridCell (velocity xyz, mass w). Then the static-basin no-penetration boundary: nodes at or outside a basin face may not move into the wall — only the normal component is removed, tangential flow is free-slip. Bounds arrive as params (defaults enclose the design section 8 pool); gravity arrives on three optional scalar wires (default -9.81 m/s^2 Y). The translating-collider boundary is node.water_collide_box / S5, not this stage.",
    inputs: {
        accumulator: Channels["water_grid_accum": I32] required,
        step_dt: ScalarF32 optional,
        gravity_x: ScalarF32 optional,
        gravity_y: ScalarF32 optional,
        gravity_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(WaterGridCell),
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
            name: Cow::Borrowed("basin_min_x"),
            label: "Basin Min X",
            ty: ParamType::Float,
            default: ParamValue::Float(BASIN_MIN[0]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_min_y"),
            label: "Basin Min Y",
            ty: ParamType::Float,
            default: ParamValue::Float(BASIN_MIN[1]),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_min_z"),
            label: "Basin Min Z",
            ty: ParamType::Float,
            default: ParamValue::Float(BASIN_MIN[2]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_max_x"),
            label: "Basin Max X",
            ty: ParamType::Float,
            default: ParamValue::Float(BASIN_MAX[0]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_max_y"),
            label: "Basin Max Y",
            ty: ParamType::Float,
            default: ParamValue::Float(BASIN_MAX[1]),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_max_z"),
            label: "Basin Max Z",
            ty: ParamType::Float,
            default: ParamValue::Float(BASIN_MAX[2]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cell_count"),
            label: "Grid Cells",
            ty: ParamType::Int,
            default: ParamValue::Float(CELL_COUNT as f32),
            range: Some((1.0, 16_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Fourth stage of the repeated water region body, after the two mpm_scatter stages. The accumulator is consumed BufferGather-style (the body reads the four i32 slots of its own cell); output cell_count must be exactly nx*ny*nz for the configured domain. Basin defaults match node.seed_water's default pool: floor at the pool bottom, walls 0.125 m outside the pool edge.",
    examples: [],
    picker: { label: "MPM Grid Velocity", category: Atom },
    summary: "Turns the grid's accumulated momentum into cell velocities, adds gravity, and stops the wall and floor cells from pushing into the basin.",
    category: Particles3D,
    role: Filter,
    aliases: ["mpm grid velocity", "grid resolve", "grid force", "water grid"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mpm_grid_velocity_body.wgsl"),
    input_access: [BufferGather],
    derived_uniforms: ["gravity:vec3"],
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
}

impl Primitive for MpmGridVelocity {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            match params.get("cell_count") {
                Some(ParamValue::Float(f)) => Some(f.round().max(0.0) as u32),
                _ => Some(CELL_COUNT),
            }
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let step_dt = ctx.scalar_or_param("step_dt", DEFAULT_STEP_DT);
        let read_axis = |name: &str, default: f32| match ctx.inputs.scalar(name) {
            Some(ParamValue::Float(f)) => f,
            _ => default,
        };
        let gravity = [
            read_axis("gravity_x", GRAVITY[0]),
            read_axis("gravity_y", GRAVITY[1]),
            read_axis("gravity_z", GRAVITY[2]),
        ];
        let read_param = |name: &str, default: f32| match ctx.params.get(name) {
            Some(ParamValue::Float(f)) => *f,
            _ => default,
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let Some(accum) = ctx.inputs.array("accumulator") else {
            return;
        };
        let capacity = (out_buf.size / std::mem::size_of::<WaterGridCell>() as u64) as u32;
        let cell_count = match ctx.params.get("cell_count") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => CELL_COUNT,
        };
        let cell_count = cell_count.min(capacity);
        if cell_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel
            // is generated from the `wgsl_body`; the BufferGather accumulator
            // keeps the atom a fusion boundary in practice.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.mpm_grid_velocity standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.mpm_grid_velocity",
            )
        });

        let uniforms = GridVelocityUniforms {
            step_dt,
            basin_min_x: read_param("basin_min_x", BASIN_MIN[0]),
            basin_min_y: read_param("basin_min_y", BASIN_MIN[1]),
            basin_min_z: read_param("basin_min_z", BASIN_MIN[2]),
            basin_max_x: read_param("basin_max_x", BASIN_MAX[0]),
            basin_max_y: read_param("basin_max_y", BASIN_MAX[1]),
            basin_max_z: read_param("basin_max_z", BASIN_MAX[2]),
            cell_count: cell_count as i32,
            gravity_x: gravity[0],
            gravity_y: gravity[1],
            gravity_z: gravity[2],
            dispatch_count: cell_count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // uniform(0), accumulator(1), grid out(2).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: accum,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [cell_count.div_ceil(256), 1, 1],
            "node.mpm_grid_velocity",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn grid_velocity_declares_accumulator_in_grid_out() {
        use crate::node_graph::ports::{ArrayType, ChannelElementType, PortType};
        let grid_layout = ArrayType::of_known::<WaterGridCell>();
        assert_eq!(MpmGridVelocity::TYPE_ID, "node.mpm_grid_velocity");
        let accum_in = MpmGridVelocity::INPUTS
            .iter()
            .find(|p| p.name == "accumulator")
            .expect("accumulator input");
        let PortType::Array(at) = &accum_in.ty else {
            panic!("accumulator must be an array wire");
        };
        assert_eq!(at.specs[0].ty, ChannelElementType::I32);
        assert_eq!(MpmGridVelocity::OUTPUTS.len(), 1);
        assert_eq!(MpmGridVelocity::OUTPUTS[0].name, "out");
        assert_eq!(MpmGridVelocity::OUTPUTS[0].ty, PortType::Array(grid_layout));
    }

    #[test]
    fn grid_velocity_registers_as_palette_atom() {
        let prim = MpmGridVelocity::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mpm_grid_velocity");
    }

    #[test]
    fn grid_velocity_codegen_binds_accumulator_gather() {
        let wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<MpmGridVelocity>()
                .expect("node.mpm_grid_velocity standalone codegen");
        assert!(wgsl.contains("var<storage, read> buf_accumulator: array<i32>"));
        assert!(wgsl.contains("gravity_x"));
        assert!(wgsl.contains("vec4<f32>"), "single-channel Vec4F output");
    }
}
