//! `node.surface_tiles` — the display's world tiles. Every drawn water field
//! (level set, velocity, mist, wet map, solid) lives on the table this node
//! builds each frame from the displayed liquid particles, at the display cell
//! size. A tile is held while this frame's particles or grid interior touch
//! it, and let go once they leave, so it costs the surface passes only while
//! water is on it. Foam, spray and bubbles hold no tiles: they draw as
//! particles, and mist only where the liquid holds tiles.
//! The pool follows the water: it starts at [`START_POOL_TILES`], grows
//! ahead of the tiles the table holds, read a frame or two late, and shrinks
//! once they stay under a third of it for [`SHRINK_HOLD`]. With GPU
//! FLIP Narrow Band on, the deep water has no particles: the step's grid
//! interior holds the tiles it fills, so `node.interior_fill` has tiles to
//! draw it on.

use manifold_gpu::BufferUse;
use std::borrow::Cow;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, TILE, lattice_origin};
use manifold_water_liquid::primitives::prefix_scan::open_gate;
use manifold_water_liquid::tile_pool::{grown_pool, shrunk_pool};
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

/// The first pool, in tiles; it grows by [`grown_pool`] and shrinks by
/// [`shrunk_pool`], never below this.
pub(crate) const START_POOL_TILES: u32 = 1024;

/// How long the held tiles stay under a third of the pool before it shrinks,
/// so water that sloshes back does not rebuild the table every frame.
const SHRINK_HOLD: Duration = Duration::from_secs(2);

/// Field order identical to the shader's `Params`; three 16-byte rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct TileUniforms {
    origin: [f32; 3],
    inv_cell: f32,
    pool_tiles: u32,
    cell: f32,
    sim_pool_tiles: u32,
    sim_cell: f32,
    interior_at: u32,
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
    pub pool_tiles: u32,
    /// Records for the water's grid interior: one an octant of each of its
    /// slots (8 · its pool), 0 without one.
    pub interior: u32,
}

impl Layout {
    /// The records the build bins: the frame's particles, then the interior's.
    fn records(self) -> u64 {
        u64::from(self.particles) + u64::from(self.interior)
    }

    /// The interior's first record: the frame's particles come before it.
    fn head(self) -> u32 {
        self.particles
    }

    /// Bytes of the node's own buffers (the world tiles hold theirs).
    pub(crate) fn bytes(self) -> u64 {
        let padded = |bytes: u64| bytes.max(16);
        padded(self.records() * PARTICLE) // points
            + padded(16) // record
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

/// The table and the buffers round the build.
pub(crate) struct Held {
    layout: Layout,
    tiles: WorldTiles,
    /// The records the build bins: the particles copied in, then the
    /// interior's records.
    points: GpuBuffer,
    /// `[groups, 1, 1, n]` over `points`: the build's point passes.
    record: GpuBuffer,
    /// The held tiles' lowest and highest tile coordinates, six i32.
    bounds: GpuBuffer,
    /// min xyz, 0, max xyz, 0, metres.
    extent: GpuBuffer,
    /// A clock plan that is always active.
    gate: GpuBuffer,
}

/// One frame's inputs.
struct Frame<'a> {
    particles: &'a GpuBuffer,
    /// The step's grid interior and its table, while Narrow Band runs.
    interior: Option<(&'a GpuBuffer, &'a GpuBuffer)>,
    /// The water's cell size.
    sim_cell: f32,
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
    let buffer = device.try_create_buffer(bytes.max(16), BufferUse::Upload)?;
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
        Ok(Self {
            layout,
            tiles,
            points: shared(device, layout.records() * PARTICLE)?,
            record,
            bounds: shared(device, 24)?,
            extent: shared(device, 32)?,
            gate: open_gate(device),
        })
    }

    /// The pool this table's last finished build asks for, `None` while it
    /// fits.
    fn grown(&self) -> Option<u32> {
        let (held, short) = self.tiles.late_held()?;
        grown_pool(self.layout.pool_tiles, held, short)
    }

    /// The pool this table's last finished build could shrink to, `None`
    /// while the held tiles fill a third of it or more.
    fn shrunk(&self) -> Option<u32> {
        let (held, short) = self.tiles.late_held()?;
        shrunk_pool(self.layout.pool_tiles, held, START_POOL_TILES).filter(|_| !short)
    }

    fn encode(&mut self, enc: &mut GpuEncoder, kernels: &Kernels, frame: &Frame<'_>) {
        let layout = self.layout;
        let pool = layout.pool_tiles;
        let head = layout.head();
        let uniforms = TileUniforms {
            origin: lattice_origin(frame.cell_size),
            inv_cell: 1.0 / frame.cell_size,
            pool_tiles: pool,
            cell: frame.cell_size,
            sim_pool_tiles: layout.interior / 8,
            sim_cell: frame.sim_cell,
            interior_at: head,
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
            buffer(5, &self.bounds),
            buffer(6, &self.extent),
        ];
        let pass = |enc: &mut GpuEncoder, pipeline: &GpuComputePipeline, groups, label| {
            enc.dispatch_compute(pipeline, &bindings, groups, label);
            enc.compute_memory_barrier_buffers();
        };
        let per_slot = [pool.div_ceil(THREADS), 1, 1];
        if layout.particles > 0 {
            let bytes = u64::from(layout.particles) * PARTICLE;
            enc.copy_buffer_range(frame.particles, 0, &self.points, 0, bytes);
        }
        if let Some((interior, interior_tiles)) = frame.interior.filter(|_| layout.interior > 0) {
            let mut bound = [bindings[0]; 7];
            bound[..5].copy_from_slice(&bindings);
            bound[5] = buffer(7, interior);
            bound[6] = buffer(8, interior_tiles);
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
        pass(
            enc,
            &kernels.clear_bounds,
            [1, 1, 1],
            "node.surface_tiles.clear_bounds",
        );
        pass(enc, &kernels.bounds, per_slot, "node.surface_tiles.bounds");
        pass(enc, &kernels.extent, [1, 1, 1], "node.surface_tiles.extent");
    }
}

/// The passes round the build, one pipeline each.
pub(crate) struct Kernels {
    clear_bounds: GpuComputePipeline,
    bounds: GpuComputePipeline,
    extent: GpuComputePipeline,
    interior: GpuComputePipeline,
}

impl Kernels {
    fn new(device: &GpuDevice) -> Self {
        let make = |entry: &str| {
            device.create_compute_pipeline(SOURCE.as_str(), entry, "node.surface_tiles")
        };
        Self {
            clear_bounds: make("st_clear_bounds"),
            bounds: make("st_bounds"),
            extent: make("st_extent"),
            interior: make("st_interior"),
        }
    }
}

manifold_node_engine::primitive! {
    name: SurfaceTiles,
    type_id: "node.surface_tiles",
    purpose: "The display's world tiles: the table every drawn water field is stored through, built each frame from the displayed liquid particles at the display cell size (sim_cell_size over resolution_scale). A particle's tile and the ring of tiles round it join the table. A tile is held, in the same slot, while this frame's particles or grid interior touch it and let go once they leave, so a tile costs the surface passes only while water is on it. The pool starts at 1024 tiles and follows the water: once the held tiles fill two thirds of it, or a tile finds no free slot, it grows to twice the tiles held in grains of 256, read a frame or two late, and the table starts over at the new size. Once the held tiles stay under a third of it for two seconds it shrinks to twice them, never below 1024. tiles is the table, pool_tiles and cell_size say how to read it, and extent is the held tiles' node bounds in metres (min xyz, 0, max xyz, 0; min above max with none).",
    inputs: {
        particles: Array(FluidParticle) required,
        interior: Array(f32) optional,
        interior_tiles: Array(u32) optional,
        sim_cell_size: ScalarF32 optional,
    },
    outputs: {
        tiles: Array(u32),
        pool_tiles: ScalarF32,
        cell_size: ScalarF32,
        extent: Array(f32),
    },
    params: [
        int_param!("resolution_scale", "Resolution Scale", 1.0, 1.0, 4.0),
        float_param!("sim_cell_size", "Water Cell Size", 0.0625, 0.0001, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire particles from the liquid frame's particles, interior and interior_tiles from node.liquid_state's (GPU FLIP Narrow Band's grid water: every water tile with a grid cell under water joins the table) and sim_cell_size from the water step. Every display atom takes tiles, pool_tiles and cell_size from here; extent feeds the water object's field_extent.",
    examples: [],
    picker: { label: "Surface Tiles", category: Atom },
    summary: "Builds the tiles the drawn water lives in, following the displayed water and letting go of tiles it has left.",
    category: Particles3D,
    role: Filter,
    aliases: ["surface tiles", "display tiles", "world tiles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        held: Option<Held> = None,
        kernels: Option<Kernels> = None,
        // When the held tiles last dropped under a third of the pool.
        shrink_since: Option<Instant> = None,
    },
}

impl SurfaceTiles {
    /// The pool for this frame, from what the last builds held: grown at
    /// once, shrunk once the tiles have stayed under a third of it for
    /// [`SHRINK_HOLD`].
    fn next_pool(&mut self) -> u32 {
        let Some(held) = self.held.as_ref() else {
            return START_POOL_TILES;
        };
        let pool = held.layout.pool_tiles;
        if let Some(grown) = held.grown() {
            self.shrink_since = None;
            return grown;
        }
        let Some(shrunk) = held.shrunk() else {
            self.shrink_since = None;
            return pool;
        };
        let since = *self.shrink_since.get_or_insert_with(Instant::now);
        if since.elapsed() < SHRINK_HOLD {
            return pool;
        }
        self.shrink_since = None;
        shrunk
    }
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
        let sim_cell = ctx.scalar_or_param("sim_cell_size", 0.0625);
        let cell_size = match display_cell_size(sim_cell, ctx.param_f32("resolution_scale", 1.0)) {
            Ok(cell_size) => cell_size,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let interior = ctx
            .inputs
            .array("interior")
            .zip(ctx.inputs.array("interior_tiles"));
        let interior_pool = interior
            .and_then(|(interior, _)| super::interior_fill::interior_pool(interior.size))
            .unwrap_or(0);
        // A count past a u32 saturates, and the layout refuses it by name.
        let slots = u32::try_from(particles.size / PARTICLE).unwrap_or(u32::MAX);
        let pool_tiles = self.next_pool();
        let layout = Layout {
            particles: slots,
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
            interior,
            sim_cell,
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
        for entry in ["st_clear_bounds", "st_bounds", "st_extent", "st_interior"] {
            assert!(
                module.entry_points.iter().any(|point| point.name == entry),
                "missing {entry}"
            );
        }
        assert_eq!(std::mem::size_of::<TileUniforms>(), 48);
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
            particles: 1200,
            pool_tiles: 40,
            interior: 24,
        };
        assert_eq!(layout.head(), 1200);
        assert_eq!(layout.records(), 1224);
        assert_eq!(layout.bytes(), 1224 * 32 + 16 + 24 + 32 + 48);
    }
}
