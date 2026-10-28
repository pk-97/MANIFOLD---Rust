//! `node.wet_map` — soft wet marks (decals) where the water touched a
//! surface, kept between frames (the layout is
//! `manifold_node_engine::scene::wet_map`). Each frame every liquid particle
//! near a solid wets the decal of the merge cell and facing it touches, or
//! starts one, so a wetted surface holds one decal a cell; the scene's wet
//! pass draws the decals and dries them by the age of their last touch, so
//! the marks cost one pass over the particles a frame, a compaction a second
//! and nothing while the water is gone.

use manifold_gpu::BufferUse;
use std::borrow::Cow;
use std::sync::LazyLock;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::wet_map::{CAPACITY, DRY_SECONDS, HASH_SLOTS, layout, map_words};
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;

// The read side and the map's layout, then the kernels.
static SHADER: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{}\n{}",
        layout(),
        include_str!("shaders/wet_map.wgsl")
    )
});
const THREADS: u32 = 64;

/// A decal's radius in water cells: wide enough that the decals a spread
/// leaves a cell apart overlap into one film.
const DECAL_RADIUS_CELLS: f32 = 1.5;

/// The side of a merge cell in water cells: the contacts in one cell on one
/// facing wet one decal.
const MERGE_CELLS: f32 = 1.0;

/// Frames between compactions, which drop the decals that have dried.
const COMPACT_FRAMES: u32 = 60;

/// A compaction keeps a decal until it has dried this many times over (to
/// e^-5, below what a pixel shows).
const KEEP_DRY_TIMES: f32 = 5.0;

/// How long a compaction keeps a decal after its last touch at `drying`; a
/// surface that never dries keeps its decals.
fn keep_seconds(drying: f32) -> f32 {
    if drying > 0.0 {
        KEEP_DRY_TIMES * DRY_SECONDS / drying
    } else {
        f32::MAX
    }
}

/// The water a contact adds to a decal a second of display time: a quarter of
/// full (`WD_FULL`, 256) a 60th of a second.
const HIT_PER_SECOND: f32 = 64.0 * 60.0;

/// The water a contact adds over `elapsed` seconds of display time, at most
/// full: the same per second at any frame rate, nothing while time stands
/// still.
fn contact_hit(elapsed: f32) -> u32 {
    (HIT_PER_SECOND * elapsed).round().clamp(0.0, 256.0) as u32
}

/// Field order identical to the shader's `Params`; three 16-byte rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct WetMapUniforms {
    now: f32,
    drying: f32,
    radius: f32,
    merge: f32,
    capacity: u32,
    pool_tiles: u32,
    cell: f32,
    keep: f32,
    hit: u32,
    pad: [u32; 3],
}

/// What the last run saw: the decals carry on from it only at the same epoch
/// and water cell, with time standing still or moving on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LastRun {
    time: f32,
    epoch: Option<f32>,
    sim_cell: f32,
}

fn restarts(last: Option<LastRun>, now: LastRun) -> bool {
    last.is_none_or(|last| {
        last.epoch != now.epoch || last.sim_cell != now.sim_cell || now.time < last.time
    })
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

manifold_node_engine::primitive! {
    name: WetMap,
    type_id: "node.wet_map",
    purpose: "Soft round wet marks (decals) where the water touched a surface, kept between frames. Each run every liquid particle within two radii of the solid (its trilinear distance on the display tiles) finds the surface point it touches (back along the solid's normal by that distance), that surface's normal and its facing (the axis the normal is nearest, either way). The decal of that facing and that water-cell square across its axis, its plane within a water cell and facing within about 45 degrees, takes the touch (its time, and water for the display time since the last run, a quarter of full a 60th of a second, up to full; none while time stands still), or else a new decal starts halfway between the point and the square's middle. A decal is 1.5 water cells across in radius, with a soft edge, so the decals of a spread's cells overlap into one film; the scene draws them over what it sees and a decal wets only the pixels facing its way, so a floor mark never wets the wall it meets. Its wetness is how much water hit it times exp(-age * drying / 15 s) since its last touch, so it dries without any pass over the decals. Once a second the decals dried to e^-5 are dropped. It holds 131072 decals; past that a new decal overwrites the oldest. It starts over, cleared, on its first run, when epoch changes, when the water's cell size changes and when time goes backwards.",
    inputs: {
        particles: Array(FluidParticle) required,
        solid: Array(f32) required,
        tiles: Array(u32) required,
        time: ScalarF32 required,
        epoch: ScalarF32 optional,
        drying: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        map: Array(u32),
    },
    params: [
        // Times the stage drying rate: 1 dries a splash in about half a minute, 0 never.
        float_param!("drying", "Drying", 1.0, 0.0, 1000.0),
        float_param!("sim_cell_size", "Water Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire particles from the drawn liquid particles, solid from the display solid, tiles, pool_tiles and cell_size from node.surface_tiles, sim_cell_size from the water's cell size, time from the domain's display_time and epoch from its epoch; its map feeds the water object's wet_map. The decals persist between frames, so keep the node running while the water is shown.",
    examples: [],
    picker: { label: "Wet Map", category: Atom },
    summary: "Remembers where the water touched the ground and walls, so they look wet and dry off over time.",
    category: Particles3D,
    role: Filter,
    aliases: ["wet map", "wetness", "wet surfaces", "wet decals"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        kernels: Option<Kernels> = None,
        map: Option<GpuBuffer> = None,
        last: Option<LastRun> = None,
        frames_since_compact: u32 = 0,
    },
}

/// The node's passes, one pipeline each.
struct Kernels {
    begin: GpuComputePipeline,
    clear_hash: GpuComputePipeline,
    compact: GpuComputePipeline,
    finish: GpuComputePipeline,
    mark: GpuComputePipeline,
}

impl Kernels {
    fn new(device: &GpuDevice) -> Self {
        let make =
            |entry: &str| device.create_compute_pipeline(SHADER.as_str(), entry, "node.wet_map");
        Self {
            begin: make("cs_begin"),
            clear_hash: make("cs_clear_hash"),
            compact: make("cs_compact"),
            finish: make("cs_finish"),
            mark: make("cs_mark"),
        }
    }
}

impl WetMap {
    /// The map, made once and zeroed: a zeroed map holds no decal.
    fn ensure_state(&mut self, device: &GpuDevice) -> Result<(), String> {
        if self.map.is_some() {
            return Ok(());
        }
        let bytes = map_words() * 4;
        manifold_node_engine::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            bytes,
        )
        .map_err(|error| error.to_string())?;
        let map = device.try_create_buffer(bytes, BufferUse::Upload)?;
        map.zero_fill();
        self.map = Some(map);
        Ok(())
    }
}

impl Primitive for WetMap {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.kernels.get_or_insert_with(|| Kernels::new(device));
    }

    fn provides_array_output(&self, port: &str) -> bool {
        port == "map"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "map" => self.map.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, made at the first run.
        (port == "map").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Wet Map") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(particles), Some(solid), Some(tiles)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
        ) else {
            ctx.error("Wet Map: particles, solid and tiles must be wired");
            return;
        };
        let Some(ParamValue::Float(time)) = ctx.inputs.scalar("time") else {
            ctx.error("Wet Map: time must be wired");
            return;
        };
        if !time.is_finite() {
            ctx.error("Wet Map: time must be finite");
            return;
        }
        let drying = ctx.scalar_or_param("drying", 1.0);
        if !(drying.is_finite() && drying >= 0.0) {
            ctx.error("Wet Map: drying must be finite and at least 0");
            return;
        }
        let sim_cell = ctx.scalar_or_param("sim_cell_size", 0.0625);
        if !(sim_cell.is_finite() && sim_cell > 0.0) {
            ctx.error(format!(
                "Wet Map: the water's cell size {sim_cell} is not a length"
            ));
            return;
        }
        if solid.size < pool.elements() * 4 {
            ctx.error(format!(
                "Wet Map: solid holds {} values for a pool of {} tiles; it needs {}",
                solid.size / 4,
                pool.pool_tiles,
                pool.elements()
            ));
            return;
        }
        let epoch = match ctx.inputs.scalar("epoch") {
            Some(ParamValue::Float(epoch)) => Some(epoch),
            _ => None,
        };
        let run = LastRun {
            time,
            epoch,
            sim_cell,
        };
        let Ok(capacity) = u32::try_from(particles.size / size_of::<FluidParticle>() as u64) else {
            ctx.error("Wet Map: particles holds more records than a pass indexes");
            return;
        };
        let restart = restarts(self.last, run);
        // A cleared map has no last run: it takes the frame's interval.
        let elapsed = match self.last {
            Some(last) if !restart => time - last.time,
            _ => ctx.time.delta.0 as f32,
        };
        let hit = contact_hit(elapsed);
        let uniforms = WetMapUniforms {
            now: time,
            drying,
            radius: DECAL_RADIUS_CELLS * sim_cell,
            merge: MERGE_CELLS * sim_cell,
            capacity,
            pool_tiles: pool.pool_tiles,
            cell: pool.cell_size,
            keep: keep_seconds(drying),
            hit,
            pad: [0; 3],
        };
        let prepared = {
            let gpu = ctx.gpu_encoder();
            self.kernels.get_or_insert_with(|| Kernels::new(gpu.device));
            self.ensure_state(gpu.device)
        };
        if let Err(error) = prepared {
            ctx.error(format!(
                "Wet Map: the decals need storage the device cannot give: {error}"
            ));
            return;
        }
        let (Some(kernels), Some(map)) = (self.kernels.as_ref(), self.map.as_ref()) else {
            return;
        };
        let compact = !restart && self.frames_since_compact + 1 >= COMPACT_FRAMES;
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            buffer(1, particles),
            buffer(2, solid),
            buffer(3, tiles),
            buffer(4, map),
        ];
        let enc = &mut *ctx.gpu_encoder().native_enc;
        if restart {
            enc.clear_buffer(map);
        }
        enc.dispatch_compute(&kernels.begin, &bindings, [1, 1, 1], "node.wet_map.begin");
        enc.compute_memory_barrier_buffers();
        if compact {
            enc.dispatch_compute(
                &kernels.clear_hash,
                &bindings,
                [(HASH_SLOTS as u32).div_ceil(THREADS), 1, 1],
                "node.wet_map.clear_hash",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &kernels.compact,
                &bindings,
                [CAPACITY.div_ceil(THREADS), 1, 1],
                "node.wet_map.compact",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(&kernels.finish, &bindings, [1, 1, 1], "node.wet_map.finish");
            enc.compute_memory_barrier_buffers();
        }
        if capacity > 0 && hit > 0 {
            enc.dispatch_compute(
                &kernels.mark,
                &bindings,
                [capacity.div_ceil(THREADS), 1, 1],
                "node.wet_map.mark",
            );
        }
        self.frames_since_compact = if compact || restart {
            0
        } else {
            self.frames_since_compact + 1
        };
        self.last = Some(run);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("wet map shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("wet map shader validates");
        for entry in [
            "cs_begin",
            "cs_clear_hash",
            "cs_compact",
            "cs_finish",
            "cs_mark",
        ] {
            assert!(
                module.entry_points.iter().any(|point| point.name == entry),
                "missing {entry}"
            );
        }
        assert_eq!(std::mem::size_of::<WetMapUniforms>(), 48);
    }

    #[test]
    fn a_contact_wets_by_the_time_it_covers() {
        assert_eq!(contact_hit(1.0 / 60.0), 64);
        assert_eq!(contact_hit(1.0 / 144.0), 27);
        // Two frames at 120 fps add what one does at 60.
        assert_eq!(2 * contact_hit(1.0 / 120.0), contact_hit(1.0 / 60.0));
        assert_eq!(contact_hit(1.0 / 24.0), 160);
        assert_eq!(contact_hit(0.0), 0, "time standing still adds nothing");
        assert_eq!(contact_hit(2.0), 256, "never past full");
    }

    #[test]
    fn a_compaction_keeps_a_decal_until_it_has_dried_five_times_over() {
        assert_eq!(keep_seconds(1.0), 75.0);
        assert_eq!(keep_seconds(3.0), 25.0);
        assert_eq!(keep_seconds(0.0), f32::MAX);
    }

    #[test]
    fn the_decals_start_over_on_a_new_epoch_cell_or_time_running_back() {
        let at = |time, epoch, sim_cell| LastRun {
            time,
            epoch,
            sim_cell,
        };
        let last = Some(at(4.0, Some(1.0), 0.04));
        assert!(restarts(None, at(0.0, None, 0.04)));
        assert!(!restarts(last, at(4.0, Some(1.0), 0.04)));
        assert!(!restarts(last, at(9.0, Some(1.0), 0.04)));
        assert!(restarts(last, at(3.9, Some(1.0), 0.04)));
        assert!(restarts(last, at(9.0, Some(2.0), 0.04)));
        assert!(restarts(last, at(9.0, Some(1.0), 0.05)));
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
