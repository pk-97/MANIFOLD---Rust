// node.matter_stats' grain view: every point as a MatterGrain at the end of
// each tick, for a liquid stepping the same tick (node.gpu_flip_step's sand).
// Storage order is id order. The radius is the sphere of the point's rest
// volume (as node.matter_frame draws it), and the solid share its material
// row's (float 13): the share of its bulk its grains fill. A removed point
// (id 0) has radius 0. A tick whose stats hold a non-finite value keeps the
// last view. Composed after matter_material.wgsl.

struct MatterPoint {
    position: vec3<f32>,
    id: u32,
    velocity: vec3<f32>,
    volume_ratio: f32,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
    f_x: vec4<f32>,
    f_y: vec4<f32>,
    f_z: vec4<f32>,
    stress_diag: vec4<f32>,
    stress_off: vec4<f32>,
}

struct MatterGrain {
    position_radius: vec4<f32>,
    velocity_solid: vec4<f32>,
}

struct FrameParams {
    count: u32,
    previous_count: u32,
    radius_scale: f32,
    // The run's first point: 0 for every point.
    first: u32,
}

@group(0) @binding(0) var<uniform> params: FrameParams;
@group(0) @binding(1) var<storage, read> points: array<MatterPoint>;
@group(0) @binding(2) var<storage, read> stats: array<u32>;
@group(0) @binding(3) var<storage, read_write> view: array<MatterGrain>;
@group(0) @binding(4) var<storage, read> materials: array<f32>;

@compute @workgroup_size(256)
fn grain_view(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    if stats[0] != 0u {
        if i >= params.previous_count {
            view[i] = MatterGrain(vec4<f32>(0.0), vec4<f32>(0.0));
        }
        return;
    }
    let p = points[params.first + i];
    if p.id == 0u {
        view[i] = MatterGrain(vec4<f32>(p.position, 0.0), vec4<f32>(0.0));
        return;
    }
    let radius = params.radius_scale * pow(max(p.affine_y.w, 0.0), 1.0 / 3.0);
    let solid = materials[matter_slot(p.affine_z.w) * 16u + 13u];
    view[i] = MatterGrain(vec4<f32>(p.position, radius), vec4<f32>(p.velocity, solid));
}
