//! `node.tile_surface_mesh` — the triangle list of a pooled level set's zero
//! surface (marching cubes, each vertex relaxed once toward its neighbours so
//! small drops come out round), built tile by tile on the display's world
//! tiles. Each held tile meshes its own 8³ cells in workgroup memory and takes its
//! run of the vertex buffer with one atomic, so the only memory past the field
//! is the triangles: it follows the water's surface area, not a box.
//!
//! The node owns its vertex buffer and grows it from the triangles the surface
//! asked for, read a frame or two late from the GPU's own count. Growth is the
//! only allocation; steady state allocates nothing.

use std::borrow::Cow;
use std::sync::LazyLock;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::live_extent::{LiveExtent, LiveExtentOutput};
use manifold_node_engine::scene::world_tiles::{READ_SIDE, table_entries};
use manifold_water_liquid::float_param;
use manifold_water_liquid::primitives::redistance_lattice::MARCHING_CUBES_COMMON;
use manifold_water_liquid::tile_pool::tile_pool;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

static SOURCE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{MARCHING_CUBES_COMMON}\n{}",
        include_str!("shaders/tile_surface_mesh.wgsl")
    )
});
const THREADS: u32 = 256;
const VERTEX_BYTES: u64 = std::mem::size_of::<MeshVertex>() as u64;

/// Growth granularity and the first buffer, in triangles.
const GRAIN: u64 = 16_384;
pub(crate) const START_TRIANGLES: u64 = 4 * GRAIN;
/// Headroom over the late count: it is read a frame or two behind the
/// surface, and a splash can more than double it in that time.
const HEADROOM: f64 = 2.0;
/// The buffer grows once the late count needs more than this share of it.
const GROW_AT: f64 = 2.0 / 3.0;
/// Most triangles a buffer holds: every vertex index fits a u32.
const MAX_TRIANGLES: u64 = u32::MAX as u64 / 3;

/// The counts buffer: live triangles, asked triangles, last frame's live.
const COUNTS_BYTES: u64 = 16;

/// Field order identical to the shader's `Params`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshUniforms {
    cell_size: f32,
    pool_tiles: u32,
    entries: u32,
    capacity: u32,
}

/// The capacity, in triangles, to grow to when `asked` crowds `capacity`:
/// `None` while it fits. Whole grains, so a slowly rising surface regrows
/// rarely.
pub(crate) fn grown_capacity(asked: u32, capacity: u64) -> Option<u64> {
    let asked = f64::from(asked);
    if asked <= capacity as f64 * GROW_AT {
        return None;
    }
    let wanted = (asked * HEADROOM).ceil() as u64;
    Some(
        (wanted.div_ceil(GRAIN) * GRAIN)
            .min(MAX_TRIANGLES)
            .max(capacity),
    )
}

/// A CPU upper bound on live vertices (ray-tracing builds): the late count
/// with headroom and a grain, never above the buffer.
pub(crate) fn vertex_bound(asked: u32, capacity: u64) -> u32 {
    let wanted = (f64::from(asked) * HEADROOM).ceil() as u64 + GRAIN;
    (3 * (wanted.div_ceil(GRAIN) * GRAIN).min(capacity)) as u32
}

pub(crate) struct Kernels {
    reset: GpuComputePipeline,
    tiles: GpuComputePipeline,
    finish: GpuComputePipeline,
    tail: GpuComputePipeline,
}

impl Kernels {
    fn new(device: &GpuDevice) -> Self {
        let make = |entry: &str| {
            device.create_compute_pipeline(SOURCE.as_str(), entry, "node.tile_surface_mesh")
        };
        Self {
            reset: make("sm_reset"),
            tiles: make("sm_tiles"),
            finish: make("sm_finish"),
            tail: make("sm_tail"),
        }
    }
}

/// The vertex buffer, its counts and how many frames have written it.
pub(crate) struct Mesh {
    vertices: GpuBuffer,
    counts: GpuBuffer,
    /// Triangles `vertices` holds.
    capacity: u64,
    /// Frames written since `vertices` was made: the late count describes
    /// this buffer's surface from the third.
    frames: u32,
}

fn shared(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    let buffer = device.try_create_buffer_shared(bytes.max(16))?;
    buffer.zero_fill();
    Ok(buffer)
}

impl Mesh {
    /// The triangles the surface asked for on the last frame the GPU finished.
    fn late_asked(&self) -> u32 {
        let ptr = self
            .counts
            .mapped_ptr()
            .expect("the counts buffer is shared");
        // SAFETY: the counts buffer is shared, 16 bytes, u32-aligned; a word
        // the GPU is writing reads as either value.
        unsafe { std::ptr::read_volatile(ptr.cast::<u32>().add(1)) }
    }

    fn encode(
        &mut self,
        enc: &mut GpuEncoder,
        kernels: &Kernels,
        uniforms: &MeshUniforms,
        levelset: &GpuBuffer,
        tiles: &GpuBuffer,
    ) {
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: levelset,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &self.vertices,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &self.counts,
                offset: 0,
            },
        ];
        let mut pass = |pipeline: &GpuComputePipeline, groups, label| {
            enc.dispatch_compute(pipeline, &bindings, groups, label);
            enc.compute_memory_barrier_buffers();
        };
        pass(&kernels.reset, [1, 1, 1], "node.tile_surface_mesh.reset");
        pass(
            &kernels.tiles,
            [uniforms.pool_tiles, 1, 1],
            "node.tile_surface_mesh.tiles",
        );
        pass(&kernels.finish, [1, 1, 1], "node.tile_surface_mesh.finish");
        pass(
            &kernels.tail,
            [(3 * uniforms.capacity).div_ceil(THREADS), 1, 1],
            "node.tile_surface_mesh.tail",
        );
        self.frames = self.frames.saturating_add(1);
    }
}

manifold_node_engine::primitive! {
    name: TileSurfaceMesh,
    type_id: "node.tile_surface_mesh",
    purpose: "The marching-cubes triangle list of a pooled level set's zero surface, on world tiles: node p at (p - 1/2) cells, a cell's corners its eight nodes, inside where the level set is below zero, normals the interpolated central-difference gradient pointing outward. Each vertex moves once to the mean of the vertices it shares a triangle with (one pass of umbrella smoothing), which rounds drops and thins sheets without deleting either. Each held tile meshes its own 8^3 cells; an edge shared by two tiles gives both the same vertex, so the surface is watertight across tile faces. A node in a tile the table does not hold reads the canonical element. UV is the vertex's xz in metres. The vertex buffer starts at 65536 triangles and grows from the triangles the surface asked for, read a frame late; a frame that asks for more than the buffer holds drops the rest and the buffer grows. Vertices past the live triangles are zero, and the live extent is published.",
    inputs: {
        levelset: Array(f32) required,
        tiles: Array(u32) required,
        pool_tiles: ScalarF32 optional,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire levelset from the display's level set (after its smoothing), and tiles, pool_tiles and cell_size from node.surface_tiles. Wire vertices to node.scene_object.vertices with the water's material: render_scene draws only the live triangles.",
    examples: [],
    picker: { label: "Tile Surface Mesh", category: Atom },
    summary: "Builds the triangle mesh of a liquid's surface tile by tile, ready to render with any material.",
    category: Geometry3D,
    role: Filter,
    aliases: ["marching cubes", "isosurface", "surface mesh", "liquid mesh", "water mesh"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        mesh: Option<Mesh> = None,
        kernels: Option<Kernels> = None,
    },
}

inventory::submit! {
    LiveExtentOutput { type_id: "node.tile_surface_mesh", port: "vertices" }
}

impl TileSurfaceMesh {
    /// Make the first buffer, or grow it when the late count crowds it.
    /// Grow-only; a refused allocation keeps the current buffer.
    fn ensure_capacity(&mut self, device: &GpuDevice) -> Result<(), String> {
        let wanted = match &self.mesh {
            None => START_TRIANGLES,
            Some(mesh) => match grown_capacity(mesh.late_asked(), mesh.capacity) {
                Some(wanted) => wanted,
                None => return Ok(()),
            },
        };
        let bytes = wanted * 3 * VERTEX_BYTES;
        let vertices = shared(device, bytes).map_err(|error| {
            format!(
                "the surface needs a {:.0} MB mesh and the device refused it ({error}). Lower the display's Resolution Scale.",
                bytes as f64 / 1e6
            )
        })?;
        let counts = match self.mesh.take() {
            Some(mesh) => {
                log::info!(
                    "[tile_surface_mesh] vertex buffer grown from {} to {wanted} triangles",
                    mesh.capacity
                );
                mesh.counts
            }
            None => shared(device, COUNTS_BYTES)?,
        };
        self.mesh = Some(Mesh {
            vertices,
            counts,
            capacity: wanted,
            frames: 0,
        });
        Ok(())
    }
}

impl Primitive for TileSurfaceMesh {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.kernels.get_or_insert_with(|| Kernels::new(device));
    }

    fn provides_array_output(&self, port: &str) -> bool {
        port == "vertices"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "vertices")
            .then(|| self.mesh.as_ref().map(|mesh| &mesh.vertices))
            .flatten()
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-triangle hint, sized from the surface at run time.
        (port == "vertices").then_some(3)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Tile Surface Mesh") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let Some(levelset) = ctx.inputs.array("levelset") else {
            ctx.error("Tile Surface Mesh: levelset must be wired");
            return;
        };
        if levelset.size < pool.elements() * 4 {
            ctx.error(format!(
                "Tile Surface Mesh: the level set holds less than a pool of {} tiles",
                pool.pool_tiles
            ));
            return;
        }
        let prepared = {
            let device = ctx.gpu_encoder().device;
            self.kernels.get_or_insert_with(|| Kernels::new(device));
            self.ensure_capacity(device)
        };
        if let Err(error) = prepared {
            ctx.error(format!("Tile Surface Mesh: {error}"));
            if self.mesh.is_none() {
                return;
            }
        }
        let (Some(levelset), Some(tiles)) =
            (ctx.inputs.array("levelset"), ctx.inputs.array("tiles"))
        else {
            return;
        };
        let (Some(mesh), Some(kernels)) = (self.mesh.as_mut(), self.kernels.as_ref()) else {
            return;
        };
        // The first two frames on a buffer carry no late count of it.
        let bound = if mesh.frames < 2 {
            (3 * mesh.capacity) as u32
        } else {
            vertex_bound(mesh.late_asked(), mesh.capacity)
        };
        let uniforms = MeshUniforms {
            cell_size: pool.cell_size,
            pool_tiles: pool.pool_tiles,
            entries: table_entries(pool.pool_tiles) as u32,
            capacity: mesh.capacity as u32,
        };
        let gpu = ctx.gpu_encoder();
        mesh.encode(gpu.native_enc, kernels, &uniforms, levelset, tiles);
        let counts = mesh.counts.clone();
        ctx.outputs.set_live_extent(
            "vertices",
            LiveExtent {
                counts,
                offset: 0,
                per_item: 3,
                bound,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SOURCE).expect("tile surface mesh parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("tile surface mesh validates");
        for entry in ["sm_reset", "sm_tiles", "sm_finish", "sm_tail"] {
            assert!(
                module.entry_points.iter().any(|point| point.name == entry),
                "missing {entry}"
            );
        }
        assert_eq!(std::mem::size_of::<MeshUniforms>(), 16);
        assert_eq!(VERTEX_BYTES, 80);
    }

    #[test]
    fn the_buffer_grows_in_grains_ahead_of_the_surface() {
        // Two thirds full or less: no growth.
        assert_eq!(grown_capacity(0, START_TRIANGLES), None);
        assert_eq!(grown_capacity(43_690, START_TRIANGLES), None);
        // Past two thirds: twice the ask, whole grains.
        assert_eq!(grown_capacity(43_700, START_TRIANGLES), Some(6 * GRAIN));
        assert_eq!(grown_capacity(100_000, START_TRIANGLES), Some(13 * GRAIN));
        // Never past what a u32 vertex index reaches, never below the buffer.
        assert_eq!(
            grown_capacity(u32::MAX, START_TRIANGLES),
            Some(MAX_TRIANGLES)
        );
        assert_eq!(grown_capacity(10, 1), Some(GRAIN));
    }

    #[test]
    fn the_bound_covers_the_late_surface_within_the_buffer() {
        // Twice the ask and a grain, whole grains, in vertices.
        assert_eq!(vertex_bound(0, START_TRIANGLES), 3 * GRAIN as u32);
        assert_eq!(vertex_bound(10_000, START_TRIANGLES), 3 * 3 * GRAIN as u32);
        // Never past the buffer.
        assert_eq!(
            vertex_bound(60_000, START_TRIANGLES),
            3 * START_TRIANGLES as u32
        );
    }
}
