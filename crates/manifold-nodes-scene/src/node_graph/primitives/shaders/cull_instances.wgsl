// node.render_scene: per-copy view cull for the camera and shadow passes. An instanced
// draw can place copies all around the view; the ones whose mesh box lies
// wholly outside the camera's clip volume draw nothing, so they are dropped
// before the vertex stage. Surviving copies are compacted into a second
// instance array (and a matching previous-record array when the velocity
// pass reads one) and counted into the draw's own view block of the
// arguments buffer, a copy of the live block with its instance word raised
// one survivor at a time. Each shadow caster culls the same way against its
// own clip volume, into blocks and survivor buffers of its own.
//
// A mesh with detail levels (`lods`, laid out by mesh_lods.rs) gets one view
// block per level: block 0 draws the full mesh, block k level k from the
// `lods` array. Each copy draws at the coarsest level whose simplification
// error covers at most MAX_PIXEL_ERROR pixels at the copy's nearest point.
// `count_levels` counts each level's copies, `level_bases` sets each block's
// first instance after the levels before it, and `place_levels` writes the
// copies there, so every level draws one run of the survivor array.
//
// The test is `box_outside_clip` (render_scene/frustum.rs) on the matrix
// clip * model * T_instance * instance_inner, where T_instance is placed
// exactly as scene_vs_body places a vertex. euler_xyz and mirror_sign come
// from instance_transform.wgsl, which is prepended to this source.

struct CullArgs {
    // The jittered view_proj the draw uniforms carry.
    clip: mat4x4<f32>,
    model: mat4x4<f32>,
    // The draw's instance_inner.
    inner: mat4x4<f32>,
    box_min: vec4<f32>,
    box_max: vec4<f32>,
    // Instances the draw may reach (CPU bound).
    count: u32,
    // Eight-word argument blocks: the draw's live block, and its view block
    // (the first of `levels + 1` for a mesh with detail levels).
    live_block: u32,
    view_block: u32,
    has_prev: u32,
    // The view's size in pixels, which a level's error is measured in.
    viewport: vec2<f32>,
    // Detail-level blocks past the full mesh's: 0 without `lods`.
    levels: u32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> params: CullArgs;
// InstanceTransform: pos_scale then rot_pad, two vec4 per record.
@group(0) @binding(1) var<storage, read> instances: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> args: array<atomic<u32>>;
@group(0) @binding(3) var<storage, read_write> out_instances: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> prev: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> out_prev: array<vec4<f32>>;
// The mesh's detail levels; unread when `levels` is 0.
@group(0) @binding(6) var<storage, read> lods: array<u32>;

// f32 on the GPU against the CPU's f64 and 1e-5.
const SLACK: f32 = 1e-4;
// A coarser level's error, projected, stays within this many pixels.
const MAX_PIXEL_ERROR: f32 = 1.0;
const CULLED: u32 = 0xffffffffu;

// The view blocks start as the live block with no instances; a detail
// level's block draws that level's indices from the `lods` array.
@compute @workgroup_size(1)
fn view_args() {
    let live = params.live_block * 8u;
    let built = min(lods[0], params.levels);
    for (var level = 0u; level <= params.levels; level = level + 1u) {
        let view = (params.view_block + level) * 8u;
        for (var word = 0u; word < 8u; word = word + 1u) {
            atomicStore(&args[view + word], atomicLoad(&args[live + word]));
        }
        atomicStore(&args[view + 1u], 0u);
        if level > 0u {
            let entry = 1u + 3u * (level - 1u);
            let count = select(0u, lods[entry + 1u], level <= built);
            atomicStore(&args[view], count);
            atomicStore(&args[view + 2u], lods[entry]);
        }
    }
}

fn abs_mat(m: mat4x4<f32>) -> mat4x4<f32> {
    return mat4x4<f32>(abs(m[0]), abs(m[1]), abs(m[2]), abs(m[3]));
}

// Copy `i`'s placement: T_instance, as scene_vs_body places a vertex. A hole
// (an all-zero record) has none.
fn placement(i: u32, out_place: ptr<function, mat4x4<f32>>) -> bool {
    let pos_scale = instances[2u * i];
    let rot_pad = instances[2u * i + 1u];
    if all(pos_scale == vec4<f32>(0.0)) && all(rot_pad == vec4<f32>(0.0)) {
        return false;
    }
    let rot = euler_xyz(rot_pad.xyz);
    let msign = mirror_sign(rot_pad.w);
    let s = pos_scale.w;
    // inst_pos = R * (v * msign * s) + t
    *out_place = mat4x4<f32>(
        vec4<f32>(rot[0] * (msign.x * s), 0.0),
        vec4<f32>(rot[1] * (msign.y * s), 0.0),
        vec4<f32>(rot[2] * (msign.z * s), 0.0),
        vec4<f32>(pos_scale.xyz, 1.0),
    );
    return true;
}

// The coarsest level whose error, at depth `near_w`, spans at most
// MAX_PIXEL_ERROR pixels: the clip matrix's x and y rows give how far one
// mesh unit moves the point on screen at w = 1. A copy reaching the eye
// plane draws the full mesh.
fn level_for(to_clip: mat4x4<f32>, near_w: f32) -> u32 {
    let built = min(lods[0], params.levels);
    if built == 0u || !(near_w > 0.0) {
        return 0u;
    }
    let across = length(vec3<f32>(to_clip[0].x, to_clip[1].x, to_clip[2].x)) * params.viewport.x;
    let up = length(vec3<f32>(to_clip[0].y, to_clip[1].y, to_clip[2].y)) * params.viewport.y;
    let pixels_per_unit = 0.5 * max(across, up) / near_w;
    var level = 0u;
    for (var k = 1u; k <= built; k = k + 1u) {
        let error = bitcast<f32>(lods[1u + 3u * (k - 1u) + 2u]);
        if error * pixels_per_unit <= MAX_PIXEL_ERROR {
            level = k;
        }
    }
    return level;
}

// The level copy `i` draws at in this view, or CULLED when it is a hole or
// its box lies wholly outside the clip volume.
fn copy_level(i: u32) -> u32 {
    var place: mat4x4<f32>;
    if !placement(i, &place) {
        return CULLED;
    }
    let to_clip = params.clip * params.model * place * params.inner;
    // The same chain of absolute terms: how large the sums that make up
    // each clip coordinate are, which is what f32 rounding scales with.
    let mag = abs_mat(params.clip) * abs_mat(params.model) * abs_mat(place) * abs_mat(params.inner);

    // Per plane: still set while every corner so far lies outside it.
    var outside = 63u;
    var near_w = 3.4e38;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let p = vec3<f32>(
            select(params.box_min.x, params.box_max.x, (corner & 1u) != 0u),
            select(params.box_min.y, params.box_max.y, (corner & 2u) != 0u),
            select(params.box_min.z, params.box_max.z, (corner & 4u) != 0u),
        );
        let c = to_clip * vec4<f32>(p, 1.0);
        near_w = min(near_w, c.w);
        let size = mag * vec4<f32>(abs(p), 1.0);
        var value = array<f32, 6>(
            c.x + c.w,
            c.w - c.x,
            c.y + c.w,
            c.w - c.y,
            c.z,
            c.w - c.z,
        );
        var scale = array<f32, 6>(
            size.x + size.w,
            size.x + size.w,
            size.y + size.w,
            size.y + size.w,
            size.z,
            size.z + size.w,
        );
        for (var plane = 0u; plane < 6u; plane = plane + 1u) {
            // A NaN fails the comparison, so it never culls.
            if !(value[plane] < -SLACK * scale[plane]) {
                outside = outside & ~(1u << plane);
            }
        }
    }
    if outside != 0u {
        return CULLED;
    }
    return level_for(to_clip, near_w);
}

fn keep(i: u32, k: u32) {
    out_instances[2u * k] = instances[2u * i];
    out_instances[2u * k + 1u] = instances[2u * i + 1u];
    if params.has_prev != 0u {
        out_prev[2u * k] = prev[2u * i];
        out_prev[2u * k + 1u] = prev[2u * i + 1u];
    }
}

// A mesh without detail levels: survivors counted and written in one pass.
@compute @workgroup_size(256)
fn cull(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
) {
    let stride = groups.x * 256u;
    let block = params.view_block * 8u + 1u;
    for (var i = gid.x; i < params.count; i = i + stride) {
        if copy_level(i) == CULLED {
            continue;
        }
        keep(i, atomicAdd(&args[block], 1u));
    }
}

@compute @workgroup_size(256)
fn count_levels(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
) {
    let stride = groups.x * 256u;
    for (var i = gid.x; i < params.count; i = i + stride) {
        let level = copy_level(i);
        if level != CULLED {
            atomicAdd(&args[(params.view_block + level) * 8u + 1u], 1u);
        }
    }
}

// Each level's first instance follows the levels before it; word 5, unread
// by the draw, counts the copies placed so far.
@compute @workgroup_size(1)
fn level_bases() {
    var base = 0u;
    for (var level = 0u; level <= params.levels; level = level + 1u) {
        let view = (params.view_block + level) * 8u;
        atomicStore(&args[view + 4u], base);
        atomicStore(&args[view + 5u], 0u);
        base = base + atomicLoad(&args[view + 1u]);
    }
}

@compute @workgroup_size(256)
fn place_levels(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
) {
    let stride = groups.x * 256u;
    for (var i = gid.x; i < params.count; i = i + stride) {
        let level = copy_level(i);
        if level == CULLED {
            continue;
        }
        let view = (params.view_block + level) * 8u;
        keep(i, atomicLoad(&args[view + 4u]) + atomicAdd(&args[view + 5u], 1u));
    }
}
