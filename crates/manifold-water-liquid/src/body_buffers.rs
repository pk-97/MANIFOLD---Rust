//! The GPU storage a liquid domain provides for its bodies: the body rows and
//! their contact normals, the source and drain region rows, the shapes and the half-precision
//! distance atlas, and the shapes' tree nodes and triangles. Shapes, atlas and
//! trees are rebuilt into fresh buffers, so a buffer the GPU may still read is
//! never written; the atlas is filled on the GPU from the trees in the same
//! encoder, before any reader. Body and region rows are written in encoder
//! order each frame.

use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use crate::bodies::{BodySupports, COLLIDER_BVH, LiquidBodies, LiquidBody, LiquidShape};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;

const UPLOAD_SHADER: &str = include_str!("primitives/shaders/liquid_body_upload.wgsl");
const LATTICE_KERNEL: &str = include_str!("primitives/shaders/collider_lattice.wgsl");
/// 16-byte groups one inline upload carries (setBytes stays under 4 KB).
const UPLOAD_GROUPS: usize = 254;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UploadParams {
    start: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    words: [[u32; 4]; UPLOAD_GROUPS],
}

const _: () = assert!(std::mem::size_of::<UploadParams>() < 4096);

/// `collider_lattice.wgsl`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LatticeParams {
    origin_spacing: [f32; 4],
    dims_word: [u32; 4],
    counts: [u32; 4],
    band_shell: [f32; 4],
}

/// Workgroups of 64 threads along a lattice dispatch's x, matching the
/// kernel's row length.
const LATTICE_ROW_GROUPS: u32 = 4096;

/// The lattice kernel's source: the tree queries, then the kernel over them.
pub(crate) fn lattice_shader() -> String {
    format!("{COLLIDER_BVH}\n{LATTICE_KERNEL}")
}

struct Stored {
    bodies: GpuBuffer,
    contacts: GpuBuffer,
    regions: GpuBuffer,
    shapes: GpuBuffer,
    atlas: GpuBuffer,
    collider_nodes: GpuBuffer,
    collider_triangles: GpuBuffer,
    clock_obstacles: GpuBuffer,
    clock_sources: GpuBuffer,
    /// The clock vertices' generation last written; `None` until written.
    clock_gen: Option<u64>,
    version: u64,
}

/// A domain's provided `bodies`, `regions`, `shapes`, `atlas`,
/// `collider_nodes` and `collider_triangles` outputs.
#[derive(Default)]
pub struct LiquidBodyBuffers {
    stored: Option<Stored>,
    upload: Option<GpuComputePipeline>,
    lattice: Option<GpuComputePipeline>,
}

impl LiquidBodyBuffers {
    pub fn bodies(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.bodies)
    }

    pub fn contacts(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.contacts)
    }

    pub fn regions(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.regions)
    }

    pub fn shapes(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.shapes)
    }

    pub fn atlas(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.atlas)
    }

    pub fn collider_nodes(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.collider_nodes)
    }

    pub fn collider_triangles(&self) -> Option<&GpuBuffer> {
        self.stored
            .as_ref()
            .map(|stored| &stored.collider_triangles)
    }

    /// The provided buffer behind a domain's `bodies`, `regions`, `shapes`,
    /// `atlas`, `collider_nodes` or `collider_triangles` output.
    pub fn output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "bodies" => self.bodies(),
            "contacts" => self.contacts(),
            "regions" => self.regions(),
            "shapes" => self.shapes(),
            "atlas" => self.atlas(),
            "collider_nodes" => self.collider_nodes(),
            "collider_triangles" => self.collider_triangles(),
            "clock_obstacles" => self.stored.as_ref().map(|s| &s.clock_obstacles),
            "clock_sources" => self.stored.as_ref().map(|s| &s.clock_sources),
            _ => None,
        }
    }

    /// Keep the buffers current with `bodies`: a new shape version rebuilds
    /// shapes, atlas and colliders' trees, and when `rows_fresh` this frame's body and region
    /// rows are written in encoder order. `label` names the dispatches for
    /// the profiler. A rebuild whose atlas the device cannot admit is
    /// refused by the shape that needs the most, leaving the buffers as they
    /// were.
    pub fn upload(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        bodies: &LiquidBodies,
        rows_fresh: bool,
        label: &'static str,
    ) -> Result<(), String> {
        let row_bytes = std::mem::size_of_val(bodies.last_rows());
        let region_bytes = std::mem::size_of_val(bodies.last_region_rows());
        let needs_bodies = self
            .stored
            .as_ref()
            .is_none_or(|stored| stored.bodies.size < row_bytes as u64);
        let contact_bytes = std::mem::size_of_val(bodies.last_contacts());
        let needs_contacts = self
            .stored
            .as_ref()
            .is_none_or(|stored| stored.contacts.size < contact_bytes as u64);
        let needs_regions = self
            .stored
            .as_ref()
            .is_none_or(|stored| stored.regions.size < region_bytes as u64);
        let obstacle_bytes = std::mem::size_of_val(bodies.clock_obstacles());
        let source_bytes = std::mem::size_of_val(bodies.clock_sources());
        let needs_clock = self.stored.as_ref().is_none_or(|s| {
            s.clock_obstacles.size < obstacle_bytes as u64
                || s.clock_sources.size < source_bytes as u64
        });
        let version = bodies.version;
        if self
            .stored
            .as_ref()
            .is_none_or(|stored| stored.version != version)
            || needs_bodies
            || needs_contacts
            || needs_regions
            || needs_clock
        {
            let atlas = if bodies.lattice_builds().is_empty() {
                gpu.device.create_buffer(4, BufferUse::Gpu)
            } else {
                let bytes = u64::from(bodies.atlas_words()) * 4;
                let largest = bodies
                    .lattice_builds()
                    .iter()
                    .max_by_key(|build| build.nodes())
                    .map_or("a shape", |build| build.name.as_str());
                manifold_node_engine::load::expand::admit_world_bytes(
                    gpu.device.modifier_memory_snapshot(),
                    bytes,
                )
                .map_err(|error| format!("Liquid: {largest}'s distance lattice: {error}"))?;
                gpu.device
                    .try_create_buffer(bytes, BufferUse::Gpu)
                    .map_err(|error| format!("Liquid: {largest}'s distance lattice: {error}"))?
            };
            let fresh = |bytes: &[u8], least: usize| {
                let buffer = gpu
                    .device
                    .create_buffer(bytes.len().max(least) as u64, BufferUse::Upload);
                // SAFETY: new shared buffer, not yet visible to the GPU.
                unsafe { buffer.write(0, bytes) };
                buffer
            };
            let row = std::mem::size_of::<LiquidBody>();
            // Packed only for this upload: the trees are shared, the GPU keeps
            // the one packed copy.
            let (mut nodes, mut triangles) = (Vec::new(), Vec::new());
            bodies.pack_colliders(&mut nodes, &mut triangles);
            let (old_bodies, old_contacts, old_regions) = match self.stored.take() {
                Some(stored) => (
                    Some(stored.bodies),
                    Some(stored.contacts),
                    Some(stored.regions),
                ),
                None => (None, None, None),
            };
            let keep = |old: Option<GpuBuffer>, needs: bool, bytes: usize, usage| match old {
                Some(buffer) if !needs => buffer,
                _ => gpu.device.create_buffer(bytes.max(row) as u64, usage),
            };
            // A coupled liquid's tick exchange writes the body rows and the
            // contact normals from the CPU (`exchange_tick`), so both are
            // shared; the region rows only ever take the GPU upload.
            self.stored = Some(Stored {
                bodies: keep(old_bodies, needs_bodies, row_bytes, BufferUse::Upload),
                contacts: match old_contacts {
                    Some(buffer) if !needs_contacts => buffer,
                    _ => gpu.device.create_buffer(
                        contact_bytes.max(std::mem::size_of::<BodySupports>()) as u64,
                        BufferUse::Upload,
                    ),
                },
                regions: keep(old_regions, needs_regions, region_bytes, BufferUse::Gpu),
                shapes: fresh(
                    bytemuck::cast_slice(bodies.shapes()),
                    std::mem::size_of::<LiquidShape>(),
                ),
                atlas,
                collider_nodes: fresh(bytemuck::cast_slice(&nodes), 16),
                collider_triangles: fresh(bytemuck::cast_slice(&triangles), 16),
                // The coupled exchange writes the clock vertices from the CPU too.
                clock_obstacles: gpu
                    .device
                    .create_buffer(obstacle_bytes.max(96) as u64, BufferUse::Upload),
                clock_sources: gpu
                    .device
                    .create_buffer(source_bytes.max(96) as u64, BufferUse::Upload),
                clock_gen: None,
                version,
            });
            let stored = self.stored.as_ref().expect("allocated above");
            if !bodies.lattice_builds().is_empty() {
                let pipeline = self.lattice.get_or_insert_with(|| {
                    gpu.device.create_compute_pipeline(
                        &lattice_shader(),
                        "main",
                        "collider_lattice",
                    )
                });
                for build in bodies.lattice_builds() {
                    let params = LatticeParams {
                        origin_spacing: [
                            build.origin[0],
                            build.origin[1],
                            build.origin[2],
                            build.spacing,
                        ],
                        dims_word: [build.dims[0], build.dims[1], build.dims[2], build.word_base],
                        counts: [build.nodes(), build.tree_node, build.tree_triangle, 0],
                        band_shell: [build.band(), build.spacing, 0.0, 0.0],
                    };
                    let groups = build.words().div_ceil(64);
                    gpu.native_enc.dispatch_compute(
                        pipeline,
                        &[
                            GpuBinding::Bytes {
                                binding: 0,
                                data: bytemuck::bytes_of(&params),
                            },
                            GpuBinding::Buffer {
                                binding: 1,
                                buffer: &stored.collider_nodes,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 2,
                                buffer: &stored.collider_triangles,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 3,
                                buffer: &stored.atlas,
                                offset: 0,
                            },
                        ],
                        [
                            groups.min(LATTICE_ROW_GROUPS),
                            groups.div_ceil(LATTICE_ROW_GROUPS),
                            1,
                        ],
                        "collider_lattice",
                    );
                }
                gpu.native_enc.compute_memory_barrier_buffers();
            }
        }
        if !rows_fresh {
            return Ok(());
        }
        let pipeline = self.upload.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(UPLOAD_SHADER, "cs_main", label)
        });
        let stored = self.stored.as_mut().expect("allocated above");
        for (target, groups) in [
            (
                &stored.bodies,
                bytemuck::cast_slice::<_, [u32; 4]>(bodies.last_rows()),
            ),
            (
                &stored.contacts,
                bytemuck::cast_slice(bodies.last_contacts()),
            ),
            (
                &stored.regions,
                bytemuck::cast_slice(bodies.last_region_rows()),
            ),
        ] {
            write_groups(gpu, pipeline, target, groups, label);
        }
        // The clock vertices are the bulk of the bytes and are unchanged for
        // a still scene, so they are rewritten only when they changed.
        if stored.clock_gen != Some(bodies.clock_generation()) {
            for (target, groups) in [
                (
                    &stored.clock_obstacles,
                    bytemuck::cast_slice(bodies.clock_obstacles()),
                ),
                (
                    &stored.clock_sources,
                    bytemuck::cast_slice(bodies.clock_sources()),
                ),
            ] {
                write_groups(gpu, pipeline, target, groups, label);
            }
            stored.clock_gen = Some(bodies.clock_generation());
        }
        Ok(())
    }

    /// Writes `groups`, 16-byte rows, from the start of `target` in encoder
    /// order, as the body and region rows are: the domain's own row arrays
    /// (its region site boxes) use the same upload.
    pub fn write_rows(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        target: &GpuBuffer,
        groups: &[[u32; 4]],
        label: &'static str,
    ) {
        let pipeline = self.upload.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(UPLOAD_SHADER, "cs_main", label)
        });
        write_groups(gpu, pipeline, target, groups, label);
    }
}

/// One inline-bytes dispatch per [`UPLOAD_GROUPS`] rows of `groups`.
fn write_groups(
    gpu: &mut GpuEncoder<'_>,
    pipeline: &GpuComputePipeline,
    target: &GpuBuffer,
    groups: &[[u32; 4]],
    label: &'static str,
) {
    for (chunk_index, chunk) in groups.chunks(UPLOAD_GROUPS).enumerate() {
        let mut params: UploadParams = bytemuck::Zeroable::zeroed();
        let UploadParams {
            start,
            count,
            words,
            ..
        } = &mut params;
        *start = (chunk_index * UPLOAD_GROUPS) as u32;
        *count = chunk.len() as u32;
        words[..chunk.len()].copy_from_slice(chunk);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: target,
                    offset: 0,
                },
            ],
            [(chunk.len() as u32).div_ceil(64), 1, 1],
            label,
        );
    }
}
