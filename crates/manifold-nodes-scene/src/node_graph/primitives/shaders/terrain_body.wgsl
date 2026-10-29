// node.terrain — fusable BUFFER body (freeze section 12, buffer domain), SOURCE.
// Lays a resolution × resolution grid of MeshVertex items across `size` metres
// in the XZ plane, centred on the origin, and lifts each by eroded-looking
// noise: value-noise fBm whose octaves fade where the land is already steep
// (Inigo Quilez's "Elevated" terrain, 2009). Normals are left facing up; the
// downstream `node.make_triangles` works them out from the lifted grid.
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the MeshVertex written to
// buf_vertices[idx]. Slots past resolution² are dead vertices.

// Each octave turns the domain so the lattices of successive octaves don't
// line up into visible grid rows.
const TERRAIN_TURN = mat2x2<f32>(vec2<f32>(0.8, 0.6), vec2<f32>(-0.6, 0.8));

// Integer hash of a lattice cell to [0, 1). Integer, not sin-based, so it
// holds at any distance from the origin.
fn terrain_hash(cell: vec2<i32>, seed: u32) -> f32 {
    var h = (bitcast<u32>(cell.x) * 0x8da6b343u) ^ (bitcast<u32>(cell.y) * 0xd8163841u) ^ (seed * 0xcb1ab31fu);
    h = (h ^ (h >> 16u)) * 0x7feb352du;
    h = (h ^ (h >> 15u)) * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return f32(h) * (1.0 / 4294967296.0);
}

// Value noise in [-1, 1] with quintic smoothing, and its gradient.
fn terrain_noise(p: vec2<f32>, seed: u32) -> vec3<f32> {
    let cell = vec2<i32>(floor(p));
    let w = fract(p);
    let u = w * w * w * (w * (w * 6.0 - 15.0) + 10.0);
    let du = 30.0 * w * w * (w * (w - 2.0) + 1.0);
    let a = terrain_hash(cell, seed);
    let b = terrain_hash(cell + vec2<i32>(1, 0), seed);
    let c = terrain_hash(cell + vec2<i32>(0, 1), seed);
    let d = terrain_hash(cell + vec2<i32>(1, 1), seed);
    let k1 = b - a;
    let k2 = c - a;
    let k4 = a - b - c + d;
    let value = -1.0 + 2.0 * (a + k1 * u.x + k2 * u.y + k4 * u.x * u.y);
    let gradient = 2.0 * du * vec2<f32>(k1 + k4 * u.y, k2 + k4 * u.x);
    return vec3<f32>(value, gradient);
}

// Height at `xz` metres, about [-1, 1]. Each octave halves the wavelength and
// scales by `roughness`; its share falls as the slope summed so far grows
// (`weathering` 0 is plain fBm). Octaves shorter than two grid spacings would
// alias, so each fades out between four and two spacings and the land doesn't
// pop as Size changes.
fn terrain_height(
    xz: vec2<f32>,
    feature_size: f32,
    roughness: f32,
    weathering: f32,
    spacing: f32,
    seed: u32,
) -> f32 {
    var p = xz / feature_size;
    var amplitude = 1.0;
    var wavelength = feature_size;
    var slope = vec2<f32>(0.0);
    var sum = 0.0;
    for (var octave = 0u; octave < 16u; octave++) {
        let fade = clamp(wavelength / (2.0 * spacing) - 1.0, 0.0, 1.0);
        if (fade <= 0.0) {
            break;
        }
        let n = terrain_noise(p, seed + octave * 0x9e3779b9u);
        slope += n.yz;
        sum += fade * amplitude * n.x / (1.0 + weathering * dot(slope, slope));
        amplitude *= roughness;
        wavelength *= 0.5;
        p = TERRAIN_TURN * p * 2.0;
    }
    // The full series 1 + r + r² + … sums to 1 / (1 - r).
    return sum * (1.0 - roughness);
}

fn body(
    idx: u32,
    count: u32,
    resolution: i32,
    size: f32,
    height: f32,
    feature_size: f32,
    roughness: f32,
    weathering: f32,
    seed: i32,
) -> Element {
    let res = u32(max(resolution, 2));
    if idx >= res * res {
        return Element(vec3<f32>(0.0), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(0.0), vec2<f32>(0.0), vec4<f32>(0.0), vec4<f32>(1.0));
    }
    let row = idx / res;
    let col = idx % res;
    let uv = vec2<f32>(f32(col), f32(row)) / f32(res - 1u);
    let xz = (uv - 0.5) * size;
    let spacing = size / f32(res - 1u);
    let y = height * terrain_height(xz, max(feature_size, 1e-3), roughness, weathering, spacing, bitcast<u32>(seed));
    return Element(vec3<f32>(xz.x, y, xz.y), vec3<f32>(0.0, 1.0, 0.0), uv, vec2<f32>(0.0), vec4<f32>(0.0), vec4<f32>(1.0));
}
