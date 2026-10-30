// node.render_scene: per-patch culling of an indexed mesh. A patch is one of
// two things:
// - a run of PATCH_TRIANGLES consecutive triangles of the index buffer (the
//   last run may be shorter), when the mesh has no cluster hierarchy;
//   imports and Terrain order their triangles so each run is compact, and
//   `patch_bounds` works out each run's record once per mesh content;
// - a cluster of the mesh's crack-free detail hierarchy (`mesh_clusters.rs`
//   lays out the array: header, records, then every level's indices).
// Per view, `patch_args` clears the view's indexed draw arguments and
// `cull_patches` drops the patches wholly outside the clip volume, all
// facing away from the eye, or not on this view's detail cut, copying the
// survivors' indices into the view's index buffer and counting them into the
// arguments; `clamp_args` keeps the count inside that buffer.
//
// A cluster draws when its own group's simplified error projects past
// MAX_PIXEL_ERROR (the coarser version is not good enough) and its refined
// group's projects within it (it is good enough), or it is original
// geometry. Every cluster of a group reads the same group spheres, so the
// cut is the same on both sides of a seam and opens no cracks. A run is
// always on the cut.
//
// One workgroup per patch, laid out over two dispatch dimensions.

const PATCH_TRIANGLES: u32 = 128u;
const PATCH_INDICES: u32 = 384u;
const THREADS: u32 = 64u;
// Words per record, as `mesh_clusters.rs` writes them.
const RECORD_WORDS: u32 = 24u;
// A cone wider than this (its sine) is no use: the patch is never culled by it.
const NO_CONE: f32 = 2.0;
// f32 on the GPU against the CPU's f64 and 1e-5.
const SLACK: f32 = 1e-4;
// A detail level's error, projected, stays within this many pixels.
const MAX_PIXEL_ERROR: f32 = 1.0;
const NEVER: f32 = 3.4e38;

struct PatchArgs {
    // clip * model: object space straight to the view's clip space.
    clip_model: mat4x4<f32>,
    // The eye in object space; w = 1 when the view may cull by facing.
    eye: vec4<f32>,
    // The view's size in pixels, which detail errors are measured in.
    viewport: vec2<f32>,
    // Runs: the mesh's index count and patch count. Clusters: the full
    // mesh's index count, which sizes the view's index buffer; the patch
    // count comes from the array's header.
    index_count: u32,
    patch_count: u32,
    block: u32,
    // 1 when the records are a cluster array, read from word 16 on.
    clusters: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> u: PatchArgs;
// MeshVertex, 80 bytes: five vec4 per vertex, the position first.
@group(0) @binding(1) var<storage, read> vertices: array<vec4<f32>>;
// The index words the records point into: the mesh's indices for runs, the
// cluster array for clusters.
@group(0) @binding(2) var<storage, read> indices: array<u32>;
// Runs: the records `patch_bounds` writes. Clusters: the cluster array.
@group(0) @binding(3) var<storage, read_write> records: array<u32>;
@group(0) @binding(4) var<storage, read_write> args: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> out_indices: array<u32>;

var<workgroup> lo: array<vec3<f32>, THREADS>;
var<workgroup> hi: array<vec3<f32>, THREADS>;
var<workgroup> facing: array<vec3<f32>, THREADS>;
var<workgroup> spread: array<f32, THREADS>;
var<workgroup> kept: u32;
var<workgroup> start: u32;
var<workgroup> first: u32;

fn patch_of(wg: vec3<u32>, groups: vec3<u32>) -> u32 {
    return wg.x + wg.y * groups.x;
}

// Indices in run `p`: PATCH_INDICES, fewer for the last.
fn run_len(p: u32) -> u32 {
    return min(PATCH_INDICES, u.index_count - p * PATCH_INDICES);
}

fn position(i: u32) -> vec3<f32> {
    return vertices[indices[i] * 5u].xyz;
}

fn record_at(p: u32) -> u32 {
    return select(0u, 16u, u.clusters != 0u) + p * RECORD_WORDS;
}

fn patch_count() -> u32 {
    if u.clusters != 0u {
        return records[0];
    }
    return u.patch_count;
}

fn word_f(at: u32) -> f32 {
    return bitcast<f32>(records[at]);
}

fn vec3_at(at: u32) -> vec3<f32> {
    return vec3<f32>(word_f(at), word_f(at + 1u), word_f(at + 2u));
}

fn put_vec3(at: u32, v: vec3<f32>) {
    records[at] = bitcast<u32>(v.x);
    records[at + 1u] = bitcast<u32>(v.y);
    records[at + 2u] = bitcast<u32>(v.z);
}

@compute @workgroup_size(64)
fn patch_bounds(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
) {
    let p = patch_of(wg, groups);
    if p >= u.patch_count {
        return;
    }
    let base = p * PATCH_INDICES;
    let triangles = run_len(p) / 3u;
    var box_lo = vec3<f32>(3.4e38);
    var box_hi = vec3<f32>(-3.4e38);
    var sum = vec3<f32>(0.0);
    for (var t = lid; t < triangles; t = t + THREADS) {
        let a = position(base + 3u * t);
        let b = position(base + 3u * t + 1u);
        let c = position(base + 3u * t + 2u);
        box_lo = min(box_lo, min(a, min(b, c)));
        box_hi = max(box_hi, max(a, max(b, c)));
        let n = cross(b - a, c - a);
        let len = length(n);
        if len > 0.0 {
            sum = sum + n / len;
        }
    }
    lo[lid] = box_lo;
    hi[lid] = box_hi;
    facing[lid] = sum;
    workgroupBarrier();
    for (var stride = THREADS / 2u; stride > 0u; stride = stride / 2u) {
        if lid < stride {
            lo[lid] = min(lo[lid], lo[lid + stride]);
            hi[lid] = max(hi[lid], hi[lid + stride]);
            facing[lid] = facing[lid] + facing[lid + stride];
        }
        workgroupBarrier();
    }
    let total = facing[0];
    let axis = select(vec3<f32>(0.0), normalize(total), length(total) > 0.0);
    // The least any triangle's normal agrees with the axis.
    var least = 1.0;
    for (var t = lid; t < triangles; t = t + THREADS) {
        let a = position(base + 3u * t);
        let b = position(base + 3u * t + 1u);
        let c = position(base + 3u * t + 2u);
        let n = cross(b - a, c - a);
        let len = length(n);
        if len > 0.0 {
            least = min(least, dot(n / len, axis));
        }
    }
    spread[lid] = least;
    workgroupBarrier();
    for (var stride = THREADS / 2u; stride > 0u; stride = stride / 2u) {
        if lid < stride {
            spread[lid] = min(spread[lid], spread[lid + stride]);
        }
        workgroupBarrier();
    }
    if lid == 0u {
        let cos_half = spread[0];
        // sin of the half-angle; a cone of 90 degrees or more culls nothing.
        let sine = select(NO_CONE, sqrt(max(0.0, 1.0 - cos_half * cos_half)), cos_half > 0.0);
        let at = record_at(p);
        put_vec3(at, lo[0]);
        records[at + 3u] = base;
        put_vec3(at + 4u, hi[0]);
        records[at + 7u] = run_len(p);
        put_vec3(at + 8u, axis);
        records[at + 11u] = bitcast<u32>(sine);
        // Always on the cut: no coarser version, original geometry.
        records[at + 16u] = bitcast<u32>(NEVER);
        records[at + 21u] = bitcast<u32>(-1.0);
    }
}

@compute @workgroup_size(1)
fn patch_args() {
    let base = u.block * 8u;
    atomicStore(&args[base], 0u);
    atomicStore(&args[base + 1u], 1u);
    atomicStore(&args[base + 2u], 0u);
    atomicStore(&args[base + 3u], 0u);
    atomicStore(&args[base + 4u], 0u);
}

// The kept indices never pass the view's index buffer, sized for the full
// mesh: a cut never holds more triangles than the original geometry, and if
// one ever did, the patch past the end was cut short rather than drawn from
// stale words.
@compute @workgroup_size(1)
fn clamp_args() {
    let base = u.block * 8u;
    atomicMin(&args[base], u.index_count);
}

// True when every corner of the box lies outside the same plane of Metal's
// clip volume: -w <= x <= w, -w <= y <= w, 0 <= z <= w.
fn box_outside(box_min: vec3<f32>, box_max: vec3<f32>) -> bool {
    var outside = array<bool, 6>(true, true, true, true, true, true);
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let q = select(box_min, box_max, vec3<bool>((corner & 1u) != 0u, (corner & 2u) != 0u, (corner & 4u) != 0u));
        let c = u.clip_model * vec4<f32>(q, 1.0);
        let slack = SLACK * (abs(c.w) + abs(c.x) + abs(c.y) + abs(c.z));
        outside[0] = outside[0] && c.x + c.w < -slack;
        outside[1] = outside[1] && c.w - c.x < -slack;
        outside[2] = outside[2] && c.y + c.w < -slack;
        outside[3] = outside[3] && c.w - c.y < -slack;
        outside[4] = outside[4] && c.z < -slack;
        outside[5] = outside[5] && c.w - c.z < -slack;
    }
    return outside[0] || outside[1] || outside[2] || outside[3] || outside[4] || outside[5];
}

// True when every triangle of the patch faces away from the eye: for every
// point and every normal in the cone, the eye sees the triangle's back.
fn faces_away(box_min: vec3<f32>, box_max: vec3<f32>, cone: vec4<f32>) -> bool {
    if u.eye.w == 0.0 || cone.w >= 1.0 {
        return false;
    }
    let center = 0.5 * (box_min + box_max);
    let radius = 0.5 * length(box_max - box_min);
    let v = center - u.eye.xyz;
    return dot(v, cone.xyz) >= cone.w * length(v) + radius;
}

// Whether an error of `error` mesh units anywhere in the sphere spans more
// than MAX_PIXEL_ERROR pixels: the clip matrix's x and y rows give how far
// one mesh unit moves a point on screen at w = 1, and the sphere's nearest
// w scales it. A sphere reaching the eye plane is always over.
fn over(center: vec3<f32>, radius: f32, error: f32) -> bool {
    let m = u.clip_model;
    let row_x = vec3<f32>(m[0].x, m[1].x, m[2].x);
    let row_y = vec3<f32>(m[0].y, m[1].y, m[2].y);
    let row_w = vec3<f32>(m[0].w, m[1].w, m[2].w);
    let near_w = dot(row_w, center) + m[3].w - radius * length(row_w);
    if !(near_w > 0.0) {
        return true;
    }
    let pixels_per_unit = 0.5 * max(length(row_x) * u.viewport.x, length(row_y) * u.viewport.y) / near_w;
    return error * pixels_per_unit > MAX_PIXEL_ERROR;
}

// Whether record `at` is on this view's detail cut.
fn on_cut(at: u32) -> bool {
    let own_error = word_f(at + 16u);
    if own_error < NEVER && !over(vec3_at(at + 12u), word_f(at + 15u), own_error) {
        return false;
    }
    let refined_error = word_f(at + 21u);
    return refined_error < 0.0 || !over(vec3_at(at + 17u), word_f(at + 20u), refined_error);
}

@compute @workgroup_size(64)
fn cull_patches(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
) {
    let p = patch_of(wg, groups);
    if p >= patch_count() {
        return;
    }
    if lid == 0u {
        let at = record_at(p);
        let box_min = vec3_at(at);
        let box_max = vec3_at(at + 4u);
        let cone = vec4<f32>(vec3_at(at + 8u), word_f(at + 11u));
        kept = 0u;
        if on_cut(at) && !box_outside(box_min, box_max) && !faces_away(box_min, box_max, cone) {
            let count = records[at + 7u];
            let slot = atomicAdd(&args[u.block * 8u], count);
            kept = min(count, max(u.index_count, slot) - slot);
            start = slot;
            first = records[at + 3u];
        }
    }
    let n = workgroupUniformLoad(&kept);
    let base = workgroupUniformLoad(&start);
    let src = workgroupUniformLoad(&first);
    for (var i = lid; i < n; i = i + THREADS) {
        out_indices[base + i] = indices[src + i];
    }
}
