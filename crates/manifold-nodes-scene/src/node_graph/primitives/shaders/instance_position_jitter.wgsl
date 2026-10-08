// node.instance_position_jitter — add 3-axis 3D-simplex position
// noise to each instance's pos.xyz.
//
// noise_common.wgsl is prepended at pipeline creation time and
// supplies simplex3d().
//
// For each idx:
//   uv_scaled = vec3(uv.x * freq + time_uvx_drift,
//                    uv.y * freq,
//                    z_coord)
//   pos.x += simplex3d(uv_scaled)                      * amp
//   pos.y += simplex3d(uv_scaled + vec3(seed, 0, 0))   * amp
//   pos.z += simplex3d(uv_scaled + vec3(0, seed, 0))   * amp
//
// `axis_seed` is the magnitude of the decorrelation offsets between
// the three axis samples (legacy DigitalPlants uses 100 for the
// cylinder detail pass, 50 for the torus micro pass). The choice
// only matters in that it should be large enough to land in a
// different noise cell across the channels; tuning it is rare.

struct Uniforms {
    count:           u32,
    frequency:       f32,
    amplitude:       f32,
    time_uvx_drift:  f32,
    z_coord:         f32,
    axis_seed:       f32,
    _pad0:           u32,
    _pad1:           u32,
};

struct InstanceTransform {
    pos_scale: vec4<f32>,
    rot_pad:   vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read>       uv_in:     array<vec2<f32>>;
@group(0) @binding(2) var<storage, read>       in_inst:   array<InstanceTransform>;
@group(0) @binding(3) var<storage, read_write> out_inst:  array<InstanceTransform>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }

    let uv = uv_in[idx];
    let base = vec3<f32>(
        uv.x * u.frequency + u.time_uvx_drift,
        uv.y * u.frequency,
        u.z_coord,
    );

    let dx = simplex3d(base);
    let dy = simplex3d(base + vec3<f32>(u.axis_seed, 0.0, 0.0));
    let dz = simplex3d(base + vec3<f32>(0.0, u.axis_seed, 0.0));

    let inst = in_inst[idx];
    var pos = inst.pos_scale.xyz;
    pos = pos + vec3<f32>(dx, dy, dz) * u.amplitude;

    out_inst[idx] = InstanceTransform(
        vec4<f32>(pos, inst.pos_scale.w),
        inst.rot_pad,
    );
}
