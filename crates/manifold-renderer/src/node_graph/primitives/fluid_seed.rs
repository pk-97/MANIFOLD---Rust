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
//! Dispatched on chain init or on clip-trigger — not every frame.

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
    purpose: "Seed an Array<Particle> with one of 7 geometric patterns (center cluster / horizontal lines / vertical lines / concentric rings / diagonal cross / spiral / edge ring). Bit-exact port of FluidSim's Unity SeedPatternKernel. Excess particles (beyond visible_count) sit dead at the center nozzle. When the optional `trigger` input is wired, the seed dispatches only on integer-edge changes of that input (matches FluidSim2D's clip-trigger mode 3 re-seed semantics). When `trigger` is unwired the seed dispatches every frame — pair with `node.array_feedback` to capture-once-and-loop.",
    inputs: {
        // Optional edge-triggered dispatch gate. When wired the seed
        // only re-runs on integer-edge changes (first observation
        // always fires so the buffer initialises). When unwired,
        // dispatches every frame (legacy behaviour for `node.array_feedback`-
        // seeded chains).
        trigger: ScalarF32 optional,
        // Port-shadows of the count params so the outer-card
        // Particles / Fill sliders can drive the same active/visible
        // bounds the FluidSim primitives use downstream.
        active_count: ScalarF32 optional,
        visible_count: ScalarF32 optional,
        // Port-shadow of the hash seed param. Wire `gen_input.trigger_count`
        // here so each clip-trigger re-seed produces a fresh random
        // placement (matches FluidSim's `trigger_count` argument to
        // `dispatch_seed`). The integer trigger value goes into the
        // shader as the seed for the Wang hash that places particles
        // along the pattern.
        trigger_count: ScalarF32 optional,
        // Port-shadow of the pattern enum. Wire `node.clip_trigger_cycle`
        // here to drive pattern selection per clip-trigger event (the
        // legacy "cycle through 7 patterns" behaviour for mode 3).
        pattern: ScalarF32 optional,
    },
    outputs: {
        particles: Array(Particle),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(1_048_576.0),
            range: Some((1024.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "visible_count",
            label: "Visible Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
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
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "max_capacity governs chain-build allocation; active_count is the upper bound of pattern-placed particles; visible_count is the upper bound of *alive* particles (rest are dead at center nozzle). For FluidSim parity: pair with node.fluid_simulate downstream — the seed runs once on init or on clip-trigger, the simulate runs every frame. trigger_count is the hash seed inside the shader (per-particle randomness); the `trigger` input port (separate) is the dispatch gate for edge-triggered re-seeding. Wire the same upstream signal into both if you want each clip-trigger to fire a fresh-randomised re-seed.",
    examples: [],
    picker: { label: "Fluid Seed", category: Atom },
    extra_fields: {
        // Tracks the last value seen on the `trigger` input port so
        // we can detect integer-edge changes and only dispatch when
        // something has actually changed. `None` means "no observation
        // yet" — the first observation always fires so the buffer
        // gets initialised.
        last_trigger: Option<i32> = None,
    },
}

impl Primitive for FluidSeed {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.inputs.scalar("active_count") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => match ctx.params.get("active_count") {
                Some(ParamValue::Float(n)) => n.round().max(0_f32) as u32,
                _ => 100_000,
            },
        };
        let visible_count = match ctx.inputs.scalar("visible_count") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => match ctx.params.get("visible_count") {
                Some(ParamValue::Float(n)) => n.round().max(0_f32) as u32,
                _ => 100_000,
            },
        };
        let pattern_index = match ctx.inputs.scalar("pattern") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => match ctx.params.get("pattern") {
                Some(ParamValue::Enum(n)) => *n,
                _ => 0,
            },
        };
        let trigger_count = match ctx.inputs.scalar("trigger_count") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => match ctx.params.get("trigger_count") {
                Some(ParamValue::Float(n)) => n.round() as u32,
                _ => 0,
            },
        };

        // Edge-gated dispatch. When the `trigger` input is wired,
        // only dispatch on integer-edge changes (first observation
        // always fires so the buffer initialises). When unwired,
        // dispatch every frame.
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("trigger") {
            let current = v.round() as i32;
            let should_fire = match self.last_trigger {
                None => true,
                Some(prev) => current != prev,
            };
            self.last_trigger = Some(current);
            if !should_fire {
                return;
            }
        }

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
    fn fluid_seed_declares_optional_trigger_input_and_particle_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        assert_eq!(FluidSeed::TYPE_ID, "node.fluid_seed");
        let names: Vec<&str> = FluidSeed::INPUTS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["trigger", "active_count", "visible_count", "trigger_count", "pattern"]
        );
        for input in FluidSeed::INPUTS {
            assert!(!input.required, "{} should be optional", input.name);
        }
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
