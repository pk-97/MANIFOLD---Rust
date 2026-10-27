// The foam field: matte white foam lying on the water, as the fraction of each
// surface patch it covers (0 to 1), one value per node of the water's display
// lattice, a pooled field under the water's world-tile table
// (`field_tiles.wgsl`). It reads that table through `ripple`
// (`look_ripple_detail.wgsl`'s uniforms, the same field as the level set), and
// `ripple.tiles.z` is 1 when a foam field is bound. This file holds only the
// sampling, so the ripple detail can scale its strength by the cover without
// the scene shading; `look_foam.wgsl` does the shading.
//
// Binds 66 itself; the mesh shader includes this file but never calls
// `foam_coverage_at`, so meshes bind none of it.

@group(0) @binding(66) var<storage, read> foam_field: array<f32>;

// The foam cover at a world position, 0 to 1; 0 when no foam field is bound.
fn foam_coverage_at(world_pos: vec3<f32>) -> f32 {
    if ripple.tiles.z == 0u {
        return 0.0;
    }
    field_pool = ripple.tiles.xy;
    let local = (ripple.world_to_local * vec4<f32>(world_pos, 1.0)).xyz;
    let q = field_node_coord(local, ripple.field.x);
    field_enter(q);
    let c = field_corners(q);
    let cover = field_mix(
        foam_field[c.c000], foam_field[c.c100], foam_field[c.c010], foam_field[c.c110],
        foam_field[c.c001], foam_field[c.c101], foam_field[c.c011], foam_field[c.c111],
        c.f,
    );
    return clamp(cover, 0.0, 1.0);
}
