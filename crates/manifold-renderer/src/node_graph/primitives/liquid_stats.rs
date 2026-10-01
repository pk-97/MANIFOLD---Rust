//! `node.liquid_stats` — once per tick, reduce a particle liquid's state to
//! its statistics words (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.1,
//! amendment 2): non-finite records, live records, the fastest particle,
//! mass, momentum and kinetic energy. A fixed reduction tree with no atomics
//! keeps the words the same on every run. Exempt from the codegen mandate as
//! a barriered reduction (ADDING_PRIMITIVES.md exclusion 1).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SHADER: &str = include_str!("shaders/liquid_stats.wgsl");
/// Records one workgroup folds: 256 threads × 8.
const BLOCK: u32 = 256 * 8;
/// Bytes of one partial record.
const PARTIAL_BYTES: u64 = 32;

/// Scratch for the reduction over `count` records: one partial per block.
pub(crate) fn partial_bytes(count: u32) -> u64 {
    u64::from(count.div_ceil(BLOCK).max(1)) * PARTIAL_BYTES
}

/// Words in the stats array: 0 non-finite records, 1 live records, 2 fastest
/// speed (m/s), 3 mass (kg), 4-6 momentum (kg·m/s), 7 kinetic energy (J).
/// Floats are stored as bits.
pub const LIQUID_STATS_WORDS: u32 = 8;

/// One tick's statistics, decoded from the stats words.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiquidTickStats {
    pub nonfinite: u32,
    pub live: u32,
    pub max_speed: f32,
    pub mass: f32,
    pub momentum: [f32; 3],
    pub kinetic: f32,
}

impl LiquidTickStats {
    pub fn from_words(w: &[u32]) -> Self {
        let f = |i: usize| f32::from_bits(w[i]);
        Self { nonfinite: w[0], live: w[1], max_speed: f(2), mass: f(3), momentum: [f(4), f(5), f(6)], kinetic: f(7) }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StatsParams {
    count: u32,
    groups: u32,
    particle_mass: f32,
    _pad0: u32,
}

pub struct StatsPipelines {
    particles: GpuComputePipeline,
    finish: GpuComputePipeline,
}

crate::primitive! {
    name: LiquidStats,
    type_id: "node.liquid_stats",
    purpose: "Reduce a particle liquid's first `count` records to its statistics words, written in place over `stats`: 0 records with a non-finite position, radius or velocity, 1 live records (radius above 0), 2 the fastest speed, 3 mass, 4-6 momentum and 7 kinetic energy, each particle weighing particle_mass. Sums run in a fixed order with no atomics, so the words are the same on every run.",
    inputs: {
        particles: Array(FluidParticle) required,
        stats: Array(u32) required,
        count: ScalarF32 optional,
        particle_mass: ScalarF32 optional,
    },
    outputs: {
        stats_out: Array(u32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("count"), label: "Count", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("particle_mass"), label: "Particle Mass (kg)", ty: ParamType::Float, default: ParamValue::Float(0.030_517_578), range: Some((0.0, 1.0e6)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The last atom of a particle liquid's tick, inside node.liquid_state's region: particles from the tick's last step, stats and stats_out aliased with node.liquid_state's stats result, which escapes the region to node.liquid_frame (it never publishes a non-finite tick) and to liquid_state's own readback (live count, fault). count and particle_mass come from the fill and the domain.",
    examples: [],
    picker: { label: "Liquid Stats", category: Atom },
    summary: "Measures a particle liquid once per tick: how much there is, how fast it moves, and whether anything went wrong.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid stats", "particle diagnostics", "non-finite check"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        pipelines: Option<StatsPipelines> = None,
        partials: Option<GpuBuffer> = None,
    },
}

impl Primitive for LiquidStats {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "stats_out")
            .then(|| input_capacities.iter().find(|(p, _)| *p == "stats").map(|&(_, n)| n))
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("stats", "stats_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let requested = ctx.scalar_or_param("count", 0.0).round().max(0.0) as u32;
        let particle_mass = ctx.scalar_or_param("particle_mass", 0.030_517_578);
        let particles = ctx.inputs.array("particles");
        let stats = ctx.inputs.array("stats");
        // In place on the stats input.
        ctx.mark_gpu_accessed();
        let (Some(particles), Some(stats)) = (particles, stats) else { return };
        if stats.size < u64::from(LIQUID_STATS_WORDS) * 4 {
            ctx.error(format!("Liquid Stats: the stats array holds fewer than {LIQUID_STATS_WORDS} words"));
            return;
        }
        let count = requested.min((particles.size / std::mem::size_of::<FluidParticle>() as u64) as u32);
        let groups = count.div_ceil(BLOCK);
        let partial_bytes = partial_bytes(count);
        let gpu = ctx.gpu_encoder();
        let pipelines = self.pipelines.get_or_insert_with(|| StatsPipelines {
            particles: gpu.device.create_compute_pipeline(SHADER, "particles_main", "node.liquid_stats.particles"),
            finish: gpu.device.create_compute_pipeline(SHADER, "finish_main", "node.liquid_stats.finish"),
        });
        if self.partials.as_ref().is_none_or(|b| b.size < partial_bytes) {
            self.partials = Some(gpu.device.create_buffer(partial_bytes));
        }
        let partials = self.partials.as_ref().expect("partials prepared");
        let params = StatsParams { count, groups, particle_mass, _pad0: 0 };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
            GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: partials, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: stats, offset: 0 },
        ];
        if groups > 0 {
            gpu.native_enc.dispatch_compute(&pipelines.particles, &bindings, [groups, 1, 1], "node.liquid_stats.particles");
        }
        gpu.native_enc.dispatch_compute(&pipelines.finish, &bindings, [1, 1, 1], "node.liquid_stats.finish");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_stats_params_match_the_shader_and_use_no_atomics() {
        assert_eq!(std::mem::size_of::<StatsParams>(), 16);
        assert!(SHADER.contains("struct StatsParams"));
        assert_eq!(std::mem::size_of::<FluidParticle>(), 32);
        assert!(!SHADER.contains("atomic"), "GPU FLIP: the tick statistics use no atomics");
        let module = naga::front::wgsl::parse_str(SHADER).expect("liquid_stats.wgsl parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("liquid_stats.wgsl validates");
    }

    #[test]
    fn liquid_stats_words_decode() {
        let words = [2, 5, 1.5f32.to_bits(), 0.25f32.to_bits(), 1.0f32.to_bits(), (-2.0f32).to_bits(), 0.0f32.to_bits(), 3.0f32.to_bits()];
        let stats = LiquidTickStats::from_words(&words);
        assert_eq!(stats, LiquidTickStats { nonfinite: 2, live: 5, max_speed: 1.5, mass: 0.25, momentum: [1.0, -2.0, 0.0], kinetic: 3.0 });
        assert_eq!(words.len(), LIQUID_STATS_WORDS as usize);
    }
}
