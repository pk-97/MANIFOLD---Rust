// node.instance_rotation_jitter — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT. Add hash-driven per-instance Euler-rotation jitter to
// each InstanceTransform's rot.xyz (ADD semantics); position + scale pass
// through. Matches instance_rotation_jitter.wgsl bit-for-bit (same hash_u32,
// prepended via wgsl_includes from noise_common.wgsl; integer hash → exact).
//
// ABI (buffer standalone codegen): `instances` (InstanceTransform) is coincident,
// so the wrapper pre-reads `e_instances = buf_instances[idx]` and passes it; the
// body returns the jittered instance written to buf_out_instances[idx] (the
// in/out ports share the name `instances`). The codegen synthesizes
//   struct Element { pos_scale: vec4<f32>, rot: vec4<f32> }
// from InstanceTransform's Channels signature. `hash_u32` comes from the
// prepended noise_common include; the body uses `idx` to key the per-axis hash.
fn body(idx: u32, count: u32, e_instances: Element, amplitude: f32) -> Element {
    let rx = (hash_u32(idx * 3u + 0u) - 0.5) * amplitude;
    let ry = (hash_u32(idx * 3u + 1u) - 0.5) * amplitude;
    let rz = (hash_u32(idx * 3u + 2u) - 0.5) * amplitude;

    let new_rot = e_instances.rot.xyz + vec3<f32>(rx, ry, rz);
    return Element(
        e_instances.pos_scale,
        vec4<f32>(new_rot, e_instances.rot.w),
    );
}
