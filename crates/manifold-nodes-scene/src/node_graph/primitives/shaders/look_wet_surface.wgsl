// Look hook: wet surfaces. Called from `fs_pbr` after the material resolves,
// keyed on world position because materials are shared. A water film on a rough
// diffuse surface darkens it: light the surface scatters is caught under the
// film by total internal reflection and returns to the surface, which absorbs
// part of it each time (Lekner and Dorf 1988, "Why some things are darker when
// wet", Applied Optics 27(7)). With the water's index `n`, the diffuse
// (hemispherical) reflectance of the film's outer face `r_e`, the inner face's
// `r_i = 1 - (1 - r_e) / n^2` and the dry albedo `A`, the soaked albedo is
//   A_wet = (1 - r_e) (1 - r_i) A / (1 - r_i A),   per channel.
// A film is also optically smooth, so its roughness is water's: 0.
//
// The wetness is one value per node of the water's display lattice, 0 dry to 1
// soaked, a pooled field under the water's world-tile table (`field_tiles.wgsl`),
// read by world position: a mesh outside the field's extent, or over a tile the
// table does not hold, is dry.
//
// Binds 68-69 itself; the water shade includes this file but never calls the
// hook, so the water binds neither of them.

struct WetUniforms {
    world_to_local: mat4x4<f32>,
    tiles: vec4<u32>,  // x pool tiles, y table entries, z = 1 if a wetness field is bound
    cell: vec4<f32>,   // x display cell size in metres
};

@group(0) @binding(68) var<uniform> wet: WetUniforms;
@group(0) @binding(69) var<storage, read> wet_field: array<f32>;

struct WetSurface {
    albedo: vec3<f32>,
    roughness: f32,
}

// Lekner and Dorf 1988: water's refractive index and the diffuse reflectance of
// its outer face.
const WATER_IOR: f32 = 1.333;
const WATER_DIFFUSE_REFLECTANCE: f32 = 0.066;
// The inner face's diffuse reflectance, from the light that total internal
// reflection keeps: r_i = 1 - (1 - r_e) / n^2.
const WATER_INNER_REFLECTANCE: f32 = 1.0 - (1.0 - WATER_DIFFUSE_REFLECTANCE) / (WATER_IOR * WATER_IOR);

// The smallest roughness `resolve_mr` hands the GGX terms, which divide by it.
const WET_MIN_ROUGHNESS: f32 = 0.01;

// The wetness at a world position, 0 to 1; 0 with no field bound or outside the
// field's extent.
fn wetness_at(world_pos: vec3<f32>) -> f32 {
    if wet.tiles.z == 0u {
        return 0.0;
    }
    let local = (wet.world_to_local * vec4<f32>(world_pos, 1.0)).xyz;
    if !field_box_holds(local, wet.cell.x) {
        return 0.0;
    }
    field_pool = wet.tiles.xy;
    let q = field_node_coord(local, wet.cell.x);
    field_enter(q);
    let c = field_corners(q);
    let w = field_mix(
        wet_field[c.c000], wet_field[c.c100], wet_field[c.c010], wet_field[c.c110],
        wet_field[c.c001], wet_field[c.c101], wet_field[c.c011], wet_field[c.c111],
        c.f,
    );
    return clamp(w, 0.0, 1.0);
}

fn look_wet_surface(world_pos: vec3<f32>, albedo: vec3<f32>, roughness: f32) -> WetSurface {
    let w = wetness_at(world_pos);
    if w <= 0.0 {
        return WetSurface(albedo, roughness);
    }
    let r_i = WATER_INNER_REFLECTANCE;
    let soaked = (1.0 - WATER_DIFFUSE_REFLECTANCE) * (1.0 - r_i) * albedo / (vec3<f32>(1.0) - r_i * albedo);
    return WetSurface(
        mix(albedo, soaked, w),
        max(mix(roughness, 0.0, w), WET_MIN_ROUGHNESS),
    );
}
