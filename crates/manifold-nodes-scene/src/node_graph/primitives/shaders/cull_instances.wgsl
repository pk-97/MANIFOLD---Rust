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
    // Eight-word argument blocks: the draw's live block, and its view block.
    live_block: u32,
    view_block: u32,
    has_prev: u32,
}

@group(0) @binding(0) var<uniform> params: CullArgs;
// InstanceTransform: pos_scale then rot_pad, two vec4 per record.
@group(0) @binding(1) var<storage, read> instances: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> args: array<atomic<u32>>;
@group(0) @binding(3) var<storage, read_write> out_instances: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> prev: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> out_prev: array<vec4<f32>>;

// f32 on the GPU against the CPU's f64 and 1e-5.
const SLACK: f32 = 1e-4;

// The view block starts as the live block with no instances.
@compute @workgroup_size(1)
fn view_args() {
    let live = params.live_block * 8u;
    let view = params.view_block * 8u;
    for (var word = 0u; word < 8u; word = word + 1u) {
        atomicStore(&args[view + word], atomicLoad(&args[live + word]));
    }
    atomicStore(&args[view + 1u], 0u);
}

fn abs_mat(m: mat4x4<f32>) -> mat4x4<f32> {
    return mat4x4<f32>(abs(m[0]), abs(m[1]), abs(m[2]), abs(m[3]));
}

@compute @workgroup_size(256)
fn cull(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
) {
    let stride = groups.x * 256u;
    let block = params.view_block * 8u + 1u;
    for (var i = gid.x; i < params.count; i = i + stride) {
        let pos_scale = instances[2u * i];
        let rot_pad = instances[2u * i + 1u];
        // A hole draws nothing.
        if all(pos_scale == vec4<f32>(0.0)) && all(rot_pad == vec4<f32>(0.0)) {
            continue;
        }
        let rot = euler_xyz(rot_pad.xyz);
        let msign = mirror_sign(rot_pad.w);
        let s = pos_scale.w;
        // inst_pos = R * (v * msign * s) + t
        let place = mat4x4<f32>(
            vec4<f32>(rot[0] * (msign.x * s), 0.0),
            vec4<f32>(rot[1] * (msign.y * s), 0.0),
            vec4<f32>(rot[2] * (msign.z * s), 0.0),
            vec4<f32>(pos_scale.xyz, 1.0),
        );
        let to_clip = params.clip * params.model * place * params.inner;
        // The same chain of absolute terms: how large the sums that make up
        // each clip coordinate are, which is what f32 rounding scales with.
        let mag = abs_mat(params.clip) * abs_mat(params.model) * abs_mat(place) * abs_mat(params.inner);

        // Per plane: still set while every corner so far lies outside it.
        var outside = 63u;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let p = vec3<f32>(
                select(params.box_min.x, params.box_max.x, (corner & 1u) != 0u),
                select(params.box_min.y, params.box_max.y, (corner & 2u) != 0u),
                select(params.box_min.z, params.box_max.z, (corner & 4u) != 0u),
            );
            let c = to_clip * vec4<f32>(p, 1.0);
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
            continue;
        }
        let k = atomicAdd(&args[block], 1u);
        out_instances[2u * k] = pos_scale;
        out_instances[2u * k + 1u] = rot_pad;
        if params.has_prev != 0u {
            out_prev[2u * k] = prev[2u * i];
            out_prev[2u * k + 1u] = prev[2u * i + 1u];
        }
    }
}
