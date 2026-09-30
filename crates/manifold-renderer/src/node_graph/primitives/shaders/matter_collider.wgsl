// Collider sampling for the matter atoms (GPU_MPM_SOLVER_DESIGN.md D29),
// declared through `wgsl_includes` after matter_pose.wgsl. A shape's distance
// lattice lives in the caller's gathered atlas, so every including body
// defines the one accessor
//   fn matter_atlas_half(index: u32) -> f32
// over its own `buf_atlas`: fused codegen renames gathered globals only in
// body text, and members reading the same atlas produce identical text.
// Distances are in the shape's local (unscaled) units.

// World point x in lattice nodes of a shape (origin_spacing: local origin
// xyz, spacing w; scale per axis) posed at (position, rotation).
fn matter_lattice_coord(
    x: vec3<f32>,
    position: vec3<f32>,
    rotation: vec4<f32>,
    origin_spacing: vec4<f32>,
    scale: vec3<f32>,
) -> vec3<f32> {
    let local = matter_rotate(vec4<f32>(-rotation.xyz, rotation.w), x - position) / scale;
    return (local - origin_spacing.xyz) / origin_spacing.w;
}

fn matter_lattice_holds(g: vec3<f32>, dims: vec3<u32>) -> bool {
    return all(g >= vec3<f32>(0.0)) && all(g <= vec3<f32>(dims - vec3<u32>(1u)));
}

// Trilinear distance at lattice coordinate g, clamped to the lattice.
fn matter_lattice_distance(offset: u32, dims: vec3<u32>, g: vec3<f32>) -> f32 {
    let c = clamp(g, vec3<f32>(0.0), vec3<f32>(dims - vec3<u32>(1u)));
    let base = min(vec3<u32>(floor(c)), dims - vec3<u32>(2u));
    let f = c - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let at = base + o;
        let w = select(1.0 - f.x, f.x, o.x == 1u)
            * select(1.0 - f.y, f.y, o.y == 1u)
            * select(1.0 - f.z, f.z, o.z == 1u);
        value = value + w * matter_atlas_half(offset + at.x + dims.x * (at.y + dims.y * at.z));
    }
    return value;
}

// The world-space gradient of the local distance at g (central differences
// half a node either side), through the inverse scale and the rotation. Its
// direction is the outward normal; −φ·grad/|grad|² steps a point at local
// distance φ onto the surface for any scale.
fn matter_lattice_gradient(
    offset: u32,
    dims: vec3<u32>,
    g: vec3<f32>,
    spacing: f32,
    rotation: vec4<f32>,
    scale: vec3<f32>,
) -> vec3<f32> {
    let h = 0.5;
    let local = vec3<f32>(
        matter_lattice_distance(offset, dims, g + vec3<f32>(h, 0.0, 0.0)) - matter_lattice_distance(offset, dims, g - vec3<f32>(h, 0.0, 0.0)),
        matter_lattice_distance(offset, dims, g + vec3<f32>(0.0, h, 0.0)) - matter_lattice_distance(offset, dims, g - vec3<f32>(0.0, h, 0.0)),
        matter_lattice_distance(offset, dims, g + vec3<f32>(0.0, 0.0, h)) - matter_lattice_distance(offset, dims, g - vec3<f32>(0.0, 0.0, h)),
    ) / (2.0 * h * spacing);
    return matter_rotate(rotation, local / scale);
}

// Closed faces (bits −X, +X, −Y, +Y, −Z, +Z) stop velocity into the wall on the
// face node (node 3) and the three padding nodes beyond it, frictionless
// (taichi_elements grid_bounding_box).
fn matter_wall_stop(v: vec3<f32>, coord: vec3<u32>, n: vec3<u32>, faces: u32) -> vec3<f32> {
    let low_closed = vec3<bool>((faces & 1u) != 0u, (faces & 4u) != 0u, (faces & 16u) != 0u);
    let high_closed = vec3<bool>((faces & 2u) != 0u, (faces & 8u) != 0u, (faces & 32u) != 0u);
    let stop_low = low_closed & (coord < vec3<u32>(4u)) & (v < vec3<f32>(0.0));
    let stop_high = high_closed & (coord >= n - vec3<u32>(4u)) & (v > vec3<f32>(0.0));
    return select(v, vec3<f32>(0.0), stop_low | stop_high);
}

// Node velocity v at world x after one body (D11): inside the body (φ < 0)
// and moving into it, v_rel · n < 0 with v_rel = v − v_body(x), v_rel loses
// its normal part and keeps t̂·max(0, |t| + v_n·friction) of the tangential
// part; v = v_body + v_rel'. Otherwise v is returned unchanged.
fn matter_collider_project(
    v: vec3<f32>,
    x: vec3<f32>,
    position: vec3<f32>,
    rotation: vec4<f32>,
    linear: vec3<f32>,
    angular: vec3<f32>,
    friction: f32,
    origin_spacing: vec4<f32>,
    dims: vec3<u32>,
    atlas_offset: u32,
    scale: vec3<f32>,
) -> vec3<f32> {
    let g = matter_lattice_coord(x, position, rotation, origin_spacing, scale);
    if !matter_lattice_holds(g, dims) || matter_lattice_distance(atlas_offset, dims, g) >= 0.0 {
        return v;
    }
    let grad = matter_lattice_gradient(atlas_offset, dims, g, origin_spacing.w, rotation, scale);
    let length_sq = dot(grad, grad);
    if !(length_sq > 0.0) {
        return v;
    }
    let normal = grad * inverseSqrt(length_sq);
    let v_body = matter_body_velocity(linear, angular, position, x);
    let v_rel = v - v_body;
    let v_n = dot(v_rel, normal);
    if v_n >= 0.0 {
        return v;
    }
    let t = v_rel - v_n * normal;
    let t_len = length(t);
    let keep = max(0.0, t_len + v_n * friction);
    return v_body + select(vec3<f32>(0.0), t * (keep / t_len), t_len > 0.0);
}
