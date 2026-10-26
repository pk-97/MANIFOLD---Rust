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
// The wetness is one value per node of the water's own lattice, 0 dry to 1
// soaked, read by world position: a mesh outside the lattice's box is dry.
//
// Binds 68-69 itself; the water shade includes this file but never calls the
// hook, so the water binds neither of them.

struct WetUniforms {
    world_to_local: mat4x4<f32>,
    lattice_min: vec4<f32>,  // xyz object-space min corner, w = 1 if a wetness field is bound
    spacing: vec4<f32>,      // xyz node spacing
    nodes: vec4<u32>,        // xyz node counts
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

fn wet_node(i: vec3<i32>) -> f32 {
    let hi = vec3<i32>(wet.nodes.xyz) - vec3<i32>(1);
    let c = clamp(i, vec3<i32>(0), hi);
    let n = u32(c.x) + wet.nodes.x * (u32(c.y) + wet.nodes.y * u32(c.z));
    return wet_field[n];
}

// The wetness at a world position, 0 to 1; 0 with no field bound or outside the
// lattice's box.
fn wetness_at(world_pos: vec3<f32>) -> f32 {
    if wet.lattice_min.w == 0.0 {
        return 0.0;
    }
    let local = (wet.world_to_local * vec4<f32>(world_pos, 1.0)).xyz - wet.lattice_min.xyz;
    let box_max = wet.spacing.xyz * (vec3<f32>(wet.nodes.xyz) - vec3<f32>(1.0));
    if any(local < vec3<f32>(0.0)) || any(local > box_max) {
        return 0.0;
    }
    let g = clamp(local / wet.spacing.xyz, vec3<f32>(0.0), vec3<f32>(wet.nodes.xyz) - vec3<f32>(1.0));
    let b = min(vec3<i32>(floor(g)), vec3<i32>(wet.nodes.xyz) - vec3<i32>(2));
    let f = g - vec3<f32>(b);
    let c000 = wet_node(b);
    let c100 = wet_node(b + vec3<i32>(1, 0, 0));
    let c010 = wet_node(b + vec3<i32>(0, 1, 0));
    let c110 = wet_node(b + vec3<i32>(1, 1, 0));
    let c001 = wet_node(b + vec3<i32>(0, 0, 1));
    let c101 = wet_node(b + vec3<i32>(1, 0, 1));
    let c011 = wet_node(b + vec3<i32>(0, 1, 1));
    let c111 = wet_node(b + vec3<i32>(1, 1, 1));
    let x00 = mix(c000, c100, f.x);
    let x10 = mix(c010, c110, f.x);
    let x01 = mix(c001, c101, f.x);
    let x11 = mix(c011, c111, f.x);
    return clamp(mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z), 0.0, 1.0);
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
