// node.instance_position_jitter — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT multi-input. Add 3-axis 3D-simplex position noise to each
// InstanceTransform's pos.xyz; scale (.w) and rotation pass through. Matches
// instance_position_jitter.wgsl bit-for-bit (same simplex3d, prepended via
// wgsl_includes from noise_common.wgsl).
//
// ABI (buffer standalone codegen): both array inputs are coincident — the
// wrapper pre-reads `e_instances = buf_instances[idx]` (InstanceTransform) and
// `e_uv = buf_uv[idx]` (the [f32;2] grid UV) and passes them; the body returns
// the jittered instance written to buf_out_instances[idx] (the in/out ports
// share the name `instances`, so the output global is disambiguated). The
// codegen synthesizes:
//   struct Element  { pos_scale: vec4<f32>, rot: vec4<f32> }  // InstanceTransform
//   struct Element2 { x: f32, y: f32 }                        // [f32;2] UV
// from the Channels signatures. `simplex3d` comes from the prepended
// noise_common include.
fn body(
    idx: u32,
    count: u32,
    e_instances: Element,
    e_uv: Element2,
    frequency: f32,
    amplitude: f32,
    time_uvx_drift: f32,
    z_coord: f32,
    axis_seed: f32,
) -> Element {
    let base = vec3<f32>(
        e_uv.x * frequency + time_uvx_drift,
        e_uv.y * frequency,
        z_coord,
    );

    let dx = simplex3d(base);
    let dy = simplex3d(base + vec3<f32>(axis_seed, 0.0, 0.0));
    let dz = simplex3d(base + vec3<f32>(0.0, axis_seed, 0.0));

    var pos = e_instances.pos_scale.xyz;
    pos = pos + vec3<f32>(dx, dy, dz) * amplitude;

    return Element(
        vec4<f32>(pos, e_instances.pos_scale.w),
        e_instances.rot,
    );
}
