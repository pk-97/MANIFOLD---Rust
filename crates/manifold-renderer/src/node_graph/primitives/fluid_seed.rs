//! `node.fluid_seed` — 7-pattern particle seeder for the FluidSim
//! family. Bit-exact wrap of
//! `generators/shaders/fluid_seed.wgsl` via include_str.
//!
//! On each dispatch, writes initial position + life to each particle
//! in an Array<Particle> based on a chosen pattern. Active particles
//! (i < visible_count) are placed by the pattern; excess particles
//! are placed at the center "nozzle" with life = 0 (dead, ready to
//! be culled or respawned).
//!
//! Patterns (0..=6):
//!   0 — Center cluster (CLT Gaussian approximation)
//!   1 — Horizontal lines (6 lines)
//!   2 — Vertical lines (6 lines)
//!   3 — Concentric rings (3 rings)
//!   4 — Diagonal cross (X-shape)
//!   5 — Archimedean spiral
//!   6 — Edge ring (implodes inward)
//!
//! Dispatched on chain init or on snap trigger — not every frame.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const FLUID_SEED_PATTERNS: &[&str] = &[
    "Center Cluster",
    "Horizontal Lines",
    "Vertical Lines",
    "Concentric Rings",
    "Diagonal Cross",
    "Spiral",
    "Edge Ring",
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FluidSeedUniforms {
    active_count: u32,
    pattern_index: u32,
    trigger_count: u32,
    visible_count: u32,
}

crate::primitive! {
    name: FluidSeed,
    type_id: "node.fluid_seed",
    purpose: "Seed an Array<Particle> with one of 7 geometric patterns (center cluster / horizontal lines / vertical lines / concentric rings / diagonal cross / spiral / edge ring). Bit-exact port of FluidSim's Unity SeedPatternKernel. Dispatched on chain init or snap trigger — not every frame. Excess particles (beyond visible_count) sit dead at the center nozzle.",
    inputs: {},
    outputs: {
        particles: Array(Particle),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Int(1_048_576),
            range: Some((1024.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Int(100_000),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "visible_count",
            label: "Visible Count",
            ty: ParamType::Int,
            default: ParamValue::Int(100_000),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "pattern",
            label: "Pattern",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: FLUID_SEED_PATTERNS,
        },
        ParamDef {
            name: "trigger_count",
            label: "Trigger Seed",
            ty: ParamType::Int,
            default: ParamValue::Int(0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "max_capacity governs chain-build allocation; active_count is the upper bound of pattern-placed particles; visible_count is the upper bound of *alive* particles (rest are dead at center nozzle). For FluidSim parity: pair with node.fluid_simulate downstream — the seed runs once on init or on snap, the simulate runs every frame. trigger_count is the hash seed — increment per snap event for fresh randomisation.",
    examples: [],
    picker: { label: "Fluid Seed", category: Atom },
}

impl Primitive for FluidSeed {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 100_000,
        };
        let visible_count = match ctx.params.get("visible_count") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 100_000,
        };
        let pattern_index = match ctx.params.get("pattern") {
            Some(ParamValue::Enum(n)) => (*n).max(0) as u32,
            _ => 0,
        };
        let trigger_count = match ctx.params.get("trigger_count") {
            Some(ParamValue::Int(n)) => (*n) as u32,
            _ => 0,
        };

        let Some(out_buf) = ctx.outputs.array("particles") else {
            return;
        };
        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (out_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);
        let visible_count = visible_count.min(active_count);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_seed.wgsl"),
                "main",
                "node.fluid_seed",
            )
        });

        let uniforms = FluidSeedUniforms {
            active_count,
            pattern_index,
            trigger_count,
            visible_count,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: out_buf,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.fluid_seed",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fluid_seed_declares_zero_inputs_and_particle_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType {
            item_size: std::mem::size_of::<Particle>() as u32,
            item_align: std::mem::align_of::<Particle>() as u32,
        };
        assert_eq!(FluidSeed::TYPE_ID, "node.fluid_seed");
        assert!(FluidSeed::INPUTS.is_empty());
        assert_eq!(FluidSeed::OUTPUTS.len(), 1);
        assert_eq!(FluidSeed::OUTPUTS[0].name, "particles");
        assert_eq!(
            FluidSeed::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );
    }

    #[test]
    fn fluid_seed_has_seven_pattern_options() {
        let pattern_param = FluidSeed::PARAMS
            .iter()
            .find(|p| p.name == "pattern")
            .unwrap();
        assert_eq!(pattern_param.ty, ParamType::Enum);
        assert_eq!(pattern_param.enum_values.len(), 7);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FluidSeed::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fluid_seed");
    }
}
