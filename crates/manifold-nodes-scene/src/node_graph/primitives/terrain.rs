//! `node.terrain` — a square height map across `size` metres in the XZ
//! plane: a smooth body of noise with gullies cut down its slopes, then
//! carved by simulated rain (`shaders/terrain.wgsl`), published as a grid of
//! `MeshVertex` items.
//!
//! The rain runs only while the land settles. A new seed or resolution
//! starts it from bare noise; a change of size, feature size or roughness
//! halves the carving already done and rains half again, so the land stays
//! carved while those move live. Weathering is the rain in drops per cell:
//! raising it rains the rest, lowering it scales the carving down. Height
//! scales the finished land without rain. Once settled the node costs
//! nothing and its vertices hold. Each vertex carries its normal, from its
//! neighbours across the grid, and in its second UV set where water runs
//! (gully floors and the rain's channels) and where its soil settled.
//!
//! While its `source` is read (a Terrain Collider), the settled land's heights
//! are read back once and published as a [`MeshSource::HeightField`]: the
//! ground bodies, water, Matter and smoke collide with.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::mesh_change::{MeshOutputRule, MeshRevisionRule};
use manifold_node_engine::scene::mesh_source::MeshSource;

const SHADER: &str = include_str!("shaders/terrain.wgsl");
const THREADS: u32 = 256;
const DEFAULT_RESOLUTION: u32 = 512;
/// Raindrops a frame while the land settles, in batches each followed by
/// the loose soil sliding, so drops running at once never pile a hollow
/// into a spike.
const DROPLETS_PER_BATCH: u32 = 1 << 14;
const BATCHES_PER_FRAME: u32 = 4;
const SLIDES_PER_BATCH: u32 = 2;
/// The steepest slope loose soil holds: about 33°, the angle of repose of
/// dry scree and sand.
const REPOSE: f32 = 0.65;

/// Field order identical to the shader's `TerrainUniforms`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct TerrainUniforms {
    res: u32,
    droplets: u32,
    first: u32,
    seed: u32,
    size: f32,
    height: f32,
    feature_size: f32,
    roughness: f32,
    keep: f32,
    repose: f32,
    rain: f32,
    pad: u32,
}

/// What the noise under the carving depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shape {
    size: f32,
    feature_size: f32,
    roughness: f32,
    seed: u32,
}

struct Pipelines {
    body: GpuComputePipeline,
    base: GpuComputePipeline,
    fade: GpuComputePipeline,
    erode: GpuComputePipeline,
    snapshot: GpuComputePipeline,
    slide: GpuComputePipeline,
    vertices: GpuComputePipeline,
    heights: GpuComputePipeline,
}

impl Pipelines {
    fn build(device: &GpuDevice) -> Self {
        let make = |entry: &str| device.create_compute_pipeline(SHADER, entry, "node.terrain");
        Self {
            body: make("body_main"),
            base: make("base_main"),
            fade: make("fade_main"),
            erode: make("erode_main"),
            snapshot: make("snapshot_main"),
            slide: make("slide_main"),
            vertices: make("vertices_main"),
            heights: make("heights_main"),
        }
    }
}

struct TerrainBuffers {
    res: u32,
    base: GpuBuffer,
    erosion: GpuBuffer,
    vertices: GpuBuffer,
    snapshot: GpuBuffer,
    flow: GpuBuffer,
    drain: GpuBuffer,
}

impl TerrainBuffers {
    fn new(device: &GpuDevice, res: u32) -> Result<Self, String> {
        let cells = u64::from(res) * u64::from(res);
        Ok(Self {
            res,
            base: device.try_create_buffer(cells * 4, BufferUse::Gpu)?,
            erosion: device.try_create_buffer(cells * 4, BufferUse::Gpu)?,
            vertices: device
                .try_create_buffer(cells * size_of::<MeshVertex>() as u64, BufferUse::Gpu)?,
            snapshot: device.try_create_buffer(cells * 4, BufferUse::Gpu)?,
            flow: device.try_create_buffer(cells * 4, BufferUse::Gpu)?,
            drain: device.try_create_buffer(cells * 4, BufferUse::Gpu)?,
        })
    }
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

fn resolution(params: &ParamValues) -> u32 {
    match params.get("resolution") {
        Some(ParamValue::Float(n)) => n.round().clamp(4.0, 4096.0) as u32,
        _ => DEFAULT_RESOLUTION,
    }
}

/// The carving still to keep after the land changes from `old` to `new`,
/// with `done` raindrops of it fallen: (share kept, drops counted as fallen).
fn carving_after(old: Option<Shape>, new: Shape, done: f64) -> (f32, f64) {
    match old {
        Some(old) if old == new => (1.0, done),
        Some(old) if old.seed == new.seed => (0.5, done * 0.5),
        _ => (0.0, 0.0),
    }
}

manifold_node_engine::primitive! {
    name: Terrain,
    type_id: "node.terrain",
    purpose: "Emit a resolution × resolution grid of MeshVertex items across size metres in the XZ plane: value-noise hills and massifs with branching gullies cut down their slopes, carved by simulated raindrops that wear channels down slopes and lay soil where they slow. The rain runs over a few frames whenever the land changes and costs nothing once settled. Pair with node.make_triangles (src_cols = src_rows = resolution) for drawable triangles with normals.",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
        source: MeshSource,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("resolution"),
            label: "Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(512.0),
            range: Some((4.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("size"),
            label: "Size",
            ty: ParamType::Float,
            default: ParamValue::Float(600.0),
            range: Some((1.0, 20_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height"),
            label: "Height",
            ty: ParamType::Float,
            default: ParamValue::Float(120.0),
            range: Some((0.0, 5000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("feature_size"),
            label: "Feature Size",
            ty: ParamType::Float,
            default: ParamValue::Float(250.0),
            range: Some((1.0, 10_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("roughness"),
            label: "Roughness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 0.95)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("weathering"),
            label: "Weathering",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed"),
            label: "Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 999.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "size is the map's side in metres; the land is fixed to the object, so Size reveals more or less of the same hills. feature_size is the widest hill's wavelength. roughness is each octave's amplitude ratio. weathering is raindrops per cell: 0 is bare noise. Height scales the result, which spans about ±height, without re-running the rain.",
    examples: [],
    picker: { label: "Terrain", category: Atom },
    summary: "Builds a landscape of hills and mountains carved by simulated rain, as a grid of points ready to triangulate.",
    category: Geometry3D,
    role: Source,
    aliases: ["terrain", "landscape", "mountains", "heightfield", "erosion"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        pipelines: Option<Pipelines> = None,
        buffers: Option<TerrainBuffers> = None,
        // The noise the carving lies on; None until the first run.
        shape: Option<Shape> = None,
        // Raindrops counted as fallen into the carving, and raindrops ever
        // started (each drop's randomness, never reused).
        done: f64 = 0.0,
        spawned: u32 = 0,
        // The Height and executor epoch the vertices were last written at.
        drawn: Option<(f32, u64)> = None,
        // Rain still to fall into the carving: the land is not settled yet.
        raining: bool = true,
        // The settled land's heights as a collider, once read back; whether
        // the land changed since the last readback; the readback in flight
        // (set once its command buffer completes; points per side and size)
        // and its buffer.
        ground: Option<MeshSource> = None,
        ground_stale: bool = true,
        ground_pending: Option<(Arc<AtomicBool>, u32, f32)> = None,
        ground_readback: Option<GpuBuffer> = None,
    },
}

impl Terrain {
    /// Publish a readback whose command buffer completed.
    fn take_ground(&mut self) {
        let Some((done, points, size)) = &self.ground_pending else {
            return;
        };
        if !done.load(Ordering::Acquire) {
            return;
        }
        let (points, size) = (*points, *size);
        self.ground_pending = None;
        let Some(readback) = &self.ground_readback else {
            return;
        };
        let ptr = readback
            .mapped_ptr()
            .expect("node.terrain: the heights readback must be CPU-visible");
        // SAFETY: the frame that wrote this shared buffer has retired, and it
        // holds `points` squared heights.
        let heights = unsafe {
            std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), (points * points) as usize)
        };
        self.ground = Some(MeshSource::HeightField {
            points,
            size,
            heights: Arc::from(heights),
        });
    }

    /// Read the drawn land's heights back, through the snapshot (free once
    /// the rain is done).
    fn read_ground(
        &mut self,
        gpu: &mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>,
        uniforms: &TerrainUniforms,
    ) -> Result<(), String> {
        let (Some(pipelines), Some(buffers)) = (self.pipelines.as_ref(), self.buffers.as_ref())
        else {
            return Ok(());
        };
        let bytes = buffers.snapshot.size;
        if self
            .ground_readback
            .as_ref()
            .is_none_or(|b| b.size != bytes)
        {
            self.ground_readback = Some(gpu.device.try_create_buffer(bytes, BufferUse::Readback)?);
        }
        let Some(readback) = &self.ground_readback else {
            return Ok(());
        };
        gpu.native_enc.dispatch_compute(
            &pipelines.heights,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(uniforms),
                },
                buffer(3, &buffers.vertices),
                buffer(4, &buffers.snapshot),
            ],
            [(uniforms.res * uniforms.res).div_ceil(THREADS), 1, 1],
            "node.terrain heights",
        );
        gpu.native_enc
            .copy_buffer_to_buffer(&buffers.snapshot, readback, bytes);
        // Done when its command buffer completes, whichever loop committed it
        // (live frames, or the load's warm-up, which runs outside the frame
        // clock).
        let done = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&done);
        gpu.native_enc
            .add_completed_handler(move || signal.store(true, Ordering::Release));
        self.ground_pending = Some((done, uniforms.res, uniforms.size));
        self.ground_stale = false;
        Ok(())
    }
}

impl Primitive for Terrain {
    /// The land settles once all its rain has fallen, so a still taken after
    /// the load's warm-up shows the finished carving.
    fn warmup_pending(&self) -> bool {
        self.raining || self.ground_pending.is_some()
    }

    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.pipelines
            .get_or_insert_with(|| Pipelines::build(device));
    }

    /// The grid's triangles never change; only its points move, so a morph
    /// refits the ray-tracing structure instead of rebuilding it.
    fn mesh_output_rule(&self, _port: &str) -> MeshOutputRule<'_> {
        MeshOutputRule {
            topology: MeshRevisionRule::Fixed,
            positions: MeshRevisionRule::Written,
        }
    }

    fn provides_array_output(&self, port: &str) -> bool {
        port == "vertices"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "vertices" => self.buffers.as_ref().map(|b| &b.vertices),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "vertices").then(|| {
            let res = resolution(params);
            res * res
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let res = resolution(ctx.params);
        let shape = Shape {
            size: ctx.param_f32("size", 600.0).max(1.0),
            feature_size: ctx.param_f32("feature_size", 250.0),
            roughness: ctx.param_f32("roughness", 0.5),
            seed: ctx.param_f32("seed", 1.0).round().max(0.0) as u32,
        };
        let height = ctx.param_f32("height", 120.0);
        let budget = f64::from(ctx.param_f32("weathering", 1.0).max(0.0)) * f64::from(res * res);

        let prepared = {
            let device = ctx.gpu_encoder().device;
            self.pipelines
                .get_or_insert_with(|| Pipelines::build(device));
            if self.buffers.as_ref().is_none_or(|b| b.res != res) {
                self.buffers = None;
                self.shape = None;
                TerrainBuffers::new(device, res).map(|buffers| self.buffers = Some(buffers))
            } else {
                Ok(())
            }
        };
        if let Err(error) = prepared {
            ctx.error(format!(
                "Terrain: the height map needs storage the device cannot give: {error}"
            ));
            return;
        }
        let reshaped = self.shape != Some(shape);
        let (mut keep, mut done) = carving_after(self.shape, shape, self.done);
        if done > budget {
            keep *= (budget / done) as f32;
            done = budget;
        }
        let droplets = (budget - done)
            .ceil()
            .min(f64::from(DROPLETS_PER_BATCH * BATCHES_PER_FRAME)) as u32;
        let changed = reshaped || keep < 1.0 || droplets > 0;
        self.raining = done + f64::from(droplets) < budget;
        let drawn = (height, ctx.rebuild_epoch);
        // The land itself changes (not just a rewrite of the same vertices
        // for a new epoch): the collider's heights need reading again.
        let relaid = changed
            || self
                .drawn
                .is_none_or(|(drawn_height, _)| drawn_height != height);
        let collides = ctx.outputs.slot("source").is_some();
        if collides {
            self.take_ground();
            if let Some(ground) = &self.ground {
                ctx.outputs.set_mesh_source("source", ground.clone());
            }
        }
        // Until its first ground lands a Collider waits for it, as for a
        // loading mesh, so the World never starts without the ground. A later
        // change keeps the last ground until the new one lands.
        if collides && self.ground.is_none() {
            ctx.mark_outputs_pending();
        }
        // Settled and drawn at this Height: the vertices hold, so the shadow
        // cache and ray-tracing structure keep. A state-carrying rebuild
        // carries a new epoch, so it writes them once.
        if !changed && self.drawn == Some(drawn) && ctx.outputs_retained() {
            if collides && self.ground_stale {
                let uniforms = TerrainUniforms {
                    res,
                    size: shape.size,
                    ..bytemuck::Zeroable::zeroed()
                };
                if let Err(error) = self.read_ground(ctx.gpu_encoder(), &uniforms) {
                    ctx.error(format!("Terrain: no room to read its ground back: {error}"));
                }
            }
            // A pending frame is never an unchanged one.
            if !(collides && self.ground.is_none()) {
                ctx.mark_outputs_unchanged();
            }
            return;
        }
        let (Some(pipelines), Some(buffers)) = (self.pipelines.as_ref(), self.buffers.as_ref())
        else {
            return;
        };
        let gpu = ctx.gpu_encoder();
        let spacing = shape.size / (res - 1) as f32;
        let uniforms = TerrainUniforms {
            res,
            droplets: 0,
            first: 0,
            seed: shape.seed,
            size: shape.size,
            height,
            feature_size: shape.feature_size,
            roughness: shape.roughness,
            keep,
            repose: REPOSE * spacing / height.max(1e-3),
            rain: ((done + f64::from(droplets)) / f64::from(res * res)) as f32,
            pad: 0,
        };
        let dispatch = |enc: &mut manifold_gpu::GpuEncoder,
                        pipeline: &GpuComputePipeline,
                        uniforms: &TerrainUniforms,
                        groups: [u32; 3],
                        label: &str| {
            let bindings = [
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(uniforms),
                },
                buffer(1, &buffers.base),
                buffer(2, &buffers.erosion),
                buffer(3, &buffers.vertices),
                buffer(4, &buffers.snapshot),
                buffer(5, &buffers.flow),
                buffer(6, &buffers.drain),
            ];
            enc.dispatch_compute(pipeline, &bindings, groups, label);
        };
        let cells = [(res * res).div_ceil(THREADS), 1, 1];
        let enc = &mut *gpu.native_enc;
        if reshaped {
            dispatch(enc, &pipelines.body, &uniforms, cells, "node.terrain body");
            dispatch(enc, &pipelines.base, &uniforms, cells, "node.terrain base");
        }
        if keep < 1.0 {
            dispatch(enc, &pipelines.fade, &uniforms, cells, "node.terrain fade");
        }
        let mut first = 0;
        while first < droplets {
            let batch = TerrainUniforms {
                droplets: (droplets - first).min(DROPLETS_PER_BATCH),
                first: self.spawned.wrapping_add(first),
                ..uniforms
            };
            let groups = [batch.droplets.div_ceil(THREADS), 1, 1];
            // Drops read the land as the batch found it, so no drop sees
            // another's change mid-batch and every run carves alike.
            dispatch(
                enc,
                &pipelines.snapshot,
                &uniforms,
                cells,
                "node.terrain snapshot",
            );
            dispatch(enc, &pipelines.erode, &batch, groups, "node.terrain erode");
            for _ in 0..SLIDES_PER_BATCH {
                dispatch(
                    enc,
                    &pipelines.snapshot,
                    &uniforms,
                    cells,
                    "node.terrain snapshot",
                );
                dispatch(
                    enc,
                    &pipelines.slide,
                    &uniforms,
                    cells,
                    "node.terrain slide",
                );
            }
            first += batch.droplets;
        }
        dispatch(
            enc,
            &pipelines.vertices,
            &uniforms,
            cells,
            "node.terrain vertices",
        );
        self.shape = Some(shape);
        self.done = done + f64::from(droplets);
        self.spawned = self.spawned.wrapping_add(droplets);
        self.drawn = Some(drawn);
        self.ground_stale |= relaid;
        // The collider follows the land once it settles.
        if collides
            && !self.raining
            && self.ground_stale
            && let Err(error) = self.read_ground(gpu, &uniforms)
        {
            ctx.error(format!("Terrain: no room to read its ground back: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("terrain shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("terrain shader validates");
        assert_eq!(size_of::<TerrainUniforms>(), 48);
        assert!(module.entry_points.iter().any(|e| e.name == "slide_main"));
    }

    #[test]
    fn carving_fades_with_the_land_under_it() {
        let shape = Shape {
            size: 600.0,
            feature_size: 250.0,
            roughness: 0.5,
            seed: 3,
        };
        assert_eq!(carving_after(None, shape, 0.0), (0.0, 0.0));
        assert_eq!(carving_after(Some(shape), shape, 9000.0), (1.0, 9000.0));
        let wider = Shape {
            size: 800.0,
            ..shape
        };
        assert_eq!(carving_after(Some(shape), wider, 9000.0), (0.5, 4500.0));
        let reseeded = Shape { seed: 4, ..shape };
        assert_eq!(carving_after(Some(shape), reseeded, 9000.0), (0.0, 0.0));
    }
}
