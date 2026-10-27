//! `node.liquid_stats` — once per tick, reduce a particle liquid's state to
//! its statistics words: non-finite records, live records, the fastest particle,
//! mass, momentum and kinetic energy, over the slots the particle identity
//! counts as in use. A fixed reduction tree with no atomics keeps the words
//! the same on every run. Exempt from the codegen mandate as a barriered
//! reduction (ADDING_PRIMITIVES.md exclusion 1).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuEncoder};

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;

use super::particle_identity::IDENTITY_BYTES;

const SHADER_SOURCE: &str = include_str!("shaders/liquid_stats.wgsl");
/// Records one workgroup folds: 256 threads × 8.
const BLOCK: u32 = 256 * 8;
/// Bytes of one partial record.
const PARTIAL_BYTES: u64 = 40;
/// Bytes of the dispatch arguments `[groups, 1, 1]`.
pub(crate) const ARGS_BYTES: u64 = 12;

/// Scratch for the reduction over up to `slots` records: one partial per block.
pub(crate) fn partial_bytes(slots: u32) -> u64 {
    u64::from(slots.div_ceil(BLOCK).max(1)) * PARTIAL_BYTES
}

/// Words in the stats array: 0 non-finite records, 1 live records, 2 fastest
/// speed (m/s), 3 mass (kg), 4-6 momentum (kg·m/s), 7 kinetic energy (J),
/// 8 solver-reported capped stages (zero for native GPU FLIP), 9 solid push-outs refused, 10
/// pressure solve iterations, 11 density solve iterations, 12 solves that
/// reached their cap without converging, 13 steps whose sealed-pocket spread
/// reached its cap unfinished, 14 and 15 the volume rate (m³/s) taken off
/// sealed pockets' pressure and density right-hand sides so their solves
/// have a solution, 16 the share of the pool's tiles the water holds
/// (|C| / P), 17-19 the last substep's dry, sealed and air cells as the
/// sealed-pocket pass classed them, 20-25 its lowest water cell that touches
/// air directly (its index, the dry neighbour's index or 0xffffffff for an
/// open box face, the face's open fraction bits, axis * 2 + 1 on the high
/// side, the neighbour's phi bits and its particle count), 26 its floor cells
/// reading dry with water on every in-box side (8-26 are 0 without a `capped`
/// input). Word 27 is the particles inflows added this tick, 28 the ones they
/// wanted to add but the pool had no free slot for, 29 the sheet births the
/// pool had no free slot for, and 30 the particles removed because their tile
/// had no slot. Floats are stored as bits.
/// The first solver word in the published stats array.
pub const SOLVER_STATS_START: u32 = 10;
/// The published stats word counting this tick's inflow births.
pub const INFLOW_BIRTHS_WORD: u32 = 27;
/// The published stats word counting inflow births a full pool refused.
pub const INFLOW_REFUSED_WORD: u32 = 28;
/// The published stats word counting sheet births a full pool refused.
pub const SHEET_REFUSED_WORD: u32 = 29;
/// The published stats word counting particles removed because their tile
/// had no slot in the pool.
pub const TILE_REMOVED_WORD: u32 = 30;
/// The number of words in the stats array.
pub const LIQUID_STATS_WORDS: u32 = TILE_REMOVED_WORD + 1;
/// The number of solver words copied from the capped counters.
pub const SOLVER_WORDS: u32 = LIQUID_STATS_WORDS - SOLVER_STATS_START;
/// The inflow births' offset within the solver tail.
pub const INFLOW_BIRTHS_TAIL: u32 = INFLOW_BIRTHS_WORD - SOLVER_STATS_START;
/// The refused inflow births' offset within the solver tail.
pub const INFLOW_REFUSED_TAIL: u32 = INFLOW_REFUSED_WORD - SOLVER_STATS_START;
/// The refused sheet births' offset within the solver tail.
pub const SHEET_REFUSED_TAIL: u32 = SHEET_REFUSED_WORD - SOLVER_STATS_START;
/// The tile-removed count's offset within the solver tail.
pub const TILE_REMOVED_TAIL: u32 = TILE_REMOVED_WORD - SOLVER_STATS_START;

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
    /// Particles inflows added this tick, every substep.
    pub inflow_births: u32,
    /// Particles inflows wanted to add this tick but the pool had no free
    /// slot for: the pool is full and the inflow is held back.
    pub inflow_refused: u32,
    /// Sheet births this tick wanted a slot the pool did not have.
    pub sheet_refused: u32,
    /// Particles removed this tick because their tile had no slot: the pool
    /// is full where the water went.
    pub tile_removed: u32,
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
            inflow_births: w[INFLOW_BIRTHS_WORD as usize],
            inflow_refused: w[INFLOW_REFUSED_WORD as usize],
            sheet_refused: w[SHEET_REFUSED_WORD as usize],
            tile_removed: w[TILE_REMOVED_WORD as usize],
        }
    }
}

/// Prepend the stats ABI constants to a WGSL source string. This is public so
/// shader conformance tests and sibling FLIP kernels can validate and compile
/// the exact layout emitted by the stats writer, keeping Rust readers and GPU
/// writers on one source of truth.
pub fn with_stats_layout(source: &str) -> String {
    format!(
        "const SOLVER_STATS_START: u32 = {SOLVER_STATS_START}u;\nconst SOLVER_WORDS: u32 = {SOLVER_WORDS}u;\nconst INFLOW_BIRTHS_TAIL: u32 = {INFLOW_BIRTHS_TAIL}u;\nconst INFLOW_REFUSED_TAIL: u32 = {INFLOW_REFUSED_TAIL}u;\nconst SHEET_REFUSED_TAIL: u32 = {SHEET_REFUSED_TAIL}u;\nconst TILE_REMOVED_TAIL: u32 = {TILE_REMOVED_TAIL}u;\n\n{source}"
    )
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StatsParams {
    /// Slots the particle array holds, or `capped` when it holds fewer: the
    /// most any slot in use can reach.
    limit: u32,
    particle_mass: f32,
    /// 1 when a `capped` array is bound.
    has_capped: u32,
    /// Particle slots: the solver words start at word 2 · slots of `capped`.
    slots: u32,
    /// 1 when the `capped` array holds the solver words.
    has_solver: u32,
    _pad: [u32; 3],
}

pub struct StatsPipelines {
    args: GpuComputePipeline,
    particles: GpuComputePipeline,
    finish: GpuComputePipeline,
}

impl StatsPipelines {
    fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let shader = with_stats_layout(SHADER_SOURCE);
        Self {
            args: device.create_compute_pipeline(&shader, "stats_args", "node.liquid_stats.args"),
            particles: device.create_compute_pipeline(
                &shader,
                "particles_main",
                "node.liquid_stats.particles",
            ),
            finish: device.create_compute_pipeline(
                &shader,
                "finish_main",
                "node.liquid_stats.finish",
            ),
        }
    }

    /// The statistics of slots `[0, n)` of `job.particles`, `n` being word 7
    /// of `job.identity` and no more than the slots `job.particles` and
    /// `job.capped` hold. The arguments pass sizes the reduction on the GPU
    /// from the identity, and the finish folds as many partials.
    fn encode(&self, enc: &mut GpuEncoder, job: &StatsJob<'_>) {
        let slots = particle_slots(job.particles);
        let limit = slot_limit(job.particles, job.capped);
        let has_solver = job
            .capped
            .is_some_and(|c| c.size >= (2 * u64::from(slots) + u64::from(SOLVER_WORDS)) * 4);
        let params = StatsParams {
            limit,
            particle_mass: job.particle_mass,
            has_capped: u32::from(job.capped.is_some()),
            slots,
            has_solver: u32::from(has_solver),
            _pad: [0; 3],
        };
        let bind = |binding, buffer| GpuBinding::Buffer {
            binding,
            buffer,
            offset: 0,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&params),
            },
            bind(1, job.particles),
            bind(2, job.partials),
            bind(3, job.stats),
            bind(4, job.capped.unwrap_or(job.particles)),
            bind(5, job.identity),
            bind(6, job.args),
        ];
        enc.dispatch_compute(&self.args, &bindings, [1, 1, 1], "node.liquid_stats.args");
        enc.compute_memory_barrier_buffers();
        enc.dispatch_compute_indirect(
            &self.particles,
            &bindings,
            job.args,
            0,
            "node.liquid_stats.particles",
        );
        enc.compute_memory_barrier_buffers();
        enc.dispatch_compute(
            &self.finish,
            &bindings,
            [1, 1, 1],
            "node.liquid_stats.finish",
        );
    }
}

/// What one reduction reads and writes.
struct StatsJob<'a> {
    particles: &'a GpuBuffer,
    /// The particle identity, whose word 7 counts the slots in use.
    identity: &'a GpuBuffer,
    stats: &'a GpuBuffer,
    capped: Option<&'a GpuBuffer>,
    /// One partial per block of `slot_limit` slots.
    partials: &'a GpuBuffer,
    args: &'a GpuBuffer,
    particle_mass: f32,
}

/// Records a particle buffer holds.
fn particle_slots(particles: &GpuBuffer) -> u32 {
    (particles.size / std::mem::size_of::<FluidParticle>() as u64).min(u64::from(u32::MAX)) as u32
}

/// The most slots a reduction reads: the particle buffer's, or the `capped`
/// array's two words a slot when it holds fewer.
fn slot_limit(particles: &GpuBuffer, capped: Option<&GpuBuffer>) -> u32 {
    let slots = particle_slots(particles);
    capped.map_or(slots, |c| {
        slots.min((c.size / 8).min(u64::from(u32::MAX)) as u32)
    })
}

manifold_node_engine::primitive! {
    name: LiquidStats,
    type_id: "node.liquid_stats",
    purpose: "Reduce a particle liquid's slots in use (the first n records, n being word 7 of identity) to its statistics words, written in place over `stats`: 0 records with a non-finite position, radius or velocity, 1 live records (radius above 0), 2 the fastest speed, 3 mass, 4-6 momentum and 7 kinetic energy, each particle weighing particle_mass, and when capped is wired, 8 and 9 its two words per record summed: the solver's speed-capped move stages and refused solid push-outs, and 10-16 the solver words after the records: pressure and density solve iterations, the solves that reached their cap without converging, the steps whose sealed-pocket spread reached its cap unfinished, 14-15 the volume rate taken off sealed pockets' pressure and density right-hand sides, and 16 the share of the lattice's 8³ tiles the cell passes ran over. Sums run in a fixed order with no atomics, so the words are the same on every run.",
    inputs: {
        particles: Array(FluidParticle) required,
        stats: Array(u32) required,
        capped: Array(u32) optional,
        identity: Array(u32) required,
        particle_mass: ScalarF32 optional,
    },
    outputs: {
        stats_out: Array(u32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("particle_mass"), label: "Particle Mass (kg)", ty: ParamType::Float, default: ParamValue::Float(0.030_517_578), range: Some((0.0, 1.0e6)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The last atom of a particle liquid's tick, inside node.liquid_state's region: particles from the tick's last step, stats and stats_out aliased with node.liquid_state's stats result, which escapes the region to node.liquid_frame (it never publishes a non-finite tick) and to liquid_state's own readback (live count, fault). identity is the step's identity_out, whose record counts the slots in use; particle_mass comes from the domain.",
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
        args: Option<GpuBuffer> = None,
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
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "stats_out")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "stats")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("stats", "stats_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let particle_mass = ctx.scalar_or_param("particle_mass", 0.030_517_578);
        let particles = ctx.inputs.array("particles");
        let stats = ctx.inputs.array("stats");
        let identity = ctx.inputs.array("identity");
        let capped = ctx.inputs.array("capped");
        // In place on the stats input.
        ctx.mark_gpu_accessed();
        let (Some(particles), Some(stats), Some(identity)) = (particles, stats, identity) else {
            return;
        };
        if stats.size < u64::from(LIQUID_STATS_WORDS) * 4 {
            ctx.error(format!(
                "Liquid Stats: the stats array holds fewer than {LIQUID_STATS_WORDS} words"
            ));
            return;
        }
        if identity.size < IDENTITY_BYTES {
            ctx.error(format!(
                "Liquid Stats: the identity holds fewer than {} words",
                IDENTITY_BYTES / 4
            ));
            return;
        }
        let partial_bytes = partial_bytes(slot_limit(particles, capped));
        let gpu = ctx.gpu_encoder();
        let pipelines = self
            .pipelines
            .as_ref()
            .expect("liquid stats pipelines built by prepare_pipelines at install");
        if self
            .partials
            .as_ref()
            .is_none_or(|b| b.size < partial_bytes)
        {
            self.partials = Some(gpu.device.create_buffer(partial_bytes));
        }
        let args = self
            .args
            .get_or_insert_with(|| gpu.device.create_buffer(ARGS_BYTES));
        pipelines.encode(
            gpu.native_enc,
            &StatsJob {
                particles,
                identity,
                stats,
                capped,
                partials: self.partials.as_ref().expect("partials prepared"),
                args,
                particle_mass,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_stats_params_match_the_shader() {
        let shader = with_stats_layout(SHADER_SOURCE);
        assert_eq!(std::mem::size_of::<StatsParams>(), 32);
        assert_eq!(std::mem::size_of::<FluidParticle>(), 32);
        let module = naga::front::wgsl::parse_str(&shader).expect("liquid_stats.wgsl parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("liquid_stats.wgsl validates");
    }

    #[test]
    fn liquid_stats_words_decode() {
        let mut words = [0u32; LIQUID_STATS_WORDS as usize];
        words[..17].copy_from_slice(&[
            2,
            5,
            1.5f32.to_bits(),
            0.25f32.to_bits(),
            1.0f32.to_bits(),
            (-2.0f32).to_bits(),
            0.0f32.to_bits(),
            3.0f32.to_bits(),
            4,
            1,
            40,
            12,
            1,
            2,
            0.5f32.to_bits(),
            0.125f32.to_bits(),
            0.3125f32.to_bits(),
        ]);
        words[17..27].copy_from_slice(&[
            3,
            4,
            5,
            6,
            7,
            0.75f32.to_bits(),
            3,
            0.5f32.to_bits(),
            9,
            2,
        ]);
        words[INFLOW_BIRTHS_WORD as usize] = 640;
        words[INFLOW_REFUSED_WORD as usize] = 64;
        words[SHEET_REFUSED_WORD as usize] = 7;
        words[TILE_REMOVED_WORD as usize] = 3;
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
                inflow_births: 640,
                inflow_refused: 64,
                sheet_refused: 7,
                tile_removed: 3,
            }
        );
        assert_eq!(words.len(), LIQUID_STATS_WORDS as usize);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
