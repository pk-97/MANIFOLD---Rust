//! `node.interior_fill` — the display's level set with GPU FLIP Narrow
//! Band's grid water filled in. With the band on, particles live only near
//! the surface, so the level set they draw is a shell; the step's grid
//! interior holds the water under it. Each display node takes the lower of
//! its level set and the interior there, shrunk by [`FILL_DEPTH_CELLS`]: the
//! grid fills only where the band's particles already cover from above, so
//! the drawn surface stays the particles'.

use std::borrow::Cow;
use std::sync::LazyLock;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{
    CANON_ELEMENTS, READ_SIDE, TILE_CELLS, pool_len, table_entries, table_words,
};
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;

/// The display table's read side, then the simulation table's: the same
/// text renamed, so both lookups are the one read side.
static SHADER: LazyLock<String> = LazyLock::new(|| {
    let sim = READ_SIDE
        .replace("ts_", "sim_ts_")
        .replace("WT_", "SIM_WT_");
    format!(
        "{READ_SIDE}\n{sim}\n{}",
        include_str!("shaders/interior_fill.wgsl")
    )
});
const THREADS: u32 = 256;

/// The grid's water fills the display this many simulation cells below its
/// own surface: inside the band, where the particles' blobs already reach.
const FILL_DEPTH_CELLS: f32 = 2.0;

/// Field order identical to the shader's `Params`; two 16-byte rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct FillUniforms {
    cell: f32,
    sim_cell: f32,
    pool_tiles: u32,
    sim_pool_tiles: u32,
    entries: u32,
    sim_entries: u32,
    shrink: f32,
    elements: u32,
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

manifold_node_engine::primitive! {
    name: InteriorFill,
    type_id: "node.interior_fill",
    purpose: "The display's level set with GPU FLIP Narrow Band's grid water filled in. At each node of the display's world tiles (node p at (p - 1/2) display cells), the lower of the level set and the step's interior there, read trilinearly through the simulation's tiles (cell c centred on c simulation cells), raised by two simulation cells: the grid's water fills the display only two cells below its own surface, where the band's particles already draw, so the surface stays the particles'. A free slot and the canonical element keep the level set's value. The water's pool is the interior's length (a whole pool and its canonical element); an interior shorter than one tile is no band, and the level set passes through.",
    inputs: {
        levelset: Array(f32) required,
        interior: Array(f32) required,
        interior_tiles: Array(u32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
        float_param!("sim_cell_size", "Water Cell Size", 0.0625, 0.0001, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire levelset from the surface group's level set, tiles, pool_tiles and cell_size from node.surface_tiles, interior and interior_tiles from node.liquid_state's interior and interior_tiles (the step's interior and tiles carried out of the tick), sim_cell_size from the domain. Only a GPU FLIP step with Narrow Band on has an interior; the GPU FLIP builder wires this node then.",
    examples: [],
    picker: { label: "Interior Fill", category: Atom },
    summary: "Fills the drawn water under the surface from the solver's grid, where Narrow Band keeps no particles.",
    category: Particles3D,
    role: Filter,
    aliases: ["narrow band", "interior fill", "grid water"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        fill_pipeline: Option<GpuComputePipeline> = None,
    },
}

impl InteriorFill {
    fn pipeline(&mut self, device: &GpuDevice) -> &GpuComputePipeline {
        self.fill_pipeline.get_or_insert_with(|| {
            device.create_compute_pipeline(SHADER.as_str(), "cs_fill", "node.interior_fill")
        })
    }
}

impl Primitive for InteriorFill {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.pipeline(device);
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "levelset")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Interior Fill") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let sim_cell = ctx.scalar_or_param("sim_cell_size", 0.0625);
        if !(sim_cell.is_finite() && sim_cell > 0.0) {
            ctx.error(format!(
                "Interior Fill: the water's cell size {sim_cell} is not a length"
            ));
            return;
        }
        let (Some(levelset), Some(interior), Some(interior_tiles), Some(tiles), Some(out)) = (
            ctx.inputs.array("levelset"),
            ctx.inputs.array("interior"),
            ctx.inputs.array("interior_tiles"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let elements = pool.elements();
        if levelset.size.min(out.size) < elements * 4 {
            ctx.error(format!(
                "Interior Fill: the level set holds less than a pool of {} tiles",
                pool.pool_tiles
            ));
            return;
        }
        let Some(sim_pool) = interior_pool(interior.size) else {
            // No band runs: the level set is the drawn water.
            ctx.gpu_encoder()
                .native_enc
                .copy_buffer_to_buffer(levelset, out, elements * 4);
            return;
        };
        if interior.size < pool_len(sim_pool) * 4 || interior_tiles.size < table_words(sim_pool) * 4
        {
            ctx.error(format!(
                "Interior Fill: the interior holds less than the water's pool of {sim_pool} tiles"
            ));
            return;
        }
        let uniforms = FillUniforms {
            cell: pool.cell_size,
            sim_cell,
            pool_tiles: pool.pool_tiles,
            sim_pool_tiles: sim_pool,
            entries: table_entries(pool.pool_tiles),
            sim_entries: table_entries(sim_pool),
            shrink: FILL_DEPTH_CELLS * sim_cell,
            elements: elements as u32,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline(gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                buffer(1, levelset),
                buffer(2, interior),
                buffer(3, interior_tiles),
                buffer(4, tiles),
                buffer(5, out),
            ],
            [(elements as u32).div_ceil(THREADS), 1, 1],
            "node.interior_fill",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
    }
}

/// The water's pool, in tiles, of an interior `bytes` long: a whole pool and
/// its canonical element. `None` below one tile: the step holds no interior.
pub(crate) fn interior_pool(bytes: u64) -> Option<u32> {
    let elements = bytes / 4;
    let tiles = elements.checked_sub(u64::from(CANON_ELEMENTS))? / u64::from(TILE_CELLS);
    u32::try_from(tiles).ok().filter(|&tiles| tiles > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_water_pool_is_the_interiors_length() {
        assert_eq!(interior_pool(4 * pool_len(37)), Some(37));
        assert_eq!(interior_pool(4 * pool_len(1)), Some(1));
        // A one-record hint, or nothing: no band.
        assert_eq!(interior_pool(4), None);
        assert_eq!(interior_pool(0), None);
    }

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("interior fill shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("interior fill shader validates");
        assert_eq!(std::mem::size_of::<FillUniforms>(), 32);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_gpu::testkit::test_device;
    use manifold_water_liquid::testkit::world_box::BoxTable;

    fn shared<T: bytemuck::Pod>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
        let bytes: &[u8] = bytemuck::cast_slice(data);
        let buffer =
            device.create_buffer(bytes.len().max(16) as u64, manifold_gpu::BufferUse::Upload);
        // SAFETY: a new shared buffer at least `bytes` long; no GPU work in flight.
        unsafe { buffer.write(0, bytes) };
        buffer
    }

    fn read(buffer: &GpuBuffer, len: usize) -> Vec<f32> {
        let ptr = buffer.mapped_ptr().expect("shared buffer");
        // SAFETY: a shared buffer of at least `len` floats; its GPU work is done.
        unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), len) }.to_vec()
    }

    /// A display at twice the water's resolution over a grid interior rising
    /// linearly in y (exact under the trilinear read): each node takes the
    /// lower of its level set and the interior shrunk by two water cells,
    /// across the tile boundary at x = 0 of both tables.
    #[test]
    fn the_grid_water_fills_the_display_below_its_shrunk_surface() {
        let device = test_device();
        let h = 0.25f32;
        let hs = h / 2.0;
        let surface = 1.5f32;
        let sim = BoxTable::new([-3, -2, -2], [6, 12, 6]);
        let display = BoxTable::new([-6, -4, -4], [12, 24, 12]);
        let sim_cells = sim.cells.map(|n| n as usize);
        let dense_interior: Vec<f32> = (0..sim_cells.iter().product::<usize>())
            .map(|i| {
                let y = (i / sim_cells[0]) % sim_cells[1];
                (sim.origin[1] + y as i32) as f32 * h - surface
            })
            .collect();
        let interior = shared(&device, &sim.to_pool(&dense_interior, 3.0 * h));
        let nodes = display.cells.map(|n| n as usize);
        let level = |c: [usize; 3]| if c[0].is_multiple_of(2) { -0.3f32 } else { 0.5 };
        let dense_level: Vec<f32> = (0..nodes.iter().product::<usize>())
            .map(|i| {
                level([
                    i % nodes[0],
                    (i / nodes[0]) % nodes[1],
                    i / (nodes[0] * nodes[1]),
                ])
            })
            .collect();
        let levelset = shared(&device, &display.to_pool(&dense_level, 3.0 * hs));
        let elements = pool_len(display.pool_tiles) as usize;
        let out = shared(&device, &vec![0.0f32; elements]);
        let uniforms = FillUniforms {
            cell: hs,
            sim_cell: h,
            pool_tiles: display.pool_tiles,
            sim_pool_tiles: sim.pool_tiles,
            entries: table_entries(display.pool_tiles),
            sim_entries: table_entries(sim.pool_tiles),
            shrink: FILL_DEPTH_CELLS * h,
            elements: elements as u32,
        };
        let pipeline =
            device.create_compute_pipeline(SHADER.as_str(), "cs_fill", "interior fill proof");
        let (sim_words, display_words) =
            (shared(&device, &sim.words), shared(&device, &display.words));
        let mut enc = device.create_encoder("interior fill proof");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                buffer(1, &levelset),
                buffer(2, &interior),
                buffer(3, &sim_words),
                buffer(4, &display_words),
                buffer(5, &out),
            ],
            [(elements as u32).div_ceil(THREADS), 1, 1],
            "interior fill proof",
        );
        enc.commit_and_wait().expect("GPU work completes");
        let got = read(&out, elements);
        let mut filled = 0;
        let mut checked = 0;
        for z in 0..nodes[2] {
            for y in 0..nodes[1] {
                for x in 0..nodes[0] {
                    let c = [x, y, z];
                    let p: [i32; 3] = std::array::from_fn(|a| display.origin[a] + c[a] as i32);
                    // The node's water cells, all inside the water's box.
                    let inside = (0..3).all(|a| {
                        let base = ((p[a] as f32 - 0.5) * 0.5).floor() as i32;
                        base >= sim.origin[a] && base + 1 < sim.origin[a] + sim.cells[a] as i32
                    });
                    if !inside {
                        continue;
                    }
                    checked += 1;
                    let grid = (p[1] as f32 - 0.5) * hs - surface + FILL_DEPTH_CELLS * h;
                    let want = level(c).min(grid);
                    filled += usize::from(grid < level(c));
                    let at = display.pool_index(c.map(|v| v as u32));
                    assert!(
                        (got[at] - want).abs() < 1e-5,
                        "node {p:?}: {} vs {want}",
                        got[at]
                    );
                }
            }
        }
        assert!(
            checked > 0 && filled > 0 && filled < checked,
            "{filled} of {checked} nodes filled"
        );
    }
}
