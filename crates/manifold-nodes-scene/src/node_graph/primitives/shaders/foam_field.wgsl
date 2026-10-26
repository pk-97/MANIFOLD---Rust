// The foam field: matte white foam lying on the water, as the fraction of each
// surface patch it covers (0 to 1), one value per node of the water's own
// lattice. It reads that lattice through `ripple` (`look_ripple_detail.wgsl`'s
// uniforms, the same lattice as the level set), and `ripple.nodes.w` is 1 when
// a foam field is bound. This file holds only the sampling, so the ripple
// detail can scale its strength by the cover without the scene shading;
// `look_foam.wgsl` does the shading.
//
// Binds 66 itself; the mesh shader includes this file but never calls
// `foam_coverage_at`, so meshes bind none of it.

@group(0) @binding(66) var<storage, read> foam_field: array<f32>;

fn foam_node(i: vec3<i32>) -> f32 {
    let hi = vec3<i32>(ripple.nodes.xyz) - vec3<i32>(1);
    let c = clamp(i, vec3<i32>(0), hi);
    let n = u32(c.x) + ripple.nodes.x * (u32(c.y) + ripple.nodes.y * u32(c.z));
    return foam_field[n];
}

// The foam cover at a world position, 0 to 1; 0 when no foam field is bound.
fn foam_coverage_at(world_pos: vec3<f32>) -> f32 {
    if ripple.nodes.w == 0u {
        return 0.0;
    }
    let local = (ripple.world_to_local * vec4<f32>(world_pos, 1.0)).xyz - ripple.lattice_min.xyz;
    let g = ripple_lattice_coord(local);
    let b = min(vec3<i32>(floor(g)), vec3<i32>(ripple.nodes.xyz) - vec3<i32>(2));
    let f = g - vec3<f32>(b);
    let c000 = foam_node(b);
    let c100 = foam_node(b + vec3<i32>(1, 0, 0));
    let c010 = foam_node(b + vec3<i32>(0, 1, 0));
    let c110 = foam_node(b + vec3<i32>(1, 1, 0));
    let c001 = foam_node(b + vec3<i32>(0, 0, 1));
    let c101 = foam_node(b + vec3<i32>(1, 0, 1));
    let c011 = foam_node(b + vec3<i32>(0, 1, 1));
    let c111 = foam_node(b + vec3<i32>(1, 1, 1));
    let x00 = mix(c000, c100, f.x);
    let x10 = mix(c010, c110, f.x);
    let x01 = mix(c001, c101, f.x);
    let x11 = mix(c011, c111, f.x);
    return clamp(mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z), 0.0, 1.0);
}
