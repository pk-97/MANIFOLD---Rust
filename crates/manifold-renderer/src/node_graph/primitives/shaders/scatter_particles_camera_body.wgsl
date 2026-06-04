// node.scatter_particles_camera — BUFFER body (freeze §12, buffer domain),
// ATOMIC SCATTER with camera projection. Each live particle is projected
// through the camera (perspective, or orthographic with toroidal wrap) to a
// screen UV, then atomic-adds scaled_energy into a 2D disp_w × disp_h
// accumulator. Matches fluid_scatter_3d.wgsl `splat_projected`.
//
// ABI: `particles` (Particle) coincident → e_particles. The `accum` output is
// an ATOMIC u32 accumulator (`atomic_outputs: ["accum"]`) — the body
// `atomicAdd`s into the `buf_accum` global. The camera basis
// (cam_pos / cam_fwd / cam_right / cam_up) arrives as 4 DERIVED vec3 uniforms
// (run() resolves them from the wired Camera input each frame). `mode`
// (Enum→u32, port-shadowed in run()) drives ortho = (mode == 1); aspect is
// derived in-body from disp_w / disp_h. `active_count` (i32) is unused here —
// the wrapper's dispatch_count guard bounds the splat. Element = Particle.
fn scp_project(
    position: vec3<f32>,
    ortho: u32,
    aspect: f32,
    cam_pos: vec3<f32>,
    cam_fwd: vec3<f32>,
    cam_right: vec3<f32>,
    cam_up: vec3<f32>,
) -> vec2<f32> {
    let world_pos = position - 0.5;
    if ortho != 0u {
        // Orthographic: frac() wraps toroidally so edges connect seamlessly.
        return vec2<f32>(
            fract(dot(world_pos, cam_right) + 0.5),
            fract(dot(world_pos, cam_up) + 0.5),
        );
    }
    // Perspective: geometrically correct for containers; cull behind camera.
    let rel = world_pos - cam_pos;
    let view_z = dot(rel, cam_fwd);
    if view_z <= 0.001 {
        return vec2<f32>(-1.0, -1.0);
    }
    return vec2<f32>(
        dot(rel, cam_right) / (view_z * aspect) + 0.5,
        dot(rel, cam_up) / view_z + 0.5,
    );
}

fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    active_count: i32,
    disp_w: i32,
    disp_h: i32,
    mode: u32,
    scaled_energy: i32,
    cam_pos: vec3<f32>,
    cam_fwd: vec3<f32>,
    cam_right: vec3<f32>,
    cam_up: vec3<f32>,
) {
    if e_particles.life <= 0.0 {
        return;
    }
    let ortho = select(0u, 1u, mode == 1u);
    let dw = u32(disp_w);
    let dh = u32(disp_h);
    let aspect = f32(dw) / max(f32(dh), 1.0);

    let screen_uv = scp_project(
        e_particles.position, ortho, aspect, cam_pos, cam_fwd, cam_right, cam_up,
    );

    // Ortho never culls (toroidal). Perspective culls out-of-bounds.
    if ortho == 0u {
        if screen_uv.x < 0.0 || screen_uv.x >= 1.0 || screen_uv.y < 0.0 || screen_uv.y >= 1.0 {
            return;
        }
    }

    let coord = vec2<u32>(
        min(u32(screen_uv.x * f32(dw)), dw - 1u),
        min(u32(screen_uv.y * f32(dh)), dh - 1u),
    );
    let cell = coord.y * dw + coord.x;
    atomicAdd(&buf_accum[cell], u32(scaled_energy));
}
