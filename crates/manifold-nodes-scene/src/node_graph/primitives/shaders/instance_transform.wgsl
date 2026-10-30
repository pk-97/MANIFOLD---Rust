// Instance transform helpers shared by render_scene.wgsl (prepended to it) and
// cull_instances.wgsl, so the cull places a copy exactly as the vertex stage does.

// Build a 3×3 rotation matrix from XYZ Euler angles (XYZ order).
// Bit-for-bit the same as render_instanced_3d_mesh.wgsl's euler_xyz —
// forked, not shared, with that shader.
fn euler_xyz(angles: vec3<f32>) -> mat3x3<f32> {
    let cx = cos(angles.x);
    let sx = sin(angles.x);
    let cy = cos(angles.y);
    let sy = sin(angles.y);
    let cz = cos(angles.z);
    let sz = sin(angles.z);

    let rx = mat3x3<f32>(
        vec3<f32>(1.0, 0.0, 0.0),
        vec3<f32>(0.0, cx, sx),
        vec3<f32>(0.0, -sx, cx),
    );
    let ry = mat3x3<f32>(
        vec3<f32>(cy, 0.0, -sy),
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(sy, 0.0, cy),
    );
    let rz = mat3x3<f32>(
        vec3<f32>(cz, sz, 0.0),
        vec3<f32>(-sz, cz, 0.0),
        vec3<f32>(0.0, 0.0, 1.0),
    );
    return rz * ry * rx;
}


// rot_pad.w is 4 · key + mirror. The mirror, from node.reflect_array: 0 =
// original, k > 0 = mirrored across the plane perpendicular to component
// k-1; unmarked instances take (1,1,1). The key is a copy's own texture
// key (`copy_key`), 0 for none: node.matter_grains keys each grain.
fn mirror_sign(marker_w: f32) -> vec3<f32> {
    let marker = u32(marker_w + 0.5) % 4u;
    return vec3<f32>(
        select(1.0, -1.0, marker == 1u),
        select(1.0, -1.0, marker == 2u),
        select(1.0, -1.0, marker == 3u),
    );
}

// A copy's own texture key from rot_pad.w, 0 for none (`mirror_sign`).
fn copy_key(marker_w: f32) -> u32 {
    return u32(marker_w + 0.5) / 4u;
}
