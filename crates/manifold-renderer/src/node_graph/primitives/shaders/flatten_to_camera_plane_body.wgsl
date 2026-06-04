// node.flatten_to_camera_plane — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT. Compress each live particle toward the camera viewing
// plane: depth = dot(pos - 0.5, cam_fwd); pos -= cam_fwd * depth * flatten * 0.1.
// Matches flatten_to_camera_plane.wgsl bit-for-bit.
//
// ABI (buffer standalone codegen): `in` (Particle) coincident → e_in; in/out
// alias one buffer (run() binds it to slots 1 and 2), so returning e_in
// unchanged on an early-out reproduces the hand kernel's no-write. The wired
// Camera port is resolved CPU-side into the THREE derived uniform fields
// cam_fwd_x/y/z (declared `derived_uniforms`) — the generated kernel never sees
// a Camera binding. Element = the Particle struct. `active_count` (the wrapper
// guard = dispatch_count) is unused here.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    flatten: f32,
    active_count: i32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
) -> Element {
    var p = e_in;
    if flatten <= 0.0 {
        return p;
    }
    if p.life <= 0.0 {
        return p;
    }

    let cam_fwd = vec3<f32>(cam_fwd_x, cam_fwd_y, cam_fwd_z);
    let depth_from_center = dot(p.position - 0.5, cam_fwd);
    p.position = p.position - cam_fwd * depth_from_center * flatten * 0.1;
    return p;
}
