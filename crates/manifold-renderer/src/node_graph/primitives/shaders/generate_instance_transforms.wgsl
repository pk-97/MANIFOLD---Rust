// node.generate_instance_transforms — fill an
// Array<InstanceTransform> with a procedural layout (grid / ring
// / spiral / random). Phase B of BUFFER_PORT_PLAN.
//
// InstanceTransform layout (32 bytes):
//   pos_scale: vec4<f32>  (xyz position, w scale)
//   rot_pad:   vec4<f32>  (xyz Euler radians, w padding)

const LAYOUT_GRID: u32 = 0u;
const LAYOUT_RING: u32 = 1u;
const LAYOUT_SPIRAL: u32 = 2u;
const LAYOUT_RANDOM: u32 = 3u;

struct InstanceUniforms {
    active_count: u32,
    capacity: u32,
    layout: u32,
    seed: u32,
    extent_x: f32,
    extent_y: f32,
    extent_z: f32,
    base_scale: f32,
    rot_x: f32,
    rot_y: f32,
    rot_z: f32,
    _pad: f32,
};

struct InstanceTransform {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: InstanceUniforms;
@group(0) @binding(1) var<storage, read_write> instances: array<InstanceTransform>;

fn wang_hash(seed: u32) -> u32 {
    var s = seed;
    s = (s ^ 61u) ^ (s >> 16u);
    s = s + (s << 3u);
    s = s ^ (s >> 4u);
    s = s * 0x27d4eb2du;
    s = s ^ (s >> 15u);
    return s;
}

fn hash_to_unit(seed: u32) -> f32 {
    return f32(wang_hash(seed) & 0x00ffffffu) / 16777216.0;
}

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.capacity {
        return;
    }
    if i >= params.active_count {
        instances[i].pos_scale = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        instances[i].rot_pad = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        return;
    }

    var pos: vec3<f32>;
    var scale: f32 = params.base_scale;
    var rot: vec3<f32> = vec3<f32>(params.rot_x, params.rot_y, params.rot_z);

    if params.layout == LAYOUT_GRID {
        // Cube root → 3D grid. Side = ceil(N^(1/3)).
        let n = f32(params.active_count);
        let side = max(u32(ceil(pow(n, 1.0 / 3.0))), 1u);
        let cx = i % side;
        let cy = (i / side) % side;
        let cz = i / (side * side);
        let denom = f32(max(side - 1u, 1u));
        let nx = f32(cx) / denom - 0.5;
        let ny = f32(cy) / denom - 0.5;
        let nz = f32(cz) / denom - 0.5;
        pos = vec3<f32>(
            nx * params.extent_x,
            ny * params.extent_y,
            nz * params.extent_z,
        );
    } else if params.layout == LAYOUT_RING {
        let t = f32(i) / f32(max(params.active_count, 1u));
        let theta = t * 6.28318530718;
        pos = vec3<f32>(
            cos(theta) * params.extent_x * 0.5,
            0.0,
            sin(theta) * params.extent_z * 0.5,
        );
    } else if params.layout == LAYOUT_SPIRAL {
        let t = f32(i) / f32(max(params.active_count, 1u));
        let theta = t * 6.28318530718 * 4.0;
        let r = t;
        let y = (t - 0.5) * params.extent_y;
        pos = vec3<f32>(
            cos(theta) * r * params.extent_x * 0.5,
            y,
            sin(theta) * r * params.extent_z * 0.5,
        );
    } else {
        // LAYOUT_RANDOM — uniform within extent box.
        let h0 = hash_to_unit(i * 3u + 0u + params.seed);
        let h1 = hash_to_unit(i * 3u + 1u + params.seed);
        let h2 = hash_to_unit(i * 3u + 2u + params.seed);
        pos = vec3<f32>(
            (h0 - 0.5) * params.extent_x,
            (h1 - 0.5) * params.extent_y,
            (h2 - 0.5) * params.extent_z,
        );
    }

    instances[i].pos_scale = vec4<f32>(pos, scale);
    instances[i].rot_pad = vec4<f32>(rot, 0.0);
}
