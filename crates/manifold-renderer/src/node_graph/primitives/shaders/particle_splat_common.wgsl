// particle_splat_common.wgsl — shared sphere-impostor rasterisation helpers
// for the S6 water surface nodes (node.particle_surface_depth /
// node.particle_thickness). Single source of the view/bbox/ray-sphere math;
// the per-pixel emit (depth-tested min vs additive chord) differs per kernel
// and lives in the including file.
//
// Contract: docs/WATER_SIMULATION_DESIGN.md section 7. View space follows
// the codebase oracle Camera::project_to_pixel: the camera sits at the
// origin looking along +z (view z = dot(world - pos, fwd), positive in
// front), screen y is down (Metal viewport), and clip depth is the raw
// [0,1] mapping shared with depth_common.wgsl's linearize_depth.

struct SplatView {
    view: mat4x4<f32>,
    tan_half_fov: f32,
    near: f32,
    far: f32,
    radius: f32,
    width: u32,
    height: u32,
    count: u32,
}

// Bit-level finite test: exponent field 0xFF means NaN or ±inf. Fast math
// can compile away NaN comparisons; the exponent mask cannot (same
// convention as water_common.wgsl's water_finite1).
fn splat_finite1(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

fn splat_finite3(v: vec3<f32>) -> bool {
    return splat_finite1(v.x) && splat_finite1(v.y) && splat_finite1(v.z);
}

// View-space centre of one live particle in the +z-forward frame the
// helpers below work in. `Camera.view` is a RIGHT-handed world→view matrix
// (points in front of the camera map to NEGATIVE view z), so the z
// component is negated once here; x (right) and y (up) carry over
// unchanged. This matches the codebase oracle `Camera::project_to_pixel`'s
// convention: view_z = dot(world - pos, fwd), positive in front.
fn splat_view_center(u: SplatView, world: vec3<f32>) -> vec3<f32> {
    let cv = (u.view * vec4<f32>(world, 1.0)).xyz;
    return vec3<f32>(cv.x, cv.y, -cv.z);
}

// V1 reject rules (design section 7: reject near-plane intersection and
// underwater views with a diagnostic, never construct invalid depths).
// A particle is splatted only when its whole impostor is a valid
// front-hemisphere surface for this camera:
// - the sphere must not cross or poke the near plane (view z - radius > near),
// - the camera must not be inside the sphere (an underwater view of it),
// - the sphere must not lie entirely beyond the far plane,
// - the view centre must be finite.
fn splat_accept(u: SplatView, c: vec3<f32>) -> bool {
    let r = u.radius;
    if (!splat_finite3(c)) {
        return false;
    }
    if (c.z - r <= u.near) {
        return false;
    }
    if (dot(c, c) < r * r) {
        return false;
    }
    if (c.z - r > u.far) {
        return false;
    }
    return true;
}

// Conservative integer pixel bbox (inclusive) of the projected sphere,
// centred on the projected centre. For a sphere point p = c + d with
// |d| <= r, the projected-x deviation from the centre obeys
//   |p.x/p.z - c.x/c.z| = |c.z*d.x - c.x*d.z| / (p.z*c.z)
//                       <= r*(c.z + |c.x|) / (z_near*c.z),
// so that times the focal length bounds the half-extent (same for y with
// |c.y|). Contains the silhouette provably; the exact ray-sphere test
// inside the loop discards the small remaining padding.
fn splat_bbox(u: SplatView, c: vec3<f32>, out_min: ptr<function, vec2<i32>>, out_max: ptr<function, vec2<i32>>) {
    let aspect = f32(u.width) / f32(u.height);
    let f_px_x = 0.5 * f32(u.width) / (u.tan_half_fov * aspect);
    let f_px_y = 0.5 * f32(u.height) / u.tan_half_fov;
    let z_near = c.z - u.radius;
    let sx = (c.x / c.z) * f_px_x + 0.5 * f32(u.width);
    let sy = (-c.y / c.z) * f_px_y + 0.5 * f32(u.height);
    let ex = (u.radius * (c.z + abs(c.x)) / (z_near * c.z)) * f_px_x;
    let ey = (u.radius * (c.z + abs(c.y)) / (z_near * c.z)) * f_px_y;
    let lo = vec2<i32>(i32(floor(sx - ex)), i32(floor(sy - ey)));
    let hi = vec2<i32>(i32(ceil(sx + ex)), i32(ceil(sy + ey)));
    *out_min = clamp(lo, vec2<i32>(0, 0), vec2<i32>(i32(u.width) - 1, i32(u.height) - 1));
    *out_max = clamp(hi, vec2<i32>(0, 0), vec2<i32>(i32(u.width) - 1, i32(u.height) - 1));
}

// Normalised ray direction through pixel-centre `px` (y down).
fn splat_ray_dir(u: SplatView, px: vec2<i32>) -> vec3<f32> {
    let aspect = f32(u.width) / f32(u.height);
    let ndc_x = ((f32(px.x) + 0.5) / f32(u.width)) * 2.0 - 1.0;
    let ndc_y = 1.0 - ((f32(px.y) + 0.5) / f32(u.height)) * 2.0;
    return normalize(vec3<f32>(ndc_x * u.tan_half_fov * aspect, ndc_y * u.tan_half_fov, 1.0));
}

// Front hit of the pixel ray with the sphere: distance along the ray, or
// -1.0 when the ray misses or hits behind the camera.
fn splat_ray_hit(c: vec3<f32>, r: f32, dir: vec3<f32>) -> f32 {
    let b = dot(dir, c);
    let disc = b * b - dot(c, c) + r * r;
    if (disc <= 0.0) {
        return -1.0;
    }
    let t = b - sqrt(disc);
    if (t <= 0.0) {
        return -1.0;
    }
    return t;
}

// Optional fitted ellipsoid representation. Axes are orthogonal world-space
// semi-axis vectors; w on each axis is reserved and must be zero.
struct SurfaceShape {
    surface_center_radius: vec4<f32>,
    surface_axis_x: vec4<f32>,
    surface_axis_y: vec4<f32>,
    surface_axis_z: vec4<f32>,
}

struct ViewShape {
    center: vec3<f32>,
    axis_x: vec3<f32>,
    axis_y: vec3<f32>,
    axis_z: vec3<f32>,
    bound: f32,
}

fn shape_view(u: SplatView, s: SurfaceShape) -> ViewShape {
    let c = splat_view_center(u, s.surface_center_radius.xyz);
    let ax = (u.view * vec4<f32>(s.surface_axis_x.xyz, 0.0)).xyz;
    let ay = (u.view * vec4<f32>(s.surface_axis_y.xyz, 0.0)).xyz;
    let az = (u.view * vec4<f32>(s.surface_axis_z.xyz, 0.0)).xyz;
    return ViewShape(c, vec3<f32>(ax.x, ax.y, -ax.z), vec3<f32>(ay.x, ay.y, -ay.z), vec3<f32>(az.x, az.y, -az.z), s.surface_center_radius.w);
}

fn shape_valid(s: ViewShape) -> bool {
    return splat_finite3(s.center) && splat_finite3(s.axis_x) && splat_finite3(s.axis_y) && splat_finite3(s.axis_z)
        && splat_finite1(s.bound) && s.bound > 0.0
        && dot(s.axis_x, s.axis_x) > 1e-12 && dot(s.axis_y, s.axis_y) > 1e-12 && dot(s.axis_z, s.axis_z) > 1e-12;
}

fn shape_ray_hit(s: ViewShape, dir: vec3<f32>) -> vec2<f32> {
    let dx = dot(s.axis_x, s.axis_x); let dy = dot(s.axis_y, s.axis_y); let dz = dot(s.axis_z, s.axis_z);
    let u = vec3<f32>(dot(dir, s.axis_x) / dx, dot(dir, s.axis_y) / dy, dot(dir, s.axis_z) / dz);
    let v = vec3<f32>(dot(s.center, s.axis_x) / dx, dot(s.center, s.axis_y) / dy, dot(s.center, s.axis_z) / dz);
    let a = dot(u, u); let b = dot(u, v);
    if (a <= 1e-12) { return vec2<f32>(-1.0); }
    let closest = v - (b / a) * u;
    let disc = a * (1.0 - dot(closest, closest));
    if (disc <= 0.0) { return vec2<f32>(-1.0); }
    let root = sqrt(disc);
    return vec2<f32>((b - root) / a, (b + root) / a);
}

// Conservative projected bounds from the ellipsoid's componentwise extents.
fn shape_bbox(u: SplatView, s: ViewShape, out_min: ptr<function, vec2<i32>>, out_max: ptr<function, vec2<i32>>) {
    let c=s.center; let e=sqrt(s.axis_x*s.axis_x+s.axis_y*s.axis_y+s.axis_z*s.axis_z); let aspect=f32(u.width)/f32(u.height);
    let fx=0.5*f32(u.width)/(u.tan_half_fov*aspect); let fy=0.5*f32(u.height)/u.tan_half_fov; let zn=c.z-e.z;
    let sx=(c.x/c.z)*fx+0.5*f32(u.width); let sy=(-c.y/c.z)*fy+0.5*f32(u.height);
    let ex=fx*(c.z*e.x+abs(c.x)*e.z)/(zn*c.z); let ey=fy*(c.z*e.y+abs(c.y)*e.z)/(zn*c.z);
    *out_min=clamp(vec2<i32>(i32(floor(sx-ex)),i32(floor(sy-ey))),vec2<i32>(0),vec2<i32>(i32(u.width)-1,i32(u.height)-1));
    *out_max=clamp(vec2<i32>(i32(ceil(sx+ex)),i32(ceil(sy+ey))),vec2<i32>(0),vec2<i32>(i32(u.width)-1,i32(u.height)-1));
}
