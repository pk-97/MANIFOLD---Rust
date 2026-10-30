// node.render_scene glow gather (render_scene/glow.rs): an emissive mesh's
// glowing area and that area's centre and spread in the world, so a few soft
// lights can stand in for the light it gives off. Standalone module; each
// entry point names only the bindings it reads.
//
//   glow_gather:   GLOW_GROUPS workgroups stride over the mesh's triangles;
//                  each writes its area-weighted sums as the first three
//                  vec4s of its four in `partials`.
//   glow_map_mean: one workgroup averages the emissive map into the fourth
//                  vec4 of group 0.

struct GlowParams {
    model: mat4x4<f32>,
    // The point the sums are taken about (the object's origin), so the
    // second moments stay precise far from the world's origin.
    origin: vec4<f32>,
    // x: elements the buffer holds (indices when indexed, else vertices),
    // y: 1 when indexed, z: the live count's word in `counts`, or
    // 0xffffffff for none, w: live elements per counted item.
    elements: vec4<u32>,
};

const GROUP_SIZE: u32 = 256u;
const GLOW_GROUPS: u32 = 64u;
// MeshVertex is 80 bytes, position first.
const VERTEX_FLOATS: u32 = 20u;
// The map mean reads a 32 x 32 grid, four texels a thread.
const MEAN_GRID: u32 = 32u;

@group(0) @binding(0) var<uniform> params: GlowParams;
@group(0) @binding(1) var<storage, read> vertices: array<f32>;
@group(0) @binding(2) var<storage, read> indices: array<u32>;
@group(0) @binding(3) var<storage, read> counts: array<u32>;
@group(0) @binding(4) var<storage, read_write> partials: array<vec4<f32>>;
@group(0) @binding(5) var emissive_map: texture_2d<f32>;

var<workgroup> sum_area: array<vec4<f32>, GROUP_SIZE>;
var<workgroup> sum_xx: array<vec4<f32>, GROUP_SIZE>;
var<workgroup> sum_yz: array<vec4<f32>, GROUP_SIZE>;

fn corner(element: u32) -> vec3<f32> {
    var vertex = element;
    if params.elements.y == 1u {
        vertex = indices[element];
    }
    let base = vertex * VERTEX_FLOATS;
    let p = vec3<f32>(vertices[base], vertices[base + 1u], vertices[base + 2u]);
    return (params.model * vec4<f32>(p, 1.0)).xyz - params.origin.xyz;
}

// Per group: (area, area x centre), area x (xx, xy, xz, yy), area x (yz, zz).
// Triangles past the live count are zero-area.
@compute @workgroup_size(256)
fn glow_gather(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    var elements = params.elements.x;
    if params.elements.z != 0xffffffffu {
        elements = min(elements, counts[params.elements.z] * params.elements.w);
    }
    let triangles = elements / 3u;
    var area_sum = vec4<f32>(0.0);
    var xx = vec4<f32>(0.0);
    var yz = vec4<f32>(0.0);
    for (var t = wid.x * GROUP_SIZE + lid; t < triangles; t += GLOW_GROUPS * GROUP_SIZE) {
        let a = corner(3u * t);
        let b = corner(3u * t + 1u);
        let c = corner(3u * t + 2u);
        let area = 0.5 * length(cross(b - a, c - a));
        let m = (a + b + c) / 3.0;
        area_sum += area * vec4<f32>(1.0, m);
        xx += area * vec4<f32>(m.x * m.x, m.x * m.y, m.x * m.z, m.y * m.y);
        yz += area * vec4<f32>(m.y * m.z, m.z * m.z, 0.0, 0.0);
    }
    sum_area[lid] = area_sum;
    sum_xx[lid] = xx;
    sum_yz[lid] = yz;
    workgroupBarrier();
    for (var stride = GROUP_SIZE / 2u; stride > 0u; stride = stride / 2u) {
        if lid < stride {
            sum_area[lid] += sum_area[lid + stride];
            sum_xx[lid] += sum_xx[lid + stride];
            sum_yz[lid] += sum_yz[lid + stride];
        }
        workgroupBarrier();
    }
    if lid == 0u {
        partials[4u * wid.x] = sum_area[0];
        partials[4u * wid.x + 1u] = sum_xx[0];
        partials[4u * wid.x + 2u] = sum_yz[0];
    }
}

// The map's mean colour, read from the level about 32 to 64 texels across
// when the map has mips (each texel there already a box average), else from
// the full picture.
@compute @workgroup_size(256)
fn glow_map_mean(@builtin(local_invocation_index) lid: u32) {
    let levels = textureNumLevels(emissive_map);
    let level = u32(max(i32(levels) - 6, 0));
    let size = vec2<f32>(textureDimensions(emissive_map, level));
    var sum = vec3<f32>(0.0);
    for (var k = 0u; k < 4u; k++) {
        let cell = 4u * lid + k;
        let at = (vec2<f32>(f32(cell % MEAN_GRID), f32(cell / MEAN_GRID)) + 0.5) / f32(MEAN_GRID);
        sum += textureLoad(emissive_map, vec2<u32>(at * size), level).rgb;
    }
    sum_area[lid] = vec4<f32>(sum, 0.0);
    workgroupBarrier();
    for (var stride = GROUP_SIZE / 2u; stride > 0u; stride = stride / 2u) {
        if lid < stride {
            sum_area[lid] += sum_area[lid + stride];
        }
        workgroupBarrier();
    }
    if lid == 0u {
        partials[3u] = vec4<f32>(sum_area[0].rgb / f32(MEAN_GRID * MEAN_GRID), 1.0);
    }
}
