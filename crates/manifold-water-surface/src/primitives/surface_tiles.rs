//! `node.surface_tiles` — the display's world tiles. Every drawn water field
//! (level set, velocity, foam, mist, wetness, solid) lives on the table this
//! node builds each frame from the displayed particles, foam and spray, at the
//! display cell size. A tile the liquid touched stays held, in its slot, for
//! `keep_seconds` after the water leaves, so the wetness stored in it can dry;
//! foam and spray hold their tiles only while they are there.
//! The pool follows the water: it starts at [`START_POOL_TILES`] and grows
//! ahead of the tiles the table holds, read a frame or two late. With GPU
//! FLIP Narrow Band on, the deep water has no particles: the step's grid
//! interior holds the tiles it fills, so `node.interior_fill` has tiles to
//! draw it on.

use std::borrow::Cow;
use std::sync::LazyLock;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{
    READ_SIDE, TILE, lattice_origin, slot_row_word, table_entries,
};
use manifold_water_liquid::primitives::prefix_scan::open_gate;
use manifold_water_liquid::tile_pool::grown_pool;
use manifold_water_liquid::world_tiles::{
    Allocation, LIQUID_POINTS, PointCount, Reach, WorldTiles,
};
use manifold_water_liquid::{float_param, int_param};

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

// The read side, the water's (the same text renamed sim_ts_ / SIM_WT_, for
// the interior's table), then the passes around the build.
static SOURCE: LazyLock<String> = LazyLock::new(|| {
    let sim = READ_SIDE
        .replace("ts_", "sim_ts_")
        .replace("WT_", "SIM_WT_");
    format!(
        "{READ_SIDE}\n{sim}\n{}",
        include_str!("shaders/surface_tiles.wgsl")
    )
});
const THREADS: u32 = 256;
const PARTICLE: u64 = std::mem::size_of::<FluidParticle>() as u64;

/// A particle's tile and the ring of tiles round it join the table; every
/// ring tile gets a slot, so a field read one cell past a tile never leaves
/// the table.
pub(crate) const REACH: Reach = Reach {
    ring_max: 1,
    alloc_ring: 1,
    cell_reach: TILE,
};

/// The most the display subdivides a simulation cell per axis.
pub(crate) const MAX_RESOLUTION_SCALE: f32 = 4.0;

/// The first pool, in tiles; it grows by [`grown_pool`].
pub(crate) const START_POOL_TILES: u32 = 1024;

/// Field order identical to the shader's `Params`; four 16-byte rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct TileUniforms {
    origin: [f32; 3],
    inv_cell: f32,
    pool_tiles: u32,
    head: u32,
    time: f32,
    keep_seconds: f32,
    cell: f32,
    sim_pool_tiles: u32,
    sim_cell: f32,
    interior_at: u32,
    liquid: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

/// The display cell size for a simulation of `sim_cell_size` metres:
/// `resolution_scale` display cells per simulation cell along an axis.
pub(crate) fn display_cell_size(sim_cell_size: f32, resolution_scale: f32) -> Result<f32, String> {
    if !(sim_cell_size.is_finite() && sim_cell_size > 0.0) {
        return Err(format!(
            "Surface Tiles: the water's cell size {sim_cell_size} is not a length"
        ));
    }
    if !resolution_scale.is_finite() {
        return Err(format!(
            "Surface Tiles: a resolution scale of {resolution_scale} is not a number"
        ));
    }
    let scale = resolution_scale.round().clamp(1.0, MAX_RESOLUTION_SCALE);
    Ok(sim_cell_size / scale)
}

/// What the node's buffers are sized by; any change builds them again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub particles: u32,
    pub foam: u32,
    pub spray: u32,
    pub pool_tiles: u32,
    /// Records for the water's grid interior: one an octant of each of its
    /// slots (8 · its pool), 0 without one.
    pub interior: u32,
}

impl Layout {
    /// The records the build bins: the frame's points, a keep record a slot,
    /// then the interior's.
    fn records(self) -> u64 {
        u64::from(self.particles)
            + u64::from(self.foam)
            + u64::from(self.spray)
            + u64::from(self.pool_tiles)
            + u64::from(self.interior)
    }

    /// Records before the keep records.
    fn head(self) -> u32 {
        self.particles + self.foam + self.spray
    }

    /// Bytes of the node's own buffers (the world tiles hold theirs).
    pub(crate) fn bytes(self) -> u64 {
        let pool = u64::from(self.pool_tiles);
        let padded = |bytes: u64| bytes.max(16);
        padded(self.records() * PARTICLE) // points
            + padded(16) // record
            + padded(4 * pool) // touched
            + padded(16 * pool) // last rows
            + padded(24) // bounds
            + padded(32) // extent
            + padded(48) // gate
    }

    /// Records past what a u32 indexes are refused by name.
    fn checked(self) -> Result<Self, String> {
        if self.records() > u64::from(u32::MAX) {
            return Err(format!(
                "{} display points are more than a pass indexes",
                self.records()
            ));
        }
        Ok(self)
    }
}

/// The table, the buffers round the build and the last frame it saw.
pub(crate) struct Held {
    layout: Layout,
    tiles: WorldTiles,
    /// The records the build bins: particles, foam and spray copied end to
    /// end, one keep record a slot, then the interior's records.
    points: GpuBuffer,
    /// `[groups, 1, 1, n]` over `points`: the build's point passes.
    record: GpuBuffer,
    /// Per slot, the display time water last touched its tile (f32 bits).
    touched: GpuBuffer,
    /// Last frame's slot rows of the table.
    last_rows: GpuBuffer,
    /// The held tiles' lowest and highest tile coordinates, six i32.
    bounds: GpuBuffer,
    /// min xyz, 0, max xyz, 0, metres.
    extent: GpuBuffer,
    /// A clock plan that is always active.
    gate: GpuBuffer,
    last: Option<LastRun>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LastRun {
    time: f32,
    epoch: Option<f32>,
    cell_size: f32,
}

/// One frame's inputs.
struct Frame<'a> {
    particles: &'a GpuBuffer,
    foam: Option<&'a GpuBuffer>,
    spray: Option<&'a GpuBuffer>,
    /// The step's grid interior and its table, while Narrow Band runs.
    interior: Option<(&'a GpuBuffer, &'a GpuBuffer)>,
    /// The water's cell size.
    sim_cell: f32,
    time: f32,
    epoch: Option<f32>,
    keep_seconds: f32,
    cell_size: f32,
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

fn shared(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    let buffer = device.try_create_buffer_shared(bytes.max(16))?;
    buffer.zero_fill();
    Ok(buffer)
}

impl Held {
    fn new(device: &GpuDevice, layout: Layout) -> Result<Self, String> {
        let layout = layout.checked()?;
        manifold_node_engine::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            layout.bytes(),
        )
        .map_err(|error| error.to_string())?;
        let pool = layout.pool_tiles;
        let mut tiles = WorldTiles::new(device, pool, REACH, LIQUID_POINTS)?;
        tiles.prepare(device);
        let records = layout.records() as u32;
        let record = shared(device, 16)?;
        // SAFETY: a new shared buffer of 16 bytes; no GPU work has touched it.
        unsafe {
            record.write(
                0,
                bytemuck::cast_slice(&[records.div_ceil(THREADS), 1, 1, records]),
            );
        }
        let pool_bytes = u64::from(pool);
        Ok(Self {
            layout,
            tiles,
            points: shared(device, layout.records() * PARTICLE)?,
            record,
            touched: shared(device, 4 * pool_bytes)?,
            last_rows: shared(device, 16 * pool_bytes)?,
            bounds: shared(device, 24)?,
            extent: shared(device, 32)?,
            gate: open_gate(device),
            last: None,
        })
    }

    /// Water starts over on a new epoch, a new cell size, and time running back
    /// past the keep window: nothing touched before carries.
    fn restarts(&self, frame: &Frame<'_>) -> bool {
        match self.last {
            None => true,
            Some(last) => {
                last.epoch != frame.epoch
                    || last.cell_size != frame.cell_size
                    || frame.time < last.time - frame.keep_seconds
            }
        }
    }

    /// The pool this table's last finished build asks for, `None` while it
    /// fits.
    fn grown(&self) -> Option<u32> {
        let (held, short) = self.tiles.late_held()?;
        grown_pool(self.layout.pool_tiles, held, short)
    }

    fn encode(&mut self, enc: &mut GpuEncoder, kernels: &Kernels, frame: &Frame<'_>) {
        let layout = self.layout;
        let pool = layout.pool_tiles;
        let head = layout.head();
        let reset = self.restarts(frame);
        let uniforms = TileUniforms {
            origin: lattice_origin(frame.cell_size),
            inv_cell: 1.0 / frame.cell_size,
            pool_tiles: pool,
            head,
            time: frame.time,
            keep_seconds: frame.keep_seconds,
            cell: frame.cell_size,
            sim_pool_tiles: layout.interior / 8,
            sim_cell: frame.sim_cell,
            interior_at: head + pool,
            liquid: layout.particles,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            buffer(1, self.tiles.table()),
            buffer(2, &self.points),
            buffer(3, &self.touched),
            buffer(4, &self.last_rows),
            buffer(5, &self.bounds),
            buffer(6, &self.extent),
        ];
        let pass = |enc: &mut GpuEncoder, pipeline: &GpuComputePipeline, groups, label| {
            enc.dispatch_compute(pipeline, &bindings, groups, label);
            enc.compute_memory_barrier_buffers();
        };
        let per_slot = [pool.div_ceil(THREADS), 1, 1];
        if reset {
            pass(enc, &kernels.reset, per_slot, "node.surface_tiles.reset");
        }
        let mut at = 0;
        for (source, slots) in [
            (Some(frame.particles), layout.particles),
            (frame.foam, layout.foam),
            (frame.spray, layout.spray),
        ] {
            let bytes = u64::from(slots) * PARTICLE;
            if let Some(source) = source.filter(|_| slots > 0) {
                enc.copy_buffer_range(source, 0, &self.points, at, bytes);
            }
            at += bytes;
        }
        if let Some((interior, interior_tiles)) = frame.interior.filter(|_| layout.interior > 0) {
            let mut bound = [bindings[0]; 9];
            bound[..7].copy_from_slice(&bindings);
            bound[7] = buffer(7, interior);
            bound[8] = buffer(8, interior_tiles);
            enc.dispatch_compute(
                &kernels.interior,
                &bound,
                [layout.interior.div_ceil(THREADS), 1, 1],
                "node.surface_tiles.interior",
            );
            enc.compute_memory_barrier_buffers();
        }
        self.tiles.encode_allocation(
            enc,
            &Allocation {
                points: &self.points,
                count: PointCount::Record(&self.record),
                origin: uniforms.origin,
                inv_cell: uniforms.inv_cell,
                replay_counts: [0; 2],
                gate: &self.gate,
                clip: None,
                kept: None,
            },
        );
        pass(enc, &kernels.carry, per_slot, "node.surface_tiles.carry");
        if layout.particles > 0 {
            pass(
                enc,
                &kernels.touch,
                [layout.particles.div_ceil(THREADS), 1, 1],
                "node.surface_tiles.touch",
            );
        }
        pass(enc, &kernels.keep, per_slot, "node.surface_tiles.keep");
        pass(enc, &kernels.extent, [1, 1, 1], "node.surface_tiles.extent");
        // The next frame's carry compares its slots' tiles with these.
        enc.copy_buffer_range(
            self.tiles.table(),
            slot_row_word(table_entries(pool), 0) * 4,
            &self.last_rows,
            0,
            16 * u64::from(pool),
        );
        self.last = Some(LastRun {
            time: frame.time,
            epoch: frame.epoch,
            cell_size: frame.cell_size,
        });
    }
}

/// The passes round the build, one pipeline each.
pub(crate) struct Kernels {
    reset: GpuComputePipeline,
    carry: GpuComputePipeline,
    touch: GpuComputePipeline,
    keep: GpuComputePipeline,
    extent: GpuComputePipeline,
    interior: GpuComputePipeline,
}

impl Kernels {
    fn new(device: &GpuDevice) -> Self {
        let make = |entry: &str| {
            device.create_compute_pipeline(SOURCE.as_str(), entry, "node.surface_tiles")
        };
        Self {
            reset: make("st_reset"),
            carry: make("st_carry"),
            touch: make("st_touch"),
            keep: make("st_keep"),
            extent: make("st_extent"),
            interior: make("st_interior"),
        }
    }
}

manifold_node_engine::primitive! {
    name: SurfaceTiles,
    type_id: "node.surface_tiles",
    purpose: "The display's world tiles: the table every drawn water field is stored through, built each frame from the displayed particles, foam and spray at the display cell size (sim_cell_size over resolution_scale). A particle's tile and the ring of tiles round it join the table. A tile the liquid particles touched stays held, in the same slot, for keep_seconds after the water leaves (a slot that changes tiles or was free forgets), so the wetness stored in it can dry; foam and spray hold their tiles only while they are there; an epoch change, a new cell size, or display time running back past keep_seconds drops what was kept. The pool starts at 1024 tiles and follows the water: once the held tiles fill two thirds of it, or a tile finds no free slot, it grows to twice the tiles held in grains of 256, read a frame or two late, and the table starts over at the new size. It only grows. tiles is the table, pool_tiles and cell_size say how to read it, and extent is the held tiles' node bounds in metres (min xyz, 0, max xyz, 0; min above max with none).",
    inputs: {
        particles: Array(FluidParticle) required,
        foam: Array(FluidParticle) optional,
        spray: Array(FluidParticle) optional,
        interior: Array(f32) optional,
        interior_tiles: Array(u32) optional,
        sim_cell_size: ScalarF32 optional,
        time: ScalarF32 required,
        epoch: ScalarF32 optional,
    },
    outputs: {
        tiles: Array(u32),
        pool_tiles: ScalarF32,
        cell_size: ScalarF32,
        extent: Array(f32),
    },
    params: [
        int_param!("resolution_scale", "Resolution Scale", 1.0, 1.0, 4.0),
        // How long a tile the liquid touched stays held, so wetness can dry.
        float_param!("keep_seconds", "Keep Seconds", 1800.0, 0.0, 86400.0),
        float_param!("sim_cell_size", "Water Cell Size", 0.0625, 0.0001, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire particles from the liquid frame's blended particles, foam and spray from the displayed whitewater, interior and interior_tiles from node.liquid_state's (GPU FLIP Narrow Band's grid water: every water tile with a grid cell under water joins the table), sim_cell_size from the water step, time from the domain's display_time and epoch from its epoch. Every display atom takes tiles, pool_tiles and cell_size from here; extent feeds the water object's field_extent. Keep it running while the water is shown: the tiles it keeps are the wetness's memory.",
    examples: [],
    picker: { label: "Surface Tiles", category: Atom },
    summary: "Builds the tiles the drawn water lives in, following the displayed water and keeping wet ground for a while.",
    category: Particles3D,
    role: Filter,
    aliases: ["surface tiles", "display tiles", "world tiles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        held: Option<Held> = None,
        kernels: Option<Kernels> = None,
    },
}

impl Primitive for SurfaceTiles {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.kernels.get_or_insert_with(|| Kernels::new(device));
        // A one-slot table: the build's pipelines are cached by source, so every
        // pool compiles here, not on the first frame.
        match WorldTiles::new(device, 1, REACH, LIQUID_POINTS) {
            Ok(mut tiles) => tiles.prepare(device),
            Err(error) => log::warn!("Surface Tiles: world tiles at install: {error}"),
        }
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "tiles" | "extent")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let held = self.held.as_ref()?;
        match port {
            "tiles" => Some(held.tiles.table()),
            "extent" => Some(&held.extent),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, sized to the pool at run time.
        matches!(port, "tiles" | "extent").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(particles) = ctx.inputs.array("particles") else {
            ctx.error("Surface Tiles: particles must be wired");
            return;
        };
        let Some(ParamValue::Float(time)) = ctx.inputs.scalar("time") else {
            ctx.error("Surface Tiles: time must be wired");
            return;
        };
        if !time.is_finite() {
            ctx.error("Surface Tiles: time must be finite");
            return;
        }
        let epoch = match ctx.inputs.scalar("epoch") {
            Some(ParamValue::Float(epoch)) => Some(epoch),
            _ => None,
        };
        let keep_seconds = ctx.param_f32("keep_seconds", 1800.0);
        if !(keep_seconds.is_finite() && keep_seconds >= 0.0) {
            ctx.error("Surface Tiles: keep seconds must be finite and at least 0");
            return;
        }
        let sim_cell = ctx.scalar_or_param("sim_cell_size", 0.0625);
        let cell_size = match display_cell_size(sim_cell, ctx.param_f32("resolution_scale", 1.0)) {
            Ok(cell_size) => cell_size,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let foam = ctx.inputs.array("foam");
        let spray = ctx.inputs.array("spray");
        let interior = ctx
            .inputs
            .array("interior")
            .zip(ctx.inputs.array("interior_tiles"));
        let interior_pool = interior
            .and_then(|(interior, _)| super::interior_fill::interior_pool(interior.size))
            .unwrap_or(0);
        // A count past a u32 saturates, and the layout refuses it by name.
        let slots = |buffer: Option<&GpuBuffer>| {
            u32::try_from(buffer.map_or(0, |buffer| buffer.size / PARTICLE)).unwrap_or(u32::MAX)
        };
        // The pool only grows, from what the last builds held.
        let pool_tiles = self.held.as_ref().map_or(START_POOL_TILES, |held| {
            held.grown().unwrap_or(held.layout.pool_tiles)
        });
        let layout = Layout {
            particles: slots(Some(particles)),
            foam: slots(foam),
            spray: slots(spray),
            pool_tiles,
            interior: interior_pool.saturating_mul(8),
        };
        let prepared = {
            let gpu = ctx.gpu_encoder();
            self.kernels.get_or_insert_with(|| Kernels::new(gpu.device));
            if self.held.as_ref().is_none_or(|held| held.layout != layout) {
                self.held = None;
                Held::new(gpu.device, layout).map(|held| self.held = Some(held))
            } else {
                Ok(())
            }
        };
        if let Err(error) = prepared {
            ctx.error(format!("Surface Tiles: {error}"));
            return;
        }
        ctx.outputs
            .set_scalar("pool_tiles", ParamValue::Float(layout.pool_tiles as f32));
        ctx.outputs
            .set_scalar("cell_size", ParamValue::Float(cell_size));
        let frame = Frame {
            particles,
            foam,
            spray,
            interior,
            sim_cell,
            time,
            epoch,
            keep_seconds,
            cell_size,
        };
        let gpu = ctx.gpu_encoder();
        let (Some(held), Some(kernels)) = (self.held.as_mut(), self.kernels.as_ref()) else {
            return;
        };
        held.encode(gpu.native_enc, kernels, &frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SOURCE).expect("surface tiles shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("surface tiles shader validates");
        for entry in [
            "st_reset",
            "st_carry",
            "st_touch",
            "st_keep",
            "st_extent",
            "st_interior",
        ] {
            assert!(
                module.entry_points.iter().any(|point| point.name == entry),
                "missing {entry}"
            );
        }
        assert_eq!(std::mem::size_of::<TileUniforms>(), 64);
    }

    #[test]
    fn the_display_cell_follows_the_resolution_scale() {
        assert_eq!(display_cell_size(0.25, 1.0).unwrap(), 0.25);
        assert_eq!(display_cell_size(0.25, 2.0).unwrap(), 0.125);
        // The resolution scale rounds and clamps to what the node offers.
        assert_eq!(display_cell_size(0.25, 2.6).unwrap(), 0.25 / 3.0);
        assert_eq!(display_cell_size(0.25, 9.0).unwrap(), 0.0625);
        assert_eq!(display_cell_size(0.25, 0.2).unwrap(), 0.25);
        assert!(display_cell_size(0.0, 1.0).is_err());
        assert!(display_cell_size(f32::NAN, 1.0).is_err());
        assert!(display_cell_size(0.25, f32::NAN).is_err());
    }

    #[test]
    fn the_layout_counts_every_buffer() {
        let layout = Layout {
            particles: 1000,
            foam: 200,
            spray: 0,
            pool_tiles: 40,
            interior: 24,
        };
        assert_eq!(layout.head(), 1200);
        assert_eq!(layout.records(), 1264);
        assert_eq!(
            layout.bytes(),
            1264 * 32 + 16 + 4 * 40 + 16 * 40 + 24 + 32 + 48
        );
    }
}
