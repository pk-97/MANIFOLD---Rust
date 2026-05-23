// node.instance_rotation_jitter — add hash-driven per-instance
// rotation jitter to each InstanceTransform's rot_pad.xyz; positions
// and scale pass through.
//
// noise_common.wgsl is prepended at pipeline creation and supplies
// hash_u32().
//
// For each idx:
//   rx = (hash_u32(idx * 3 + 0) - 0.5) * amplitude
//   ry = (hash_u32(idx * 3 + 1) - 0.5) * amplitude
//   rz = (hash_u32(idx * 3 + 2) - 0.5) * amplitude
//   rot_pad.xyz += vec3(rx, ry, rz)
//
// Bit-exact with the legacy DigitalPlants per-instance rotation
// hash (which uses amplitude = 0.2, giving a [-0.1, 0.1] range) —
// same hash_u32 source from noise_common, same idx*3+{0,1,2} keys.

struct Uniforms {
    count:     u32,
    amplitude: f32,
    _pad0:     u32,
    _pad1:     u32,
};

struct InstanceTransform {
    pos_scale: vec4<f32>,
    rot_pad:   vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read>       in_inst:  array<InstanceTransform>;
@group(0) @binding(2) var<storage, read_write> out_inst: array<InstanceTransform>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }

    let rx = (hash_u32(idx * 3u + 0u) - 0.5) * u.amplitude;
    let ry = (hash_u32(idx * 3u + 1u) - 0.5) * u.amplitude;
    let rz = (hash_u32(idx * 3u + 2u) - 0.5) * u.amplitude;

    let inst = in_inst[idx];
    let new_rot = inst.rot_pad.xyz + vec3<f32>(rx, ry, rz);
    out_inst[idx] = InstanceTransform(
        inst.pos_scale,
        vec4<f32>(new_rot, inst.rot_pad.w),
    );
}
