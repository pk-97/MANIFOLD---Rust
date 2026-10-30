// node.render_scene: per-patch culling of an indexed mesh. A patch is a run of
// PATCH_TRIANGLES consecutive triangles of the index buffer (the last run may
// be shorter); imports and Terrain order their triangles so each run is
// compact. `patch_bounds` works out each patch's box and normal cone once per
// mesh content. Per view, `patch_args` clears the view's indexed draw
// arguments and `cull_patches` drops the patches wholly outside the clip
// volume, or all facing away from the eye, copying the survivors' indices
// into the view's index buffer and counting them into the arguments.
//
// One workgroup per patch, laid out over two dispatch dimensions.

const PATCH_TRIANGLES: u32 = 128u;
const PATCH_INDICES: u32 = 384u;
const THREADS: u32 = 64u;
// A cone wider than this (its sine) is no use: the patch is never culled by it.
const NO_CONE: f32 = 2.0;
// f32 on the GPU against the CPU's f64 box test and 1e-5.
const SLACK: f32 = 1e-4;

struct PatchArgs {
    // clip * model: object space straight to the view's clip space.
    clip_model: mat4x4<f32>,
    // The eye in object space; w = 1 when the view may cull by facing.
    eye: vec4<f32>,
    index_count: u32,
    patch_count: u32,
    block: u32,
    _pad: u32,
}

struct Patch {
    box_min: vec4<f32>,
    box_max: vec4<f32>,
    // xyz: the cone's axis; w: the sine of its half-angle, NO_CONE for none.
    cone: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: PatchArgs;
// MeshVertex, 80 bytes: five vec4 per vertex, the position first.
@group(0) @binding(1) var<storage, read> vertices: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> indices: array<u32>;
@group(0) @binding(3) var<storage, read_write> patches: array<Patch>;
@group(0) @binding(4) var<storage, read_write> args: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> out_indices: array<u32>;

var<workgroup> lo: array<vec3<f32>, THREADS>;
var<workgroup> hi: array<vec3<f32>, THREADS>;
var<workgroup> facing: array<vec3<f32>, THREADS>;
var<workgroup> spread: array<f32, THREADS>;
var<workgroup> kept: u32;
var<workgroup> start: u32;

fn patch_of(wg: vec3<u32>, groups: vec3<u32>) -> u32 {
    return wg.x + wg.y * groups.x;
}

// Indices in patch `p`: PATCH_INDICES, fewer for the last.
fn patch_len(p: u32) -> u32 {
    return min(PATCH_INDICES, u.index_count - p * PATCH_INDICES);
}

fn position(i: u32) -> vec3<f32> {
    return vertices[indices[i] * 5u].xyz;
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
    let first = p * PATCH_INDICES;
    let triangles = patch_len(p) / 3u;
    var box_lo = vec3<f32>(3.4e38);
    var box_hi = vec3<f32>(-3.4e38);
    var sum = vec3<f32>(0.0);
    for (var t = lid; t < triangles; t = t + THREADS) {
        let a = position(first + 3u * t);
        let b = position(first + 3u * t + 1u);
        let c = position(first + 3u * t + 2u);
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
        let a = position(first + 3u * t);
        let b = position(first + 3u * t + 1u);
        let c = position(first + 3u * t + 2u);
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
        patches[p] = Patch(vec4<f32>(lo[0], 0.0), vec4<f32>(hi[0], 0.0), vec4<f32>(axis, sine));
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
fn faces_away(pt: Patch) -> bool {
    if u.eye.w == 0.0 || pt.cone.w >= 1.0 {
        return false;
    }
    let center = 0.5 * (pt.box_min.xyz + pt.box_max.xyz);
    let radius = 0.5 * length(pt.box_max.xyz - pt.box_min.xyz);
    let v = center - u.eye.xyz;
    return dot(v, pt.cone.xyz) >= pt.cone.w * length(v) + radius;
}

@compute @workgroup_size(64)
fn cull_patches(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
) {
    let p = patch_of(wg, groups);
    if p >= u.patch_count {
        return;
    }
    if lid == 0u {
        let pt = patches[p];
        if box_outside(pt.box_min.xyz, pt.box_max.xyz) || faces_away(pt) {
            kept = 0u;
        } else {
            kept = patch_len(p);
            start = atomicAdd(&args[u.block * 8u], kept);
        }
    }
    let n = workgroupUniformLoad(&kept);
    let base = workgroupUniformLoad(&start);
    let first = p * PATCH_INDICES;
    for (var i = lid; i < n; i = i + THREADS) {
        out_indices[base + i] = indices[first + i];
    }
}
