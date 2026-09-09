//! RS-B/RS-C (RAYTRACING_DESIGN.md section 15.3): the per-triangle emissive
//! light table — build, refit, and the alias-table sampler math. Split out of
//! `raytrace.rs` (BUG-xmsx driver split); see that file for the module map.

use super::{GiMaterial, RtObjectGeometry, effective_instance_slots};
use crate::GpuBuffer;
use crate::GpuDevice;

// RS-B (RAYTRACING_DESIGN.md section 15.3): per-triangle emissive light table
// cap — power-rank truncated. Mirrors the embedded MSL `MAX_RT_EMISSIVE_TRIANGLES`
// at the MSL source above (same manual-sync discipline as `MAX_RT_CASTERS`).
pub const MAX_RT_EMISSIVE_TRIANGLES: u32 = 4096;

/// RS-B: GPU-side emissive triangle entry — world-space positions of the
/// three vertices plus per-vertex UVs and the owning object index for
/// gi_materials/normal_sources lookups (RS-C). `packed_float3` discipline:
/// `[f32; 3]` + explicit pad (P0 section 5.1 kernel lesson).
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
    /// object_index there) and never reads it. Replaces the old `_pad_obj`
    /// word — size stays 80 bytes.
    pub descriptor_index: u32,
}

const _: () = assert!(std::mem::size_of::<EmissiveTriangleGpu>() == 80);

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

/// RS-B: CPU-side per-triangle storage for refit — local-space vertex
/// positions survive across transforms so `refit_emissive_table` can
/// recompute world-space positions from the new object transform.
/// consumed by `build_emissive_table`/`refit_emissive_table` via
/// `EmissiveLightTable.local_triangles`.
#[allow(dead_code, reason = "stored in EmissiveLightTable.local_triangles, consumed by build_/refit_emissive_table")]
#[derive(Clone, Debug)]
struct EmissiveTriangleCpu {
    v0_local: [f32; 3],
    v1_local: [f32; 3],
    v2_local: [f32; 3],
    uv0: [f32; 2],
    uv1: [f32; 2],
    uv2: [f32; 2],
    /// Index into the `objects` slice this triangle came from (for refit).
    object_index: u32,
}

/// RS-B (section 15.3): the resident emissive-geometry light table — GPU
/// buffers for the kernel's sampling step (RS-C) and CPU-side local-space
/// data for refit alongside the TLAS. Built once at accel-registration time
/// (D17 async discipline applies — table lands with the accel-ready flag);
/// refit re-transforms positions when object transforms change.
pub struct EmissiveLightTable {
    /// GPU buffer of [`EmissiveTriangleGpu`] entries (world-space positions
    /// in the D7 fast path, LOCAL-space in instanced mode — see
    /// [`Self::entries_are_local`]).
    pub triangles: GpuBuffer,
    /// GPU buffer of [`EmissiveAliasEntry`] entries.
    pub aliases: GpuBuffer,
    /// Valid entry count (0..=[`MAX_RT_EMISSIVE_TRIANGLES`]).
    pub entry_count: u32,
    /// Arithmetic mean of per-entry power (area × build-time emissive luma)
    /// — the firefly cap anchor in [`ShadowRayParams::emissive_table_mean_power`].
    pub mean_power: f32,
    /// RS-C: total area of all table entries. D8 derivation: in instanced
    /// mode the RIS weight uses each entry's TRUE world area computed
    /// in-kernel (exact under per-slot scales), so this aggregate is NOT
    /// consumed by the weight — it carries the LOCAL-space aggregate
    /// (Σ entry local areas, per-slot duplication included) for ABI
    /// stability and diagnostics. Fast path: Σ world areas (the weight
    /// normalizer, unchanged).
    pub total_area: f32,
    /// RT_INSTANCING_DESIGN.md D8: true when entries are LOCAL-space and
    /// the kernel composes world positions from the TLAS descriptor
    /// buffer (instanced mode). False = the D7 fast path (world entries).
    pub entries_are_local: bool,
    /// CPU-side local-space vertices for refit (one per entry, same order).
    /// Fast path only: `refit_emissive_table` re-transforms them into
    /// world space. Instanced mode: never consumed (local entries never
    /// change under transform animation — the kernel reads the composed
    /// world matrix from the descriptor buffer, which `refit_accel`
    /// refreshes), kept so the fast path shape stays one code path.
    local_triangles: Vec<EmissiveTriangleCpu>,
}

/// RS-B: build the emissive-triangle light table from `objects` (SAME slice
/// the accel was built from) and the parallel `gi_materials` array. Returns
/// `None` when no object has non-black emissive (zero triangles).
///
/// Algorithm: for each object whose `gi_materials[i].emissive` luma > 0,
/// iterate its indexed/non-indexed triangles via the CPU-mapped vertex
/// buffer; compute local-space area and power (area × emissive luma).
/// RT_INSTANCING_DESIGN.md D8: the candidate set spans (object, SLOT)
/// pairs — each emissive triangle is duplicated per effective instance
/// slot (same local power; the alias is a LOCAL-power proposal), and every
/// entry carries its slot's TLAS descriptor index. The entry cap spans the
/// expanded set: power-rank truncated to [`MAX_RT_EMISSIVE_TRIANGLES`].
///
/// D8 upload: instanced mode (any object wired — the SAME rule as the
/// accel's mode trigger) keeps entries LOCAL-space; the kernel composes
/// world positions from the TLAS descriptor buffer, so transform animation
/// reaches the light table through the accel's own refit, with no CPU
/// rewrite. The D7 fast path uploads world-space entries (today's bytes).
///
/// D8 RIS-weighting derivation (the kernel side is the arbiter proof's
/// oracle): the alias proposal is p(entry) ∝ local_power, duplicated per
/// slot, so the proposal mass of an object's triangles scales with its
/// slot count — a copy is proposed as often as it emits. The weight must
/// then carry the sampled entry's TRUE world area: for a uniform scene
/// (one luma, rigid slots) E[contrib] = Σ_slots Σ_tris (A_local·lum/W)·lum·
/// cosθ·cos_emit·A_world(t,s)/dist², which equals the true direct light
/// only with the per-entry world area in the weight — a global scalar
/// normalizer would bake the slot scale into the bias. Hence the kernel
/// computes ½|(w1−w0)×(w2−w0)| from the composed world edges per sample.
/// `total_area` accordingly keeps its fast-path meaning (Σ world areas,
/// the global weight normalizer) and becomes the LOCAL aggregate in
/// instanced mode, where the weight no longer consumes it. `mean_power`
/// stays the mean over the FINAL expanded candidate set — the firefly
/// anchor.
///
/// The caller owns the returned table (pass to `refit_emissive_table` on
/// transform change; keep alive as long as the accel lives).
pub fn build_emissive_table(
    device: &GpuDevice,
    objects: &[RtObjectGeometry],
    gi_materials: &[GiMaterial],
) -> Option<EmissiveLightTable> {
    if gi_materials.is_empty() { return None; }
    assert_eq!(objects.len(), gi_materials.len(), "RT emissive table requires one material row per RT object");
    // D8: same mode rule as `build_accel` (any WIRED object, P1.5: a wired
    // 1-capacity buffer is GPU-path too) — the table's slot set must equal
    // the accel's slot set exactly, or entries would name descriptors that
    // don't exist.
    let instanced = objects.iter().any(|o| o.instances_addr != 0);

    // Phase 1: collect per-(triangle, slot) candidates (local vertices,
    // object index, descriptor slot index, local power).
    struct Candidate {
        v0: [f32; 3],
        v1: [f32; 3],
        v2: [f32; 3],
        uv0: [f32; 2],
        uv1: [f32; 2],
        uv2: [f32; 2],
        obj_index: u32,
        /// D8: the TLAS descriptor slot of this entry's copy (the kernel's
        /// world-composition index). Fast path: equals the object index.
        desc_index: u32,
        power: f32,
    }
    let mut candidates: Vec<Candidate> = Vec::new();

    // Slot bases under the SAME object-major addressing the descriptor
    // kernel and `write_instance_obj_params` use — one running sum.
    let mut slot_base = 0u32;

    for (oi, obj) in objects.iter().enumerate() {
        let obj_slots = effective_instance_slots(obj);
        let object_slot_base = slot_base;
        slot_base = slot_base.checked_add(obj_slots).expect("RT emissive slot base overflow");
        let emissive_luma = luma(gi_materials[oi].emissive);
        if emissive_luma <= 0.0 {
            continue;
        }
        let Some(ptr) = obj.vertex_buffer.mapped_ptr() else {
            log::warn!(
                "RT emissive table: object {} vertex buffer is not CPU-mapped — \
                 its emissive triangles are absent from the light table",
                oi
            );
            continue;
        };
        let stride = obj.vertex_stride as usize;
        let offset = obj.vertex_offset as usize;
        let tri_count = obj.triangle_count as usize;

        // Read position from vertex i (offset 0 = position for MeshVertex convention).
        let pos_at = |vi: usize| -> [f32; 3] {
            let byte_offset = offset + vi * stride;
            unsafe {
                let p: *const [f32; 3] = ptr.add(byte_offset) as *const [f32; 3];
                *p
            }
        };

        // Compute per-triangle local-space area and power.
        let index_at = |vi: usize| -> u32 {
            if let Some(ib) = obj.index_buffer {
                if let Some(ib_ptr) = ib.mapped_ptr() {
                    unsafe { *(ib_ptr.add(vi * 4) as *const u32) }
                } else {
                    vi as u32
                }
            } else {
                vi as u32
            }
        };

        for ti in 0..tri_count {
            let i0 = index_at(ti * 3) as usize;
            let i1 = index_at(ti * 3 + 1) as usize;
            let i2 = index_at(ti * 3 + 2) as usize;
            let v0 = pos_at(i0);
            let v1 = pos_at(i1);
            let v2 = pos_at(i2);

            let area = triangle_area(v0, v1, v2);
            if area <= 0.0 {
                continue;
            }
            let power = area * emissive_luma;
            // RS-C: also capture per-vertex UVs for emissive-map sampling.
            let uv_offset = obj.uv_offset as usize;
            let uv_at = |vi: usize| -> [f32; 2] {
                let byte_offset = offset + vi * stride + uv_offset;
                unsafe { *(ptr.add(byte_offset) as *const [f32; 2]) }
            };
            let uv0 = uv_at(i0);
            let uv1 = uv_at(i1);
            let uv2 = uv_at(i2);
            // D8: one candidate per (triangle, slot) — the local power is
            // identical across an object's slots (rigid copies), so the
            // duplication lives in the proposal mass, not the power value.
            for s in 0..obj_slots {
                candidates.push(Candidate {
                    v0,
                    v1,
                    v2,
                    uv0,
                    uv1,
                    uv2,
                    obj_index: oi as u32,
                    desc_index: object_slot_base + s,
                    power,
                });
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // Phase 2: power-rank truncate to MAX_RT_EMISSIVE_TRIANGLES.
    if candidates.len() > MAX_RT_EMISSIVE_TRIANGLES as usize {
        candidates.select_nth_unstable_by(
            MAX_RT_EMISSIVE_TRIANGLES as usize,
            |a, b| b.power.partial_cmp(&a.power).unwrap_or(std::cmp::Ordering::Equal),
        );
        candidates.truncate(MAX_RT_EMISSIVE_TRIANGLES as usize);
    }

    let entry_count = candidates.len() as u32;
    let total_power: f32 = candidates.iter().map(|c| c.power).sum();
    let mean_power = total_power / entry_count as f32;
    // RS-C: total world-space area computed below during upload.
    let mut total_area: f32 = 0.0;

    // Phase 3: build alias table from the power distribution.
    let weights: Vec<f32> = candidates.iter().map(|c| c.power).collect();
    let (probs, aliases) = build_alias_table(&weights);

    // Phase 4: upload to GPU buffers.
    let triangles_bytes = (entry_count as usize * std::mem::size_of::<EmissiveTriangleGpu>()) as u64;
    let aliases_bytes = (entry_count as usize * std::mem::size_of::<EmissiveAliasEntry>()) as u64;
    let tri_buf = device.create_buffer_shared(triangles_bytes.max(1));
    let alias_buf = device.create_buffer_shared(aliases_bytes.max(1));

    {
        let tri_ptr = tri_buf
            .mapped_ptr()
            .expect("emissive triangle buffer must be shared");
        let alias_ptr = alias_buf
            .mapped_ptr()
            .expect("emissive alias buffer must be shared");
        let mut local_triangles: Vec<EmissiveTriangleCpu> =
            Vec::with_capacity(entry_count as usize);
        let model = |oi: u32| -> [[f32; 4]; 4] {
            objects.get(oi as usize).map(|o| o.transform).unwrap_or([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ])
        };

        for (i, c) in candidates.iter().enumerate() {
            // D8: instanced entries stay LOCAL-space (the kernel composes
            // world from the descriptor buffer); the fast path uploads
            // world-space positions exactly as before.
            let (e0, e1, e2) = if instanced {
                total_area += triangle_area(c.v0, c.v1, c.v2);
                (c.v0, c.v1, c.v2)
            } else {
                let m = model(c.obj_index);
                let w0 = transform_point(&m, c.v0);
                let w1 = transform_point(&m, c.v1);
                let w2 = transform_point(&m, c.v2);
                total_area += triangle_area(w0, w1, w2);
                (w0, w1, w2)
            };

            unsafe {
                let tri_dst = tri_ptr.add(i * std::mem::size_of::<EmissiveTriangleGpu>())
                    as *mut EmissiveTriangleGpu;
                std::ptr::write_unaligned(
                    tri_dst,
                    EmissiveTriangleGpu {
                        v0: e0,
                        _pad0: 0.0,
                        v1: e1,
                        _pad1: 0.0,
                        v2: e2,
                        _pad2: 0.0,
                        uv0: c.uv0,
                        uv1: c.uv1,
                        uv2: c.uv2,
                        object_index: c.obj_index,
                        descriptor_index: c.desc_index,
                    },
                );

                let alias_dst = alias_ptr.add(i * std::mem::size_of::<EmissiveAliasEntry>())
                    as *mut EmissiveAliasEntry;
                std::ptr::write_unaligned(
                    alias_dst,
                    EmissiveAliasEntry {
                        prob: probs[i],
                        alias: aliases[i],
                    },
                );
            }

            local_triangles.push(EmissiveTriangleCpu {
                v0_local: c.v0,
                v1_local: c.v1,
                v2_local: c.v2,
                uv0: c.uv0,
                uv1: c.uv1,
                uv2: c.uv2,
                object_index: c.obj_index,
            });
        }

        Some(EmissiveLightTable {
            triangles: tri_buf,
            aliases: alias_buf,
            entry_count,
            mean_power,
            total_area,
            entries_are_local: instanced,
            local_triangles,
        })
    }
}

/// RS-B: refit the emissive table's world-space positions from the stored
/// local-space vertices and the objects' current transforms. Same call-site
/// discipline as [`refit_accel`] — called when topology is stable and
/// transforms changed.
///
/// RT_INSTANCING_DESIGN.md D8: INSTANCED MODE IS A NO-OP — the entries stay
/// LOCAL-space forever, and the kernel composes their world positions from
/// the TLAS descriptor buffer, which `refit_accel` refreshes on the same
/// cadence. A transform animation therefore reaches the light table
/// through the accel's own refit with no CPU rewrite here. (This CPU
/// re-transform stays for the D7 fast path, whose entries are world-space.)
pub fn refit_emissive_table(table: &EmissiveLightTable, objects: &[RtObjectGeometry]) {
    if table.entries_are_local {
        return;
    }
    let Some(ptr) = table.triangles.mapped_ptr() else {
        return;
    };
    for (i, local) in table.local_triangles.iter().enumerate() {
        let m = objects
            .get(local.object_index as usize)
            .map(|o| o.transform)
            .unwrap_or([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]);
        let w0 = transform_point(&m, local.v0_local);
        let w1 = transform_point(&m, local.v1_local);
        let w2 = transform_point(&m, local.v2_local);
        unsafe {
            let dst = ptr.add(i * std::mem::size_of::<EmissiveTriangleGpu>())
                as *mut EmissiveTriangleGpu;
            (*dst).v0 = w0;
            (*dst).v1 = w1;
            (*dst).v2 = w2;
        }
    }
}

/// Luminance of a linear-HDR RGB triple (Rec.709 weights, same convention
/// the kernel's `luma()` MSL helper uses).
fn luma(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// Area of a triangle from its three vertices (half the cross-product
/// magnitude of two edge vectors).
fn triangle_area(v0: [f32; 3], v1: [f32; 3], v2: [f32; 3]) -> f32 {
    let e1 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
    let e2 = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
    let cross = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    let mag2 = cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2];
    if mag2 <= 0.0 {
        return 0.0;
    }
    0.5 * mag2.sqrt()
}

/// Transform a point by a column-major 4×4 matrix (position only — w=1,
/// no projective divide). Same convention as `render_scene.wgsl`'s
/// `(M * vec4(p, 1.0)).xyz`.
fn transform_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}

/// Build a discrete alias table from a slice of non-negative weights (the
/// "alias method" — Walker 1974 / Vose 1991, O(n)). Returns `(probs, aliases)`
/// where `probs[i]` is the self-selection probability and `aliases[i]` is the
/// alternate index.
///
/// At sample time: draw `u ~ U(0,1)`, `j = floor(u * n)`, `u' = u * n - j`;
/// if `u' < probs[j]` pick `j`, else pick `aliases[j]`. `probs[j]` is stored
/// as `prob * n` (the scaled probability) so the comparison is direct.
fn build_alias_table(weights: &[f32]) -> (Vec<f32>, Vec<u32>) {
    let n = weights.len();
    if n == 0 {
        return (vec![], vec![]);
    }
    let total: f32 = weights.iter().sum();
    if total <= 0.0 {
        // Degenerate: all zero weights — uniform probabilities, alias to self.
        return (vec![1.0; n], (0..n as u32).collect());
    }
    let inv_total = 1.0 / total;
    let n_f = n as f32;
    let avg = 1.0 / n_f;

    let mut probs: Vec<f32> = weights.iter().map(|w| w * inv_total).collect();
    let mut aliases: Vec<u32> = (0..n as u32).collect();

    let mut small: Vec<usize> = Vec::new();
    let mut large: Vec<usize> = Vec::new();

    for (i, &p) in probs.iter().enumerate() {
        if p < avg {
            small.push(i);
        } else {
            large.push(i);
        }
    }

    while let (Some(&s), Some(&l)) = (small.last(), large.last()) {
        probs[s] *= n_f; // scaled probability: p * n
        aliases[s] = l as u32;
        probs[l] = (probs[l] + probs[s] / n_f) - avg; // remaining excess
        small.pop();
        if probs[l] < avg {
            large.pop();
            small.push(l);
        }
    }

    // Remaining entries (rounding) — set prob=1 (always self).
    for &s in &small {
        probs[s] = 1.0;
        aliases[s] = s as u32;
    }
    for &l in &large {
        probs[l] = 1.0;
        aliases[l] = l as u32;
    }

    (probs, aliases)
}
