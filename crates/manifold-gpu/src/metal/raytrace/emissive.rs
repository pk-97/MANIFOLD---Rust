//! RS-B/RS-C (RAYTRACING_DESIGN.md section 15.3): the per-triangle emissive
//! light table. SCENE_MODIFIER_RT_DESIGN.md §5.1 (P4a) replaced the CPU
//! build/refit path with `encode_emissive_table` — GPU candidate enumerate →
//! radix sort → gather → alias → stats, driven only by the shared AS update
//! path, reading CURRENT vertex/index data through raw GPU addresses so
//! deformed meshes reach the light table with no CPU geometry reads.

use super::{GiMaterial, RtAccel, RtAccelError, RtObjectGeometry, effective_instance_slots};
use super::tracer::MetalShadowRayTracer;
use crate::metal::encoder::GpuEncoder;
use crate::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

// RS-B (RAYTRACING_DESIGN.md section 15.3): per-triangle emissive light table
// cap — power-rank truncated. Mirrors the embedded MSL `MAX_RT_EMISSIVE_TRIANGLES`
// at the MSL source above (same manual-sync discipline as `MAX_RT_CASTERS`).
pub const MAX_RT_EMISSIVE_TRIANGLES: u32 = 4096;

/// P4a radix-sort tile size — mirrors the MSL `EM_TILE` (same manual-sync
/// discipline). One thread per tile per hist/scatter dispatch.
const EM_TILE: u32 = 1024;

/// §5.1: the table's GPU-side stats — the trace kernel's RIS sampler and the
/// firefly clamp read this 16-byte buffer directly (buffer(9) / buffer(2)).
/// Replaces the deleted `ShadowRayParams` CPU fields. Mirrors the MSL
/// `EmissiveTableStats` field-for-field.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct EmissiveTableStats {
    pub entry_count: u32,
    pub entries_are_local: u32,
    pub mean_power: f32,
    pub total_area: f32,
}

const _: () = assert!(std::mem::size_of::<EmissiveTableStats>() == 16);

/// CPU mirror of the MSL `EmissivePrepHeader` — CPU-written per refresh
/// (shared buffer); `valid_count` is zeroed by the CPU at encode time and
/// atomically counted by the enumerate kernel.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct EmissivePrepHeader {
    active_count: u32,
    valid_count: u32,
    entries_are_local: u32,
    object_count: u32,
}

const _: () = assert!(std::mem::size_of::<EmissivePrepHeader>() == 16);

/// CPU mirror of the MSL `EmissiveObjParams` — one row per EMISSIVE object
/// (luma > 0), CPU-compacted per refresh. Carries only CPU-known metadata;
/// vertex contents are never read on this path.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct EmissiveObjParams {
    vertex_base_addr: u64,
    index_base_addr: u64,
    vertex_stride: u32,
    vertex_offset: u32,
    uv_offset: u32,
    tri_count: u32,
    candidate_base: u32,
    slot_base: u32,
    object_index: u32,
    luma: f32,
    /// P4b (§5.2): bindless address of the object's per-vertex appearance
    /// weights (0 = unwired → corner weights 1.0). The gather bakes the
    /// three corner weights into each table entry; the GAIN stays out of
    /// this table (the trace kernel reads it from the normal-source row at
    /// sample time, so a fractional-to-fractional gain change needs no
    /// emissive refresh).
    appearance_weights_addr: u64,
}

const _: () = assert!(std::mem::size_of::<EmissiveObjParams>() == 56);

/// RS-B: GPU-side emissive triangle entry — world-space positions of the
/// three vertices plus per-vertex UVs and the owning object index for
/// gi_materials/normal_sources lookups (RS-C). `packed_float3` discipline:
/// `[f32; 3]` + explicit pad (P0 section 5.1 kernel lesson). P4a: written by
/// the `emissive_gather` kernel, never by the CPU. P4b (§5.2): grew the
/// per-corner APPEARANCE WEIGHTS (`w0/w1/w2`, 1.0 when unwired) — explicit
/// emitter samples have not passed the hit test, so the trace kernel
/// multiplies `coverage * brightness` at the sampled barycentrics from
/// these corners and the normal-source row's current gain.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EmissiveTriangleGpu {
    pub v0: [f32; 3],
    _pad0: f32,
    pub v1: [f32; 3],
    _pad1: f32,
    pub v2: [f32; 3],
    _pad2: f32,
    pub uv0: [f32; 2],
    pub uv1: [f32; 2],
    pub uv2: [f32; 2],
    pub object_index: u32,
    /// RT_INSTANCING_DESIGN.md D8: the TLAS descriptor slot of THIS entry's
    /// copy. In instanced mode the entries stay LOCAL-space and the kernel
    /// composes world positions from `emissive_descriptors[descriptor_index]`;
    /// the D7 fast path sets this to the object index (descriptor_index ==
    /// object_index there) and never reads it.
    pub descriptor_index: u32,
    /// P4b: per-corner appearance weights (vertex-order, post index
    /// resolution), 1.0 when the object has no weights wired.
    pub w0: f32,
    pub w1: f32,
    pub w2: f32,
    _pad3: f32,
}

const _: () = assert!(std::mem::size_of::<EmissiveTriangleGpu>() == 96);

/// RS-B: GPU-side alias-table entry — `prob` is the probability of selecting
/// the entry's own triangle; when the draw fails the self-probability, `alias`
/// names the alternative entry index.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EmissiveAliasEntry {
    pub prob: f32,
    pub alias: u32,
}

const _: () = assert!(std::mem::size_of::<EmissiveAliasEntry>() == 8);

/// §5.1: the resident emissive-geometry light table — GPU buffers written by
/// the preparation kernels. Fixed-size (the 4096-entry cap), allocated at
/// preparation so an emission zero→positive transition needs no new
/// allocation; a scene that never emits simply holds a zero-stats table.
pub struct EmissiveLightTable {
    /// GPU buffer of [`EmissiveTriangleGpu`] entries (world-space positions
    /// in the D7 fast path, LOCAL-space in instanced mode), gather-written.
    pub triangles: GpuBuffer,
    /// GPU buffer of [`EmissiveAliasEntry`] entries, alias-kernel-written.
    pub aliases: GpuBuffer,
    /// The 16-byte [`EmissiveTableStats`] buffer — stats-kernel-written on
    /// every refresh (including the zero-emission refresh); the ONLY source
    /// of count/mean/area/local-flag for the trace and firefly kernels.
    pub stats: GpuBuffer,
    /// Per-entry (power, area) scratch — gather-written, consumed by the
    /// stats and alias kernels (alias overwrites `.x` with scaled probs).
    pub(crate) entry_power: GpuBuffer,
    /// Alias construction's preallocated small/large stacks (2 × cap u32).
    pub(crate) alias_stacks: GpuBuffer,
}

impl EmissiveLightTable {
    /// Allocate the fixed-size table (preparation-time). Triangles/aliases/
    /// stats are SHARED buffers — the gpu_proofs readback path maps them
    /// after commit+wait (same discipline the deleted CPU-built table had).
    pub(crate) fn new(device: &GpuDevice) -> Self {
        let cap = u64::from(MAX_RT_EMISSIVE_TRIANGLES);
        Self {
            triangles: device.create_buffer_shared(cap * 96),
            aliases: device.create_buffer_shared(cap * 8),
            stats: device.create_buffer_shared(16),
            entry_power: device.create_buffer(cap * 8),
            alias_stacks: device.create_buffer(cap * 2 * 4),
        }
    }
}

/// §5.1 candidate/sort workspace, sized at preparation over ALL objects
/// (emission can go zero→positive without a topology edit — the capacity
/// must already cover the newly emissive object's tuples).
pub(crate) struct EmissiveScratch {
    /// Ping-pong candidate records (16 B each): key/obj/desc/tri.
    pub(crate) candidates: [GpuBuffer; 2],
    /// Radix per-tile digit histograms / scanned offsets: tiles × 256 u32.
    pub(crate) hist: GpuBuffer,
    /// Shared CPU-written header + object table (16 + max_objects × 48 B).
    pub(crate) obj_params: GpuBuffer,
    capacity: u32,
}

impl EmissiveScratch {
    pub(crate) fn new(device: &GpuDevice, capacity: u32, max_objects: usize) -> Self {
        let cap = u64::from(capacity.max(1));
        let tiles = u64::from(capacity.div_ceil(EM_TILE).max(1));
        Self {
            candidates: [device.create_buffer(cap * 16), device.create_buffer(cap * 16)],
            hist: device.create_buffer(tiles * 256 * 4),
            obj_params: device.create_buffer_shared(
                16 + (max_objects.max(1) * std::mem::size_of::<EmissiveObjParams>()) as u64,
            ),
            capacity,
        }
    }

    /// Byte size of the candidate/sort workspace for the plan's admission
    /// accounting (excludes the per-object header table).
    pub(crate) fn bytes_for(capacity: u32) -> u64 {
        let cap = u64::from(capacity.max(1));
        let tiles = u64::from(capacity.div_ceil(EM_TILE).max(1));
        cap * 16 * 2 + tiles * 256 * 4
    }
}

/// What one refresh encodes (§5.1 refresh tiers): `Full` = enumerate → sort
/// → gather → stats → alias (geometry/material/UV change, and every
/// instanced-mode update — descriptor bases are re-enumerated);
/// `GatherOnly` = fast-path transform-only update (local powers unchanged —
/// ranking and alias are stable, world positions and areas refresh).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmissiveRefresh {
    Full,
    GatherOnly,
}

/// §5.1: GPU emissive preparation, called only by the shared AS update path
/// (the tracer's `encode_accel_update` method, right after the AS encoder
/// closes on the same command buffer — the kernels then read the CURRENT
/// vertex data and the instance descriptors this frame's update wrote).
///
/// Design amendment (P4a landing): `materials` is threaded in addition to
/// §5.1's sketched signature — candidate compaction over emissive objects
/// and the material-factor luma are CPU material metadata (never geometry),
/// and threading them keeps the kernels independent of the gi_materials
/// table layout (the same class of omission as §4.1's `device`, recorded
/// in 4e949fc45).
pub(crate) fn encode_emissive_table(
    tracer: &MetalShadowRayTracer,
    device: &GpuDevice,
    encoder: &mut GpuEncoder,
    accel: &mut RtAccel,
    objects: &[RtObjectGeometry<'_>],
    materials: &[GiMaterial],
    refresh: EmissiveRefresh,
) -> Result<(), RtAccelError> {
    if accel.emissive_scratch.is_none() {
        return Err(RtAccelError::NeedsPreparation);
    }
    if accel.emissive_table.is_none() {
        accel.emissive_table = Some(EmissiveLightTable::new(device));
    }
    let instanced = accel.instanced;

    // CPU metadata compaction: one row per emissive object (luma > 0), with
    // running candidate/slot bases — the SAME object-major slot addressing
    // the descriptor kernel and `write_instance_obj_params` use. An EMPTY
    // materials slice means "no emissive anywhere" (the deleted CPU path's
    // `gi_materials.is_empty() → None`); a non-empty slice must name one
    // row per object, same contract as before.
    if !materials.is_empty() {
        assert_eq!(
            objects.len(),
            materials.len(),
            "RT emissive table requires one material row per RT object"
        );
    }
    let mut active_count = 0u32;
    let mut slot_base = 0u32;
    let mut rows: Vec<EmissiveObjParams> = Vec::new();
    for (oi, obj) in objects.iter().enumerate() {
        let obj_slots = effective_instance_slots(obj);
        let object_slot_base = slot_base;
        slot_base = slot_base
            .checked_add(obj_slots)
            .ok_or(RtAccelError::Encode("RT emissive slot base overflow"))?;
        let obj_luma = if materials.is_empty() { 0.0 } else { luma(materials[oi].emissive) };
        if obj_luma <= 0.0 {
            continue;
        }
        let candidate_base = active_count;
        active_count = active_count
            .checked_add(
                obj_slots
                    .checked_mul(obj.triangle_count)
                    .ok_or(RtAccelError::Encode("RT emissive candidate count overflow"))?,
            )
            .ok_or(RtAccelError::Encode("RT emissive candidate count overflow"))?;
        rows.push(EmissiveObjParams {
            vertex_base_addr: obj.vertex_buffer.gpu_address(),
            index_base_addr: obj.index_buffer.map_or(0, GpuBuffer::gpu_address),
            vertex_stride: obj.vertex_stride,
            vertex_offset: obj.vertex_offset,
            uv_offset: obj.uv_offset,
            tri_count: obj.triangle_count,
            candidate_base,
            slot_base: object_slot_base,
            object_index: oi as u32,
            luma: obj_luma,
            appearance_weights_addr: obj.appearance_weights.map_or(0, GpuBuffer::gpu_address),
        });
    }

    // Raw-address vertex/index/weights reads are the BUG-84fv indirect-
    // reach class: every buffer the kernels can touch is useResource-
    // declared on the dispatch (the accel's geometry set carries the
    // objects' vertex/index/weights buffers; declared again here so the
    // list names this kernel's own reads).
    let mut indirect_reads: Vec<&GpuBuffer> = Vec::with_capacity(objects.len() * 3);
    for obj in objects {
        indirect_reads.push(obj.vertex_buffer);
        if let Some(ib) = obj.index_buffer {
            indirect_reads.push(ib);
        }
        if let Some(w) = obj.appearance_weights {
            indirect_reads.push(w);
        }
    }

    {
        let scratch = accel.emissive_scratch.as_ref().expect("checked above");
        if active_count > scratch.capacity {
            // Capacity is sized at preparation over ALL objects — only a
            // preparation that predates this object set can be short.
            return Err(RtAccelError::NeedsPreparation);
        }
        // Header + object table, written at encode time (shared buffer; the
        // GPU reads it on this command buffer, after this CPU write).
        // GatherOnly preserves the GPU-counted valid_count: no enumerate
        // runs on that tier and the candidate set is unchanged, so the last
        // Full refresh's count is still correct (transform-only update).
        let ptr = scratch
            .obj_params
            .mapped_ptr()
            .expect("RT emissive object-params buffer must be CPU-mapped");
        let preserved_valid = if refresh == EmissiveRefresh::GatherOnly {
            unsafe { (ptr.add(4) as *const u32).read_unaligned() }
        } else {
            0
        };
        let header = EmissivePrepHeader {
            active_count,
            valid_count: preserved_valid,
            entries_are_local: instanced as u32,
            object_count: rows.len() as u32,
        };
        unsafe {
            std::ptr::write_unaligned(ptr as *mut EmissivePrepHeader, header);
            let row_ptr = ptr.add(16) as *mut EmissiveObjParams;
            for (i, row) in rows.iter().enumerate() {
                std::ptr::write_unaligned(row_ptr.add(i), *row);
            }
        }
    }

    let pipes = tracer.emissive_pipelines();
    let scratch = accel.emissive_scratch.as_ref().expect("checked above");
    let table = accel.emissive_table.as_ref().expect("ensured above");
    let full = refresh == EmissiveRefresh::Full;

    if full && active_count > 0 {
        // Enumerate: one thread per (object, slot, triangle) tuple.
        dispatch_emissive(
            encoder,
            &pipes.enumerate,
            accel,
            &[
                GpuBinding::Buffer { binding: 0, buffer: &scratch.obj_params, offset: 0 },
                GpuBinding::Buffer { binding: 1, buffer: &scratch.obj_params, offset: 16 },
                GpuBinding::Buffer { binding: 2, buffer: &scratch.candidates[0], offset: 0 },
            ],
            &indirect_reads,
            active_count,
            "RT emissive enumerate",
        );
        // Radix sort: 4 passes × 8 bits, ping-pong; the even pass count
        // lands the sorted order back in candidates[0].
        let num_tiles = active_count.div_ceil(EM_TILE);
        for pass in 0..4u32 {
            let shift = pass * 8;
            let (src, dst) = if pass % 2 == 0 {
                (&scratch.candidates[0], &scratch.candidates[1])
            } else {
                (&scratch.candidates[1], &scratch.candidates[0])
            };
            let shift_bytes = shift.to_le_bytes();
            dispatch_emissive(
                encoder,
                &pipes.hist,
                accel,
                &[
                    GpuBinding::Buffer { binding: 0, buffer: &scratch.obj_params, offset: 0 },
                    GpuBinding::Buffer { binding: 1, buffer: src, offset: 0 },
                    GpuBinding::Buffer { binding: 2, buffer: &scratch.hist, offset: 0 },
                    GpuBinding::Bytes { binding: 3, data: &shift_bytes },
                ],
                &indirect_reads,
                num_tiles,
                "RT emissive hist",
            );
            dispatch_emissive(
                encoder,
                &pipes.scan,
                accel,
                &[
                    GpuBinding::Buffer { binding: 0, buffer: &scratch.obj_params, offset: 0 },
                    GpuBinding::Buffer { binding: 1, buffer: &scratch.hist, offset: 0 },
                ],
                &indirect_reads,
                1,
                "RT emissive scan",
            );
            dispatch_emissive(
                encoder,
                &pipes.scatter,
                accel,
                &[
                    GpuBinding::Buffer { binding: 0, buffer: &scratch.obj_params, offset: 0 },
                    GpuBinding::Buffer { binding: 1, buffer: src, offset: 0 },
                    GpuBinding::Buffer { binding: 2, buffer: dst, offset: 0 },
                    GpuBinding::Buffer { binding: 3, buffer: &scratch.hist, offset: 0 },
                    GpuBinding::Bytes { binding: 4, data: &shift_bytes },
                ],
                &indirect_reads,
                num_tiles,
                "RT emissive scatter",
            );
        }
    }
    if active_count > 0 {
        // Gather: 4096 threads, the kernel early-outs past entry_count
        // (the CPU does not know valid_count — it is GPU-counted).
        dispatch_emissive(
            encoder,
            &pipes.gather,
            accel,
            &[
                GpuBinding::Buffer { binding: 0, buffer: &scratch.obj_params, offset: 0 },
                GpuBinding::Buffer { binding: 1, buffer: &scratch.obj_params, offset: 16 },
                GpuBinding::Buffer { binding: 2, buffer: &scratch.candidates[0], offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &table.triangles, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: &table.entry_power, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: &accel.instance_buffer, offset: 0 },
            ],
            &indirect_reads,
            MAX_RT_EMISSIVE_TRIANGLES,
            "RT emissive gather",
        );
    }
    // Stats: always dispatched on a refresh — this is what keeps the
    // zero-emission stats valid (count 0, zero mean/area).
    dispatch_emissive(
        encoder,
        &pipes.stats,
        accel,
        &[
            GpuBinding::Buffer { binding: 0, buffer: &scratch.obj_params, offset: 0 },
            GpuBinding::Buffer { binding: 1, buffer: &table.entry_power, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &table.stats, offset: 0 },
        ],
        &indirect_reads,
        1,
        "RT emissive stats",
    );
    if full && active_count > 0 {
        dispatch_emissive(
            encoder,
            &pipes.alias,
            accel,
            &[
                GpuBinding::Buffer { binding: 0, buffer: &scratch.obj_params, offset: 0 },
                GpuBinding::Buffer { binding: 1, buffer: &table.entry_power, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &table.aliases, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &table.alias_stacks, offset: 0 },
            ],
            &indirect_reads,
            1,
            "RT emissive alias",
        );
    }
    Ok(())
}

/// One emissive-preparation dispatch with the geometry-buffer useResource
/// coverage (BUG-84fv class: the kernels reach vertex/index data through
/// raw GPU addresses no binding declares). `u32::MAX` as the accel binding
/// names no acceleration structure — these kernels bind none.
fn dispatch_emissive(
    encoder: &mut GpuEncoder,
    pipeline: &GpuComputePipeline,
    accel: &RtAccel,
    bindings: &[GpuBinding<'_>],
    indirect_reads: &[&GpuBuffer],
    threads: u32,
    label: &str,
) {
    let wg = pipeline.workgroup_size;
    let per_group = (wg[0] * wg[1] * wg[2]).max(1);
    encoder.dispatch_compute_with_accel(
        pipeline,
        u32::MAX,
        accel,
        bindings,
        indirect_reads.iter().copied(),
        None,
        [threads.div_ceil(per_group), 1, 1],
        label,
    );
}

/// Luminance of a linear-HDR RGB triple (Rec.709 weights, same convention
/// the kernel's `luma()` MSL helper uses).
fn luma(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}
