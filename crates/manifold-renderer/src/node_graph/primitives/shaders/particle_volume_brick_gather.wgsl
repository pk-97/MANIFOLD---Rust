// node.particle_volume pass 1 (active bricks), cooperative schedule.
// docs/PARTICLE_VOLUME_BRICK_GATHER_DESIGN.md section 3 (The cooperative kernel).
//
// One group is one half brick, decoded like liquid_brick_map. The group stages
// the blobs of its union bin window into threadgroup memory chunk by chunk and
// computes each staged blob's support box once. Each lane still visits exactly
// its own window's (bin, k) sequence in the generated kernel's order (z, y, x,
// k ascending) and calls the same helpers, so its output is bitwise the
// generated kernel's. Composed at pipeline creation with
// liquid_bricks_common.wgsl and particle_volume_common.wgsl. The declarations
// below mirror the generated kernel's ABI (pinned by a reflection test).

struct Element {
    center_radius: vec4<f32>,
    shape_diag: vec4<f32>,
    shape_off: vec4<f32>,
}

struct Element2 {
    start: u32,
    count: u32,
}

struct Params {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    resolution_scale: i32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    band_extra: f32,
    brick_pass: u32,
    interior_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_blobs: array<Element>;
@group(0) @binding(2) var<storage, read> buf_cell_ranges: array<Element2>;
@group(0) @binding(3) var<storage, read> buf_solid: array<f32>;
@group(0) @binding(4) var<storage, read> buf_bricks: array<u32>;
@group(0) @binding(5) var<storage, read> buf_interior: array<f32>;
@group(0) @binding(6) var<storage, read> buf_bounds: array<f32>;
@group(0) @binding(7) var<storage, read_write> buf_levelset: array<f32>;

const CHUNK: u32 = 128u;
const EMPTY: u32 = 0xffffffffu;

// Inclusive scan of the run's bin counts.
var<workgroup> wg_scan: array<u32, 256>;
// Exclusive prefix of the run, prefix[256] = run total. Before the first run
// it is the union reduction's scratch.
var<workgroup> wg_prefix: array<u32, 257>;
// Staged in the blob's own record shape so the term sees the generated
// kernel's operands in the generated kernel's form.
var<workgroup> wg_blob: array<Element, CHUNK>;
// first xyz then last xyz per staged blob; unpacked so NaN/inf take the
// generated kernel's float-to-int conversion.
var<workgroup> wg_box: array<i32, CHUNK * 6u>;
var<workgroup> wg_union: array<i32, 6>;

// Reduces `a` into union slot `sa` and `b` into `sb` over all lanes: max where
// the flag is set, else min. i32 values in u32 storage. Every lane calls it
// in uniform control flow; it ends with a barrier.
fn reduce_pair(t: u32, a: i32, b: i32, sa: u32, sb: u32, max_a: bool, max_b: bool) {
    wg_scan[t] = bitcast<u32>(a);
    wg_prefix[t] = bitcast<u32>(b);
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride = stride >> 1u) {
        if t < stride {
            let a0 = bitcast<i32>(wg_scan[t]);
            let a1 = bitcast<i32>(wg_scan[t + stride]);
            let b0 = bitcast<i32>(wg_prefix[t]);
            let b1 = bitcast<i32>(wg_prefix[t + stride]);
            wg_scan[t] = bitcast<u32>(select(min(a0, a1), max(a0, a1), max_a));
            wg_prefix[t] = bitcast<u32>(select(min(b0, b1), max(b0, b1), max_b));
        }
        workgroupBarrier();
    }
    if t == 0u {
        wg_union[sa] = bitcast<i32>(wg_scan[0]);
        wg_union[sb] = bitcast<i32>(wg_prefix[0]);
    }
    workgroupBarrier();
}

// Global bin index of run-local bin `i` in slab `z` of the union rect.
fn union_bin(run: u32, i: u32, u0: vec3<i32>, w: u32, z: i32, bins: vec3<i32>) -> u32 {
    let q = run * 256u + i;
    let x = u0.x + i32(q % w);
    let y = u0.y + i32(q / w);
    return u32(x + bins.x * (y + bins.y * z));
}

@compute @workgroup_size(256)
fn cs_main(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) t: u32,
) {
    let f = pv_frame(
        vec3<f32>(params.center_x, params.center_y, params.center_z),
        vec3<f32>(params.size_x, params.size_y, params.size_z),
        vec3<f32>(params.nodes_x, params.nodes_y, params.nodes_z),
        params.resolution_scale,
        params.band_extra,
        vec2<f32>(buf_bounds[0], buf_bounds[1]),
    );
    let bins = vec3<i32>(params.bins_x, params.bins_y, params.bins_z);
    // liquid_brick_map's invocation for this lane: rank = g / 2, half = g % 2.
    let idx = liquid_brick_map(group.x * 256u + t, f.nodes);
    let stores = idx != EMPTY && idx < params.dispatch_count;
    let inside = stores && idx < f.nodes.x * f.nodes.y * f.nodes.z;

    // Uniform: bins come from uniforms. Before any barrier.
    if any(bins < vec3<i32>(1)) {
        if stores {
            buf_levelset[idx] = f.band;
        }
        return;
    }

    var ijk = vec3<u32>(0u);
    var p = vec3<f32>(0.0);
    // Inactive lanes: an empty window and identities for the union.
    var first_bin = vec3<i32>(2147483647);
    var last_bin = vec3<i32>(-2147483647 - 1);
    if inside {
        ijk = pv_ijk(idx, f.nodes);
        p = pv_position(ijk, f);
        let w = pv_window(p, f, params.cell_size, bins);
        first_bin = w.first_bin;
        last_bin = w.last_bin;
    }
    var phi = f.band;

    // Union window. Only this reduction uses wg_prefix as scratch, and its
    // last barrier precedes the first run's scan writes.
    reduce_pair(t, first_bin.x, first_bin.y, 0u, 1u, false, false);
    reduce_pair(t, first_bin.z, last_bin.x, 2u, 3u, false, true);
    reduce_pair(t, last_bin.y, last_bin.z, 4u, 5u, true, true);
    let un = workgroupUniformLoad(&wg_union);
    let u0 = vec3<i32>(un[0], un[1], un[2]);
    let u1 = vec3<i32>(un[3], un[4], un[5]);

    // Uniform: an all-inactive half brick (identities survive) or an empty
    // union computes no widths and falls through with no store.
    if all(u0 <= u1) {
        let w = u32(u1.x - u0.x + 1);
        let bins_in_slab = w * u32(u1.y - u0.y + 1);
        let runs = (bins_in_slab + 255u) / 256u;
        let row_lo = max(first_bin.y, u0.y);
        let row_hi = min(last_bin.y, u1.y);
        for (var z = u0.z; z <= u1.z; z = z + 1) {
            // The lane consumes this slab only if its own window covers it.
            let lane_z = first_bin.z <= z && z <= last_bin.z && first_bin.x <= last_bin.x;
            for (var run = 0u; run < runs; run = run + 1u) {
                let base = run * 256u;
                var count = 0u;
                if base + t < bins_in_slab {
                    count = buf_cell_ranges[union_bin(run, t, u0, w, z, bins)].count;
                }
                wg_scan[t] = count;
                workgroupBarrier();
                for (var stride = 1u; stride < 256u; stride = stride * 2u) {
                    var add = 0u;
                    if t >= stride {
                        add = wg_scan[t - stride];
                    }
                    workgroupBarrier();
                    wg_scan[t] = wg_scan[t] + add;
                    workgroupBarrier();
                }
                // Uniform total, read before anything modifies the scan.
                let total = workgroupUniformLoad(&wg_scan[255]);
                wg_prefix[t] = wg_scan[t] - count;
                if t == 0u {
                    wg_prefix[256] = total;
                }
                // Prefix complete before any consumer.
                workgroupBarrier();
                for (var chunk = 0u; chunk < total; chunk = chunk + CHUNK) {
                    let g = chunk + t;
                    if t < CHUNK && g < total {
                        // Upper-bound search: prefix[i] <= g < prefix[i + 1],
                        // skipping the repeated prefixes of empty bins. May
                        // diverge; no collective operation and no return.
                        var lo = 0u;
                        var hi = 256u;
                        while hi - lo > 1u {
                            let mid = (lo + hi) / 2u;
                            if wg_prefix[mid] <= g {
                                lo = mid;
                            } else {
                                hi = mid;
                            }
                        }
                        let range = buf_cell_ranges[union_bin(run, lo, u0, w, z, bins)];
                        let blob = buf_blobs[range.start + (g - wg_prefix[lo])];
                        let b = pv_blob_box(blob.center_radius, f);
                        wg_blob[t] = blob;
                        wg_box[t * 6u] = b.first.x;
                        wg_box[t * 6u + 1u] = b.first.y;
                        wg_box[t * 6u + 2u] = b.first.z;
                        wg_box[t * 6u + 3u] = b.last.x;
                        wg_box[t * 6u + 4u] = b.last.y;
                        wg_box[t * 6u + 5u] = b.last.z;
                    }
                    // Staged chunk complete before any lane reads it.
                    workgroupBarrier();
                    if lane_z {
                        let chunk_end = min(chunk + CHUNK, total);
                        for (var y = row_lo; y <= row_hi; y = y + 1) {
                            // The lane's row as union-rect flat bins, then
                            // clipped to this run, run-local.
                            let row = u32(y - u0.y) * w;
                            let a_flat = row + u32(first_bin.x - u0.x);
                            let b_flat = row + u32(last_bin.x - u0.x);
                            let a = max(a_flat, base);
                            let b = min(b_flat, base + 255u);
                            if a > b {
                                continue;
                            }
                            let lo = max(wg_prefix[a - base], chunk);
                            let hi = min(wg_prefix[b - base + 1u], chunk_end);
                            for (var s = lo; s < hi; s = s + 1u) {
                                let i = s - chunk;
                                var box: PvBox;
                                box.first = vec3<i32>(wg_box[i * 6u], wg_box[i * 6u + 1u], wg_box[i * 6u + 2u]);
                                box.last = vec3<i32>(wg_box[i * 6u + 3u], wg_box[i * 6u + 4u], wg_box[i * 6u + 5u]);
                                let blob = wg_blob[i];
                                if pv_box_rejects(blob.center_radius.w, ijk, box) {
                                    continue;
                                }
                                phi = min(phi, pv_blob_term(p, blob.center_radius, blob.shape_diag.xyz, blob.shape_off.xyz));
                            }
                        }
                    }
                    // Every lane is done with the chunk before it is restaged.
                    workgroupBarrier();
                }
                // Unconditional: a zero-total run executes no chunk barrier;
                // prefix reads end before the next run's scan writes.
                workgroupBarrier();
            }
        }
    }

    if inside {
        buf_levelset[idx] = pv_finish(phi, p, f, params.interior_len);
    } else if stores {
        buf_levelset[idx] = f.band;
    }
}
