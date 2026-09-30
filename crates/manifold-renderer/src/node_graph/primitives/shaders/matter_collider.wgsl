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
