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

const SHADER_SOURCE: &str = include_str!("shaders/liquid_stats.wgsl");
/// Records one workgroup folds: 256 threads × 8.
const BLOCK: u32 = 256 * 8;
/// Bytes of one partial record.
const PARTIAL_BYTES: u64 = 40;

/// Scratch for the reduction over `count` records: one partial per block.
pub(crate) fn partial_bytes(count: u32) -> u64 {
    u64::from(count.div_ceil(BLOCK).max(1)) * PARTIAL_BYTES
}

/// Words in the stats array: 0 non-finite records, 1 live records, 2 fastest
/// speed (m/s), 3 mass (kg), 4-6 momentum (kg·m/s), 7 kinetic energy (J),
/// 8 solver-reported capped stages (zero for native GPU FLIP), 9 solid push-outs refused, 10
/// pressure solve iterations, 11 density solve iterations, 12 solves that
/// reached their cap without converging, 13 steps whose sealed-pocket spread
/// reached its cap unfinished, 14 and 15 the volume rate (m³/s) taken off
/// sealed pockets' pressure and density right-hand sides so their solves
/// have a solution, 16 the share of the lattice's 8³ tiles the cell passes
/// ran over, 17-19 the last substep's dry, sealed and air cells as the
/// sealed-pocket pass classed them, 20-25 its lowest water cell that touches
/// air directly (its index, the dry neighbour's index or 0xffffffff for an
/// open box face, the face's open fraction bits, axis * 2 + 1 on the high
/// side, the neighbour's phi bits and its particle count), 26 its floor cells
/// reading dry with water on every in-box side (8-26 are 0 without a `capped`
/// input). Word 27 is the narrow-band reseed capacity shortage. Floats are
/// stored as bits.
/// The first solver word in the published stats array.
pub const SOLVER_STATS_START: u32 = 10;
/// The published stats word containing narrow-band reseed shortages.
pub const NARROW_BAND_SHORTAGE_WORD: u32 = 27;
/// The number of words in the stats array.
pub const LIQUID_STATS_WORDS: u32 = NARROW_BAND_SHORTAGE_WORD + 1;
/// The number of solver words copied from the capped counters.
pub const SOLVER_WORDS: u32 = LIQUID_STATS_WORDS - SOLVER_STATS_START;
/// The narrow-band shortage's offset within the solver tail.
pub const NARROW_BAND_SHORTAGE_TAIL: u32 = NARROW_BAND_SHORTAGE_WORD - SOLVER_STATS_START;

/// One tick's statistics, decoded from the stats words.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiquidTickStats {
    pub nonfinite: u32,
    pub live: u32,
    pub max_speed: f32,
    pub mass: f32,
    pub momentum: [f32; 3],
    pub kinetic: f32,
    /// Solver-reported clipped stages; native GPU FLIP reports zero (direct RK3).
    pub speed_capped: u32,
    /// Solid push-outs the solver refused as too far this tick.
    pub push_refused: u32,
    /// Pressure solve iterations this tick, every substep.
    pub pressure_iterations: u32,
    /// Density solve iterations this tick, every substep.
    pub density_iterations: u32,
    /// Solves this tick that reached their iteration cap without converging.
    pub unconverged: u32,
    /// Steps this tick whose sealed-pocket spread reached its cap unfinished.
    pub unresolved_pockets: u32,
    /// Volume rate (m³/s) taken off sealed pockets' pressure right-hand side
    /// this tick, summed over its steps.
    pub pressure_flux_removed: f32,
    /// The same for the density projection's source.
    pub density_flux_removed: f32,
    /// The share of the lattice's 8³ tiles the step's cell passes ran over
    /// (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 5 (Stats)), the tick's
    /// last substep.
    pub active_tiles: f32,
    /// The last substep's cells the sealed-pocket pass classed dry, sealed
    /// (water no air reaches) and touching air.
    pub pocket_cells: [u32; 3],
    /// The last substep's lowest air seed: cell, neighbour, open fraction
    /// bits, side, the neighbour's φ bits and particle count, as words 20-25.
    pub first_air_seed: [u32; 6],
    /// The last substep's floor cells reading dry with water on every
    /// in-box side: holes under the water.
    pub dry_floor_cells: u32,
    /// Narrow-band reseed sites that could not fit in the particle pool this
    /// tick. A nonzero value faults the liquid and blocks publication.
    pub narrow_band_shortage: u32,
}

impl LiquidTickStats {
    pub fn from_words(w: &[u32]) -> Self {
        let f = |i: usize| f32::from_bits(w[i]);
        Self {
            nonfinite: w[0],
            live: w[1],
            max_speed: f(2),
            mass: f(3),
            momentum: [f(4), f(5), f(6)],
            kinetic: f(7),
            speed_capped: w[8],
            push_refused: w[9],
            pressure_iterations: w[10],
            density_iterations: w[11],
            unconverged: w[12],
            unresolved_pockets: w[13],
            pressure_flux_removed: f(14),
            density_flux_removed: f(15),
            active_tiles: f(16),
            pocket_cells: [w[17], w[18], w[19]],
            first_air_seed: [w[20], w[21], w[22], w[23], w[24], w[25]],
            dry_floor_cells: w[26],
            narrow_band_shortage: w[NARROW_BAND_SHORTAGE_WORD as usize],
        }
    }
}

/// Prepend the stats ABI constants to a WGSL source string. This is public so
/// shader conformance tests and sibling FLIP kernels can validate and compile
/// the exact layout emitted by the stats writer, keeping Rust readers and GPU
/// writers on one source of truth.
pub fn with_stats_layout(source: &str) -> String {
    format!(
        "const SOLVER_STATS_START: u32 = {SOLVER_STATS_START}u;\nconst SOLVER_WORDS: u32 = {SOLVER_WORDS}u;\nconst NARROW_BAND_SHORTAGE_WORD: u32 = {NARROW_BAND_SHORTAGE_WORD}u;\nconst NARROW_BAND_SHORTAGE_TAIL: u32 = {NARROW_BAND_SHORTAGE_TAIL}u;\n\n{source}"
    )
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StatsParams {
    count: u32,
    groups: u32,
    particle_mass: f32,
    /// 1 when a `capped` array is bound.
    has_capped: u32,
    /// Particle slots: the solver words start at word 2 · slots of `capped`.
    slots: u32,
    /// 1 when the `capped` array holds the solver words.
    has_solver: u32,
    _pad: [u32; 2],
}

pub struct StatsPipelines {
    particles: GpuComputePipeline,
    finish: GpuComputePipeline,
}

impl StatsPipelines {
    fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let shader = with_stats_layout(SHADER_SOURCE);
        Self {
            particles: device.create_compute_pipeline(&shader, "particles_main", "node.liquid_stats.particles"),
            finish: device.create_compute_pipeline(&shader, "finish_main", "node.liquid_stats.finish"),
        }
    }
}

crate::primitive! {
    name: LiquidStats,
    type_id: "node.liquid_stats",
    purpose: "Reduce a particle liquid's first `count` records to its statistics words, written in place over `stats`: 0 records with a non-finite position, radius or velocity, 1 live records (radius above 0), 2 the fastest speed, 3 mass, 4-6 momentum and 7 kinetic energy, each particle weighing particle_mass, and when capped is wired, 8 and 9 its two words per record summed: the solver's speed-capped move stages and refused solid push-outs, and 10-16 the solver words after the records: pressure and density solve iterations, the solves that reached their cap without converging, the steps whose sealed-pocket spread reached its cap unfinished, 14-15 the volume rate taken off sealed pockets' pressure and density right-hand sides, and 16 the share of the lattice's 8³ tiles the cell passes ran over. Sums run in a fixed order with no atomics, so the words are the same on every run.",
    inputs: {
        particles: Array(FluidParticle) required,
        stats: Array(u32) required,
        capped: Array(u32) optional,
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
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(StatsPipelines::new(device));
        }
    }

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
        let capped = ctx.inputs.array("capped");
        // In place on the stats input.
        ctx.mark_gpu_accessed();
        let (Some(particles), Some(stats)) = (particles, stats) else { return };
        if stats.size < u64::from(LIQUID_STATS_WORDS) * 4 {
            ctx.error(format!("Liquid Stats: the stats array holds fewer than {LIQUID_STATS_WORDS} words"));
            return;
        }
        let count = requested.min((particles.size / std::mem::size_of::<FluidParticle>() as u64) as u32);
        let partial_bytes = partial_bytes(count);
        let gpu = ctx.gpu_encoder();
        let pipelines = self.pipelines.as_ref().expect("liquid stats pipelines built by prepare_pipelines at install");
        if self.partials.as_ref().is_none_or(|b| b.size < partial_bytes) {
            self.partials = Some(gpu.device.create_buffer(partial_bytes));
        }
        let partials = self.partials.as_ref().expect("partials prepared");
        let slots = (particles.size / std::mem::size_of::<FluidParticle>() as u64).min(u64::from(u32::MAX)) as u32;
        let has_solver = capped.is_some_and(|c| c.size >= (2 * u64::from(slots) + u64::from(SOLVER_WORDS)) * 4);
        let count = capped.map_or(count, |c| count.min((c.size / 8).min(u64::from(u32::MAX)) as u32));
        let groups = count.div_ceil(BLOCK);
        let params = StatsParams {
            count,
            groups,
            particle_mass,
            has_capped: u32::from(capped.is_some()),
            slots,
            has_solver: u32::from(has_solver),
            _pad: [0; 2],
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
            GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: partials, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: stats, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: capped.unwrap_or(particles), offset: 0 },
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
        let shader = with_stats_layout(SHADER_SOURCE);
        assert_eq!(std::mem::size_of::<StatsParams>(), 32);
        assert!(shader.contains("struct StatsParams"));
        assert_eq!(std::mem::size_of::<FluidParticle>(), 32);
        assert!(!shader.contains("atomic"), "GPU FLIP: the tick statistics use no atomics");
        let module = naga::front::wgsl::parse_str(&shader).expect("liquid_stats.wgsl parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("liquid_stats.wgsl validates");
    }

    #[test]
    fn liquid_stats_words_decode() {
        let mut words = [0u32; LIQUID_STATS_WORDS as usize];
        words[..17].copy_from_slice(&[2, 5, 1.5f32.to_bits(), 0.25f32.to_bits(), 1.0f32.to_bits(), (-2.0f32).to_bits(), 0.0f32.to_bits(), 3.0f32.to_bits(), 4, 1, 40, 12, 1, 2, 0.5f32.to_bits(), 0.125f32.to_bits(), 0.3125f32.to_bits()]);
        words[17..27].copy_from_slice(&[3, 4, 5, 6, 7, 0.75f32.to_bits(), 3, 0.5f32.to_bits(), 9, 2]);
        words[NARROW_BAND_SHORTAGE_WORD as usize] = 11;
        let stats = LiquidTickStats::from_words(&words);
        assert_eq!(
            stats,
            LiquidTickStats {
                nonfinite: 2,
                live: 5,
                max_speed: 1.5,
                mass: 0.25,
                momentum: [1.0, -2.0, 0.0],
                kinetic: 3.0,
                speed_capped: 4,
                push_refused: 1,
                pressure_iterations: 40,
                density_iterations: 12,
                unconverged: 1,
                unresolved_pockets: 2,
                pressure_flux_removed: 0.5,
                density_flux_removed: 0.125,
                active_tiles: 0.3125,
                pocket_cells: [3, 4, 5],
                first_air_seed: [6, 7, 0.75f32.to_bits(), 3, 0.5f32.to_bits(), 9],
                dry_floor_cells: 2,
                narrow_band_shortage: 11,
            }
        );
        assert_eq!(words.len(), LIQUID_STATS_WORDS as usize);
    }
}
