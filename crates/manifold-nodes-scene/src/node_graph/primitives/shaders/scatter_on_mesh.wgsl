// node.scatter_on_mesh — scatter Array<InstanceTransform> across a mesh's
// surface, area-weighted so density is uniform regardless of triangulation.
// The triangles are the vertices taken three at a time, or `indices` for an
// indexed mesh. The area table is summed only when the triangles change
// (`Summed` in scatter_on_mesh.rs); each copy's triangle and spot on it come
// from its own hash, so while the surface moves the copy rides it. A copy
// grows only where its CoverRule holds (cover_rule.wgsl, prepended), kept
// with that chance, so a soft edge thins out. On a Terrain's grid each copy
// takes a hashed place on its map instead (terrain_spot), so it rides the
// ground however it rises or wears; with Reach the copies fill cells round
// the camera (near_spot).
// Same 3-pass area/scan/place shape as spawn_from_mesh.wgsl (single-thread
// prefix-scan barrier between passes — the scan never reads its result back
// to the CPU, it stays on-GPU for the place pass exactly like the
// precedent).
//
// place_main writes:
//   pos_scale.xyz = a barycentric-sampled point on the mesh surface
//   pos_scale.w   = uniform scale, hashed into [scale_min, scale_max]
//   rot_pad.xyz   = an XYZ-Euler triple (radians, matches
//                   render_instanced_3d_mesh.wgsl's euler_xyz: R = Rz(rz) *
//                   Ry(ry) * Rx(rx)) — a random yaw about world Y when
//                   align_to_normal is off (Ry alone fixes (0,1,0), so the
//                   instance stays upright), or a yaw-then-align-to-normal
//                   composition when align_to_normal is on: build an
//                   orthonormal frame (tangent, normal, bitangent) with the
//                   sampled triangle's flat face normal as the "up" column
//                   and a random yaw rotating the tangent/bitangent pair
//                   about that normal, then decompose the frame's 3x3
//                   matrix into the (rx,ry,rz) triple the shader's own
//                   Rz*Ry*Rx convention expects. The decomposition formula
//                   (ry = asin(col0.z), rx = atan2(n.z, col2.z), rz =
//                   atan2(col0.y, col0.x)) is verified numerically against
//                   R = Rz(rz)*Ry(ry)*Rx(rx) in the P4 worklog — do not
//                   hand-edit the signs without re-deriving.

struct Params {
    count: u32,
    seed: u32,
    vertex_count: u32,
    triangle_count: u32,
    scale_min: f32,
    scale_max: f32,
    align_to_normal: u32,
    // Total instance slots in the output buffer. place_main runs over ALL
    // of them and parks slots >= count at zero scale — render_scene draws
    // buffer_size/32 instances unconditionally, so a stale tail (slots the
    // previous, higher count wrote) would otherwise stay on screen and the
    // count fader would appear dead.
    capacity: u32,
    // 1 when `indices` lists the triangles.
    indexed: u32,
    // A Terrain's vertices a side, 0 for any other mesh. Copies on a Terrain
    // take hashed places on its map (terrain_spot); on another mesh they
    // spread by area (surface_spot).
    grid: u32,
    // Reach: above 0, the copies fill a window of `cells` x `cells` cells
    // round the camera on a Terrain, full near it and none past `reach`
    // (near_spot).
    cells: u32,
    reach: f32,
    // Where copies grow: CoverRule's slope, height and breakup.
    rule_slope: vec4<f32>,
    rule_height: vec4<f32>,
    rule_breakup: vec4<f32>,
    // The camera in the mesh's own frame (xyz), for Reach.
    camera: vec4<f32>,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    uv1: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
};

struct InstanceTransform {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> instances: array<InstanceTransform>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read> vertices: array<MeshVertex>;
// Per-triangle scratch: raw area (area_main) rewritten in place to an
// inclusive prefix sum (scan_main). Never read back to the CPU — the scan
// stays on-GPU and feeds place_main directly, same shared-buffer contract
// as spawn_from_mesh.wgsl.
@group(0) @binding(3) var<storage, read_write> cumulative: array<f32>;
// The triangles' vertex index triples, read only when `indexed`.
@group(0) @binding(4) var<storage, read> indices: array<u32>;

// The vertex at corner `k` of triangle `t`.
fn corner(t: u32, k: u32) -> MeshVertex {
    if params.indexed != 0u {
        return vertices[indices[t * 3u + k]];
    }
    return vertices[t * 3u + k];
}

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn hash_float(seed: u32) -> f32 {
    return f32(wang_hash(seed)) / 4294967296.0;
}

fn park_zero(i: u32) {
    var inst: InstanceTransform;
    inst.pos_scale = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    inst.rot_pad = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    instances[i] = inst;
}

// Pass 1 — per-triangle area, one thread per triangle (identical to
// spawn_from_mesh.wgsl's area_main).
@compute @workgroup_size(64, 1, 1)
fn area_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let t = id.x;
    if t >= params.triangle_count {
        return;
    }
    let v0 = corner(t, 0u).position;
    let v1 = corner(t, 1u).position;
    let v2 = corner(t, 2u).position;
    cumulative[t] = length(cross(v1 - v0, v2 - v0)) * 0.5;
}

// Pass 2 — single-thread inclusive prefix sum (identical to
// spawn_from_mesh.wgsl's scan_main).
@compute @workgroup_size(1, 1, 1)
fn scan_main() {
    var acc = 0.0;
    for (var t = 0u; t < params.triangle_count; t = t + 1u) {
        acc = acc + cumulative[t];
        cumulative[t] = acc;
    }
}

// Standard XYZ-Euler extraction matching render_instanced_3d_mesh.wgsl's
// euler_xyz (R = Rz(rz) * Ry(ry) * Rx(rx)), given the target rotation's
// three columns (col1 is the "up" axis the frame aligns to).
fn euler_from_basis(col0: vec3<f32>, col1: vec3<f32>, col2: vec3<f32>) -> vec3<f32> {
    let ry = asin(clamp(col0.z, -1.0, 1.0));
    let rx = atan2(col1.z, col2.z);
    let rz = atan2(col0.y, col0.x);
    return vec3<f32>(rx, ry, rz);
}

// Reach: copies are full out to this share of it, then thin to none.
const REACH_FULL: f32 = 0.5;
// A copy grows from nothing to full size over this share of the fade.
const REACH_GROW: f32 = 0.25;

// Where a copy sits and what its surface says there.
struct Spot {
    pos: vec3<f32>,
    // The face normal of the triangle under the copy.
    normal: vec3<f32>,
    // The second UV set there: where a Terrain's water ran and soil settled.
    marks: vec2<f32>,
    // Its size against full; 0 parks it.
    size: f32,
    // The hash its size, turn and rule draws follow from.
    hash: u32,
};

// A triangle's flat face normal (not vertex-interpolated, and trivially
// correct on the flat triangle-list layout, same reasoning as
// node.facet_normals), oriented to the mesh's DECLARED outward side: it
// flips to the hemisphere of `vertex_normal`, the sum of the triangle's own
// vertex (shading) normals. Winding is not authoritative here — terrain
// grids arrive with -Y winding but +Y vertex normals, and aligning to the
// raw face normal planted every instance upside-down under the ground.
// Zero/degenerate vertex normals leave the face normal untouched.
fn face_normal(p0: vec3<f32>, p1: vec3<f32>, p2: vec3<f32>, vertex_normal: vec3<f32>) -> vec3<f32> {
    let raw_normal = cross(p1 - p0, p2 - p0);
    let n_len = length(raw_normal);
    var n = vec3<f32>(0.0, 1.0, 0.0);
    if n_len > 1.0e-8 {
        n = raw_normal / n_len;
    }
    if dot(n, vertex_normal) < 0.0 {
        n = -n;
    }
    return n;
}

// Copy `i` over the whole surface: an area-weighted triangle pick, then a
// uniform barycentric sample on it.
fn surface_spot(i: u32) -> Spot {
    var spot: Spot;
    // Slots beyond the live count park at zero scale so a lowered count
    // fader actually removes instances (see Params.capacity).
    if i >= params.count || params.triangle_count == 0u {
        return spot;
    }
    let total = cumulative[params.triangle_count - 1u];
    if total <= 0.0 {
        return spot;
    }

    let h1 = wang_hash(i ^ (params.seed * 747796405u));
    let r = hash_float(h1) * total;

    // Binary search: smallest triangle index whose cumulative area exceeds r.
    var lo = 0u;
    var hi = params.triangle_count - 1u;
    loop {
        if lo >= hi {
            break;
        }
        let mid = (lo + hi) / 2u;
        if cumulative[mid] > r {
            hi = mid;
        } else {
            lo = mid + 1u;
        }
    }
    let c0 = corner(lo, 0u);
    let c1 = corner(lo, 1u);
    let c2 = corner(lo, 2u);

    // Uniform barycentric sample within the triangle (sqrt trick), same as
    // spawn_from_mesh.wgsl's place_main.
    let h2 = wang_hash(h1);
    let r1 = hash_float(h2);
    let r2 = hash_float(wang_hash(h2));
    let sr1 = sqrt(r1);
    let w = vec3<f32>(1.0 - sr1, r2 * sr1, sr1 * (1.0 - r2));

    spot.pos = c0.position * w.x + c1.position * w.y + c2.position * w.z;
    spot.normal = face_normal(c0.position, c1.position, c2.position, c0.normal + c1.normal + c2.normal);
    spot.marks = c0.uv1 * w.x + c1.uv1 * w.y + c2.uv1 * w.z;
    spot.size = 1.0;
    spot.hash = h2;
    return spot;
}

// A Terrain grid's extent in the mesh's frame: its first vertex's xz, and
// the span from there to its last vertex's.
fn grid_extent() -> vec4<f32> {
    let origin = vertices[0].position.xz;
    let span = vertices[params.grid * params.grid - 1u].position.xz - origin;
    return vec4<f32>(origin, span);
}

// The point on a Terrain's grid over `xz` (in the mesh's frame), on the
// triangle grid_indices.wgsl cuts there: `params.grid` vertices a side,
// row-major, columns along x and rows along z (terrain.wgsl's
// vertices_main). Size 0 off the grid.
fn grid_spot(xz: vec2<f32>) -> Spot {
    var spot: Spot;
    let res = params.grid;
    let extent = grid_extent();
    if extent.z * extent.w == 0.0 {
        return spot;
    }
    let g = (xz - extent.xy) / extent.zw * f32(res - 1u);
    if any(g < vec2<f32>(0.0)) || any(g >= vec2<f32>(f32(res - 1u))) {
        return spot;
    }
    let c = vec2<u32>(g);
    let f = g - vec2<f32>(c);
    let corner00 = c.y * res + c.x;
    let corner01 = corner00 + res;
    let corner10 = corner00 + 1u;
    // Quad (c, r) is (c,r) (c,r+1) (c+1,r), then (c,r+1) (c+1,r+1) (c+1,r).
    var first_corner = corner00;
    var w = vec3<f32>(1.0 - f.x - f.y, f.y, f.x);
    if f.x + f.y > 1.0 {
        first_corner = corner01 + 1u;
        w = vec3<f32>(f.x + f.y - 1.0, 1.0 - f.x, 1.0 - f.y);
    }
    let v0 = vertices[first_corner];
    let v1 = vertices[corner01];
    let v2 = vertices[corner10];
    spot.pos = v0.position * w.x + v1.position * w.y + v2.position * w.z;
    spot.normal = face_normal(v0.position, v1.position, v2.position, v0.normal + v1.normal + v2.normal);
    spot.marks = v0.uv1 * w.x + v1.uv1 * w.y + v2.uv1 * w.z;
    spot.size = 1.0;
    return spot;
}

// Copy `i` over a whole Terrain: a hashed place on its map, so the copy rides
// the ground as it rises and wears and never moves across it.
fn terrain_spot(i: u32) -> Spot {
    if i >= params.count {
        var parked: Spot;
        return parked;
    }
    let h1 = wang_hash(i ^ (params.seed * 747796405u));
    let h2 = wang_hash(h1);
    let extent = grid_extent();
    var spot = grid_spot(extent.xy + extent.zw * vec2<f32>(hash_float(h1), hash_float(h2)));
    spot.hash = h2;
    return spot;
}

// Copy `i` round the camera on a Terrain: slot i holds one cell of a window
// `cells` wide centred on the camera's cell. A cell keeps its slot while the
// camera moves, and only the row leaving the window hands its slots to the
// row coming in, so copies never swim. Each cell's copy sits at a hashed
// place in it.
fn near_spot(i: u32) -> Spot {
    if i >= params.cells * params.cells {
        var parked: Spot;
        return parked;
    }
    let n = vec2<i32>(i32(params.cells));
    let side = 2.0 * params.reach / f32(params.cells);
    let first = vec2<i32>(floor(params.camera.xz / side)) - n / 2;
    let slot = vec2<i32>(i32(i % params.cells), i32(i / params.cells));
    let cell = first + ((slot - first) % n + n) % n;
    let h1 = wang_hash((bitcast<u32>(cell.x) * 73856093u) ^ (bitcast<u32>(cell.y) * 19349663u)
        ^ (params.seed * 747796405u));
    let h2 = wang_hash(h1);
    var spot = grid_spot((vec2<f32>(cell) + vec2<f32>(hash_float(h1), hash_float(h2))) * side);
    spot.hash = h2;
    // Count thins the window from full.
    if hash_float(h2 ^ 0x85ebca6bu) * f32(params.cells * params.cells) >= f32(params.count) {
        spot.size = 0.0;
    }
    // Each copy starts growing at its own point in the fade, so the edge
    // thins out instead of every copy shrinking at once.
    let near = 1.0 - smoothstep(REACH_FULL * params.reach, params.reach, distance(spot.pos, params.camera.xyz));
    let start = hash_float(h2 ^ 0x27d4eb2fu) * (1.0 - REACH_GROW);
    spot.size *= clamp((near - start) / REACH_GROW, 0.0, 1.0);
    return spot;
}

// Pass 3 — each copy's spot, size, turn and rule; writes its
// InstanceTransform.
@compute @workgroup_size(256, 1, 1)
fn place_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.capacity {
        return;
    }
    var spot: Spot;
    if params.reach > 0.0 {
        spot = near_spot(i);
    } else if params.grid > 0u {
        spot = terrain_spot(i);
    } else {
        spot = surface_spot(i);
    }
    if spot.size <= 0.0 {
        park_zero(i);
        return;
    }

    // Scale, hashed into [scale_min, scale_max].
    let h3 = wang_hash(spot.hash);
    let scale = mix(params.scale_min, params.scale_max, hash_float(h3)) * spot.size;

    // Random yaw in [0, 2*PI).
    let h4 = wang_hash(h3);
    let yaw = hash_float(h4) * 6.28318530718;
    let n = spot.normal;

    // Grow only where the rule holds, kept with its chance across a soft
    // edge.
    let rule = CoverRule(params.rule_slope, params.rule_height, params.rule_breakup);
    if hash_float(wang_hash(h4 ^ 0x9e3779b9u)) >= cover_rule_holds(rule, spot.pos, n.y, spot.marks) {
        park_zero(i);
        return;
    }

    var euler: vec3<f32>;
    if params.align_to_normal != 0u {
        // Build an orthonormal frame with `n` as the up column. `ref_axis`
        // is chosen so the tangent never nearly-parallels world Z when n is
        // near world-up (0,1,0) — the common terrain case — which keeps
        // col0.z away from +/-1 and avoids the ry = asin gimbal lock.
        var ref_axis = vec3<f32>(0.0, 0.0, 1.0);
        if abs(n.z) > 0.99 {
            ref_axis = vec3<f32>(1.0, 0.0, 0.0);
        }
        let tangent0 = normalize(cross(ref_axis, n));
        let bitangent0 = cross(tangent0, n);
        var col0 = cos(yaw) * tangent0 + sin(yaw) * bitangent0;
        col0 = normalize(col0);
        let col2 = cross(col0, n);
        euler = euler_from_basis(col0, n, col2);
    } else {
        // Ry alone leaves (0,1,0) fixed — pure yaw, instance stays upright.
        euler = vec3<f32>(0.0, yaw, 0.0);
    }

    var inst: InstanceTransform;
    inst.pos_scale = vec4<f32>(spot.pos, scale);
    inst.rot_pad = vec4<f32>(euler, 0.0);
    instances[i] = inst;
}
