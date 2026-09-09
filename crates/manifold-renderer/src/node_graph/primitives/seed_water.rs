//! `node.seed_water` — S4 solver stage: seed the static pool lattice.
//!
//! Pure source: fills the particle wire with the deterministic h/2-spaced
//! lattice inside the configured pool box (velocity and affine state zero,
//! density at rest, mass `rho0*(h/2)^3`), and writes zero records to every
//! slot past the seed count so the full capacity is initialised in one
//! dispatch. Emission (S5) grows the live set from the zeroed tail.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::{
    GRID_SPACING, PARTICLE_CAPACITY, REST_DENSITY, WaterParticle,
};

/// Default pool box (design section 8): centred 2x0.5x2 m, bottom resting on
/// the static basin floor (`mpm_grid_velocity::BASIN_MIN`) — the bottom
/// lattice layer centres sit exactly at floor level (cell-centred seeding at
/// h/2 spacing), so the still pool starts in equilibrium instead of dropping
/// onto the floor and sloshing through the settling proof.
pub const POOL_MIN: [f32; 3] = [-1.0, 0.234375, -1.0];
/// Default pool box top (0.5 m deep from [`POOL_MIN`]).
pub const POOL_MAX: [f32; 3] = [1.0, 0.75, 1.0];

/// Generated-codegen uniform layout: scalar params in PARAMS order (the six
/// pool bounds, `grid_spacing`, `rest_density`, then the allocation-only
/// `max_capacity` Int -> i32), then the codegen-injected `dispatch_count`
/// (u32), padded to 16 bytes. 10 words + 2 pad = 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SeedWaterUniforms {
    pub pool_min_x: f32,
    pub pool_min_y: f32,
    pub pool_min_z: f32,
    pub pool_max_x: f32,
    pub pool_max_y: f32,
    pub pool_max_z: f32,
    pub grid_spacing: f32,
    pub rest_density: f32,
    pub max_capacity: i32,
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
}

crate::primitive! {
    name: SeedWater,
    type_id: "node.seed_water",
    purpose: "Seed the Live Water pool: emit a deterministic lattice of WaterParticle records at h/2 spacing inside a configured box, zero velocity and affine state, rest density, particle mass rho0*(h/2)^3. Slots past the seed count are written as zero records (mass zero = inactive) so the whole capacity is initialised in one dispatch. Memoized by the water state boundary until the seed config changes; emission (node.water_emit) fills the zeroed tail from a deterministic cursor.",
    inputs: {},
    outputs: {
        out: Array(WaterParticle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("pool_min_x"),
            label: "Pool Min X",
            ty: ParamType::Float,
            default: ParamValue::Float(POOL_MIN[0]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pool_min_y"),
            label: "Pool Min Y",
            ty: ParamType::Float,
            default: ParamValue::Float(POOL_MIN[1]),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pool_min_z"),
            label: "Pool Min Z",
            ty: ParamType::Float,
            default: ParamValue::Float(POOL_MIN[2]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pool_max_x"),
            label: "Pool Max X",
            ty: ParamType::Float,
            default: ParamValue::Float(POOL_MAX[0]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pool_max_y"),
            label: "Pool Max Y",
            ty: ParamType::Float,
            default: ParamValue::Float(POOL_MAX[1]),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pool_max_z"),
            label: "Pool Max Z",
            ty: ParamType::Float,
            default: ParamValue::Float(POOL_MAX[2]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("grid_spacing"),
            label: "Grid Spacing h",
            ty: ParamType::Float,
            default: ParamValue::Float(GRID_SPACING),
            range: Some((0.01, 0.25)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rest_density"),
            label: "Rest Density",
            ty: ParamType::Float,
            default: ParamValue::Float(REST_DENSITY),
            range: Some((100.0, 2000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(PARTICLE_CAPACITY as f32),
            range: Some((1.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "First stage of the water solver region: `seed_water -> water_state`, with the region body (clear_grid, mpm_scatter_*, mpm_grid_velocity, mpm_gather_advect, water_validate, water_commit) repeating between boundary captures. Defaults seed the design section 8 pool: centred 2x0.5x2 m resting on the static basin floor, 65,536 particles on the h/2 lattice.",
    examples: [],
    picker: { label: "Seed Water", category: Atom },
    summary: "Creates the starting block of water particles on a fixed lattice — the still pool the rest of the solver acts on.",
    category: Particles3D,
    role: Source,
    aliases: ["seed water", "water seed", "pool seed", "water source"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/seed_water_body.wgsl"),
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
}

impl Primitive for SeedWater {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read = |name: &str, default: f32| match ctx.params.get(name) {
            Some(ParamValue::Float(f)) => *f,
            _ => default,
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let capacity = (out_buf.size / std::mem::size_of::<WaterParticle>() as u64) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel
            // is generated from the `wgsl_body` so the atom participates in
            // freeze fusion.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.seed_water standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.seed_water",
            )
        });

        let uniforms = SeedWaterUniforms {
            pool_min_x: read("pool_min_x", POOL_MIN[0]),
            pool_min_y: read("pool_min_y", POOL_MIN[1]),
            pool_min_z: read("pool_min_z", POOL_MIN[2]),
            pool_max_x: read("pool_max_x", POOL_MAX[0]),
            pool_max_y: read("pool_max_y", POOL_MAX[1]),
            pool_max_z: read("pool_max_z", POOL_MAX[2]),
            grid_spacing: read("grid_spacing", GRID_SPACING),
            rest_density: read("rest_density", REST_DENSITY),
            max_capacity: capacity as i32,
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
        };

        // Seed every slot: lattice records for the pool, zero records for
        // the emission tail.
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
            "node.seed_water",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn seed_water_declares_particle_source() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<WaterParticle>();
        assert_eq!(SeedWater::TYPE_ID, "node.seed_water");
        assert!(SeedWater::INPUTS.is_empty());
        assert_eq!(SeedWater::OUTPUTS.len(), 1);
        assert_eq!(SeedWater::OUTPUTS[0].name, "out");
        assert_eq!(SeedWater::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn seed_water_registers_as_palette_atom() {
        let prim = SeedWater::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.seed_water");
    }

    #[test]
    fn seed_water_codegen_emits_particle_struct_and_body() {
        let wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<SeedWater>()
                .expect("node.seed_water standalone codegen");
        assert!(wgsl.contains("struct Element"), "particle struct synthesized");
        assert!(wgsl.contains("position_mass"), "WATER_PARTICLE_SPECS field order");
        assert!(wgsl.contains("fn body"), "body fragment embedded");
        assert!(wgsl.contains("WATER_FIXED_SCALE"), "water_common include prepended");
    }

    /// Drift gate: the WGSL common file hardcodes the S1 domain constants
    /// (they cannot reach WGSL any other way). This test fails the moment
    /// either side moves.
    #[test]
    fn water_domain_constants_match() {
        use crate::node_graph::water::{
            DOMAIN_ORIGIN, GRID_FIXED_SCALE, GRID_NODES, GRID_SPACING,
        };
        let common = include_str!("shaders/water_common.wgsl");
        assert_eq!(DOMAIN_ORIGIN, [-2.0, 0.0, -2.0]);
        assert!(
            common.contains(&format!("const WATER_GRID_N: u32 = {GRID_NODES}u;")),
            "WATER_GRID_N drifted from GRID_NODES"
        );
        assert!(
            common.contains(&format!("const WATER_H: f32 = {};", GRID_SPACING)),
            "WATER_H drifted from GRID_SPACING"
        );
        assert!(
            common.contains("const WATER_ORIGIN: vec3<f32> = vec3<f32>(-2.0, 0.0, -2.0);"),
            "WATER_ORIGIN drifted from DOMAIN_ORIGIN"
        );
        assert_eq!(GRID_FIXED_SCALE, 1 << 20);
        assert!(common.contains("const WATER_FIXED_SCALE: f32 = 1048576.0;"));
        assert!(common.contains("const WATER_C0_SQ_OVER_7: f32 = 100000.0 / 7.0;"));
    }
}
