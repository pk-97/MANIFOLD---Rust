//! Per-patch culling of indexed meshes (`shaders/cull_patches.wgsl`). A patch
//! is a run of [`PATCH_TRIANGLES`] consecutive triangles of the index buffer
//! (imports order their triangles with [`patch_order`] and `node.grid_indices`
//! writes Terrain in tiles, so each run is compact; each run's box and normal
//! cone are worked out on the GPU once per mesh content), or, for a mesh
//! with a cluster hierarchy (`mesh_clusters.rs`), a cluster of it. Each view
//! (the camera, every shadow caster) then copies only the indices of the
//! patches it can see, on its own detail cut, into an index buffer of its
//! own and draws that, indexed and indirect, so hidden geometry costs no
//! vertex work and far geometry draws coarser.

use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};
use manifold_node_engine::content_revision::ContentVersion;

/// Triangles in one patch. `shaders/cull_patches.wgsl` and
/// `node.grid_indices` (tiles of 8 x 8 quads) hold the same number.
pub(crate) const PATCH_TRIANGLES: u32 = 128;
const PATCH_INDICES: u32 = PATCH_TRIANGLES * 3;
/// One patch record, as `mesh_clusters.rs` lays a cluster's out.
const PATCH_BYTES: u64 = 24 * 4;
/// Byte offset of the cluster array's dispatch words (`mesh_clusters.rs`).
const CLUSTER_DISPATCH_BYTES: u64 = 4 * 4;
/// Fewest patches worth a cull: a smaller mesh draws whole.
const MIN_PATCHES: u32 = 8;
/// Workgroups along x before a dispatch wraps to the next row.
const ROW: u32 = 32_768;

const SHADER: &str = include_str!("../shaders/cull_patches.wgsl");

/// Order `indices` so every run of [`PATCH_TRIANGLES`] triangles is compact:
/// triangles sorted along a space-filling curve, then each run reordered for
/// the GPU's vertex reuse. The same triangles, the same winding.
pub(crate) fn patch_order(positions: &[[f32; 3]], indices: &[u32]) -> Vec<u32> {
    if indices.iter().any(|&i| i as usize >= positions.len()) {
        return indices.to_vec();
    }
    let Ok(adapter) = meshopt::VertexDataAdapter::new(
        bytemuck::cast_slice(positions),
        std::mem::size_of::<[f32; 3]>(),
        0,
    ) else {
        return indices.to_vec();
    };
    let mut sorted = meshopt::spatial_sort_triangles(indices, &adapter);
    // Each run renumbered to its own vertices, so the cache pass costs the
    // run's size, not the mesh's. `stamp[v]` is the last run (plus one) that
    // numbered vertex v, `number[v]` its number there.
    let mut stamp = vec![0u32; positions.len()];
    let mut number = vec![0u32; positions.len()];
    let mut local: Vec<u32> = Vec::with_capacity(PATCH_INDICES as usize);
    let mut seen: Vec<u32> = Vec::with_capacity(PATCH_INDICES as usize);
    for (run_id, run) in sorted.chunks_mut(PATCH_INDICES as usize).enumerate() {
        let run_stamp = run_id as u32 + 1;
        local.clear();
        seen.clear();
        for &index in run.iter() {
            let v = index as usize;
            if stamp[v] != run_stamp {
                stamp[v] = run_stamp;
                number[v] = seen.len() as u32;
                seen.push(index);
            }
            local.push(number[v]);
        }
        meshopt::optimize_vertex_cache_in_place(&mut local, seen.len());
        for (slot, &at) in run.iter_mut().zip(&local) {
            *slot = seen[at as usize];
        }
    }
    sorted
}

/// Field order identical to the shader's `PatchArgs`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PatchArgs {
    clip_model: [[f32; 4]; 4],
    eye: [f32; 4],
    viewport: [f32; 2],
    index_count: u32,
    patch_count: u32,
    block: u32,
    clusters: u32,
    _pad0: u32,
    _pad1: u32,
}

/// One view's cull of one mesh.
pub(super) struct PatchView<'a> {
    /// The view's clip matrix, and the mesh's model matrix.
    pub clip: &'a [[f32; 4]; 4],
    pub model: &'a [[f32; 4]; 4],
    /// The eye in world space when the view may drop patches facing away
    /// from it: the camera, for a mesh that skips back faces.
    pub eye: Option<[f32; 3]>,
    /// The arguments buffer and the view's block in it.
    pub args: &'a GpuBuffer,
    pub block: u64,
    /// The view's size in pixels, which picks the detail cut.
    pub viewport: [u32; 2],
}

/// One object slot's patches and the views' index buffers.
#[derive(Default)]
struct Slot {
    bounds: Option<GpuBuffer>,
    /// The mesh content the bounds were worked out for, and the epoch.
    key: Option<(Option<ContentVersion>, Option<ContentVersion>, u64)>,
    /// Per view: 0 the camera, 1 + c shadow caster c.
    views: Vec<Option<GpuBuffer>>,
}

#[derive(Default)]
pub(super) struct PatchCull {
    bounds: Option<GpuComputePipeline>,
    args: Option<GpuComputePipeline>,
    cull: Option<GpuComputePipeline>,
    clamp: Option<GpuComputePipeline>,
    slots: Vec<Slot>,
}

/// Patches in a mesh of `index_count` indices.
fn patch_count(index_count: u32) -> u32 {
    index_count.div_ceil(PATCH_INDICES)
}

/// A dispatch of one workgroup per patch.
fn groups(patches: u32) -> [u32; 3] {
    [patches.min(ROW), patches.div_ceil(ROW), 1]
}

/// Whether a mesh of `index_count` indices is big enough to cull by patch.
pub(super) fn worth_culling(index_count: u32) -> bool {
    patch_count(index_count) >= MIN_PATCHES
}

impl PatchCull {
    /// Forget every slot's patches: the object list was rebuilt.
    pub(super) fn reset(&mut self, objects: usize) {
        self.slots = (0..objects).map(|_| Slot::default()).collect();
    }

    fn pipelines(&mut self, device: &GpuDevice) {
        if self.cull.is_none() {
            let make =
                |entry| device.create_compute_pipeline(SHADER, entry, "node.render_scene patches");
            self.bounds = Some(make("patch_bounds"));
            self.args = Some(make("patch_args"));
            self.cull = Some(make("cull_patches"));
            self.clamp = Some(make("clamp_args"));
        }
    }

    /// Work out slot `slot`'s patch boxes and cones when its mesh content
    /// changed since the last time, or when it is unknown.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn bounds(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        slot: usize,
        vertices: &GpuBuffer,
        indices: &GpuBuffer,
        index_count: u32,
        content: (Option<ContentVersion>, Option<ContentVersion>),
        rebuild_epoch: u64,
    ) {
        self.pipelines(device);
        let Some(entry) = self.slots.get_mut(slot) else {
            return;
        };
        let patches = patch_count(index_count);
        let key = (content.0, content.1, rebuild_epoch);
        let known = content.0.is_some() && content.1.is_some();
        let bytes = u64::from(patches) * PATCH_BYTES;
        let fits = entry.bounds.as_ref().is_some_and(|b| b.size >= bytes);
        if known && fits && entry.key == Some(key) {
            return;
        }
        if !fits {
            entry.bounds = Some(device.create_buffer(bytes, BufferUse::Gpu));
        }
        entry.key = Some(key);
        let uniforms = PatchArgs {
            clip_model: [[0.0; 4]; 4],
            eye: [0.0; 4],
            viewport: [0.0; 2],
            index_count,
            patch_count: patches,
            block: 0,
            clusters: 0,
            _pad0: 0,
            _pad1: 0,
        };
        encoder.dispatch_compute(
            self.bounds.as_ref().expect("built above"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: vertices,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: indices,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: entry.bounds.as_ref().expect("allocated above"),
                    offset: 0,
                },
            ],
            groups(patches),
            "node.render_scene patch bounds",
        );
    }

    /// Cull slot `slot`'s patches for view `view` (0 the camera, 1 + c
    /// shadow caster c) into that view's index buffer, which is returned,
    /// counting the kept indices into block `view.block`. The patches are
    /// `clusters` when the mesh has a hierarchy, else the runs of `indices`
    /// whose records [`Self::bounds`] wrote behind a buffer barrier. The
    /// draw that reads the result needs one more barrier.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn cull(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        slot: usize,
        index_slot: usize,
        indices: &GpuBuffer,
        index_count: u32,
        clusters: Option<&GpuBuffer>,
        view: &PatchView<'_>,
    ) -> Option<GpuBuffer> {
        self.pipelines(device);
        let entry = self.slots.get_mut(slot)?;
        let (records, source) = match clusters {
            Some(clusters) => (clusters, clusters),
            None => (entry.bounds.as_ref()?, indices),
        };
        if entry.views.len() <= index_slot {
            entry.views.resize_with(index_slot + 1, || None);
        }
        let bytes = u64::from(index_count) * 4;
        let out = &mut entry.views[index_slot];
        if out.as_ref().is_none_or(|b| b.size < bytes) {
            *out = Some(device.create_buffer(bytes, BufferUse::Gpu));
        }
        let out = out.clone().expect("allocated above");
        let clip_model = mat4_mul(view.clip, view.model);
        let eye = view
            .eye
            .and_then(|eye| {
                let inverse = super::matrix::mat4_inverse(*view.model)?;
                Some(transform_point(&inverse, eye))
            })
            .map_or([0.0; 4], |e| [e[0], e[1], e[2], 1.0]);
        let patches = patch_count(index_count);
        let uniforms = PatchArgs {
            clip_model,
            eye,
            viewport: [view.viewport[0] as f32, view.viewport[1] as f32],
            index_count,
            patch_count: patches,
            block: view.block as u32,
            clusters: u32::from(clusters.is_some()),
            _pad0: 0,
            _pad1: 0,
        };
        let uniform = GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(&uniforms),
        };
        encoder.dispatch_compute(
            self.args.as_ref().expect("built above"),
            &[
                uniform,
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: view.args,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.render_scene patch args",
        );
        encoder.compute_memory_barrier_buffers();
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: source,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: records,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: view.args,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: &out,
                offset: 0,
            },
        ];
        let cull = self.cull.as_ref().expect("built above");
        match clusters {
            // The array's header holds the dispatch for its cluster count.
            Some(clusters) => encoder.dispatch_compute_indirect(
                cull,
                &bindings,
                clusters,
                CLUSTER_DISPATCH_BYTES,
                "node.render_scene cull clusters",
            ),
            None => encoder.dispatch_compute(
                cull,
                &bindings,
                groups(patches),
                "node.render_scene cull patches",
            ),
        }
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(
            self.clamp.as_ref().expect("built above"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: view.args,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.render_scene clamp patch args",
        );
        Some(out)
    }
}

/// `a * b` for column-major `m[col][row]` matrices.
fn mat4_mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    std::array::from_fn(|col| {
        std::array::from_fn(|row| (0..4).map(|k| a[k][row] * b[col][k]).sum())
    })
}

fn transform_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| m[0][row] * p[0] + m[1][row] * p[1] + m[2][row] * p[2] + m[3][row])
}
