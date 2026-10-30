// node.matter_skin — a mesh carried by one Matter item's points
// (MATTER_DRAW_DESIGN.md, shared piece 3). Hand WGSL under exclusion 2: the
// bonds and the A/B ring of skinned vertices are cross-frame state.
//   bind_main: each rest vertex (world space, as the fill placed it) bonds to
//     its BONDS nearest seeded points of the item's fill box. The fill seeds
//     in cell order (matter_fill_body.wgsl), so a cell's points are found from
//     the fill table without a search structure. A bond keeps the point and
//     the vertex's offset from the point's seed, weighted by a kernel that
//     falls to zero past the farthest bond.
//   skin_main: after a tick, each vertex is where the affine map fitted to
//     its live bonds takes it: the weighted least-squares A, t taking the
//     bonds' seeds (relative to the vertex at rest) to their points now. The
//     fit reads positions only, so it averages out one point's jitter, and a
//     corner vertex, all of whose points lie on one side, carries their flat
//     faces on instead of one point's F bent at the corner. Where the seeds
//     span fewer than three directions the fit leans on the bonds' mean F
//     (rows f_x, f_y, f_z), which a cloud of points moved by F reproduces
//     exactly.
//   normal_main: each vertex's normal and tangent turn by the rotation that
//     takes its smoothing group's rest face normal to its deformed one, so
//     shading follows the carried shape (clay's plastic flow too, which F
//     does not hold) and the mesh's own hard edges and normals stay.
//   blend_main: the display frame between the last two ticks (the matter
//     frame's clock), written as MeshVertex records with the rest UVs.
// A tick whose stats hold a non-finite value repeats the previous tick, as
// node.matter_frame does.

const BONDS: u32 = 8u;
// The fill table's row width (matter_domain.rs FILL_ROW_WORDS).
const FILL_ROW: u32 = 8u;
// Rings of cells round the vertex's cell the bind searches before it gives
// up: thin parts of a mesh can sit a few cells from any seed.
const MAX_RING: i32 = 3;

struct MatterPoint {
    position: vec3<f32>,
    id: u32,
    velocity: vec3<f32>,
    volume_ratio: f32,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
    f_x: vec4<f32>,
    f_y: vec4<f32>,
    f_z: vec4<f32>,
    stress_diag: vec4<f32>,
    stress_off: vec4<f32>,
}

struct MeshVertex {
    position: vec3<f32>,
    pad0: f32,
    normal: vec3<f32>,
    pad1: f32,
    uv: vec2<f32>,
    uv1: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
}

// One skinned vertex of a tick: position (w 1), normal, tangent (w the rest
// handedness).
struct Skinned {
    position: vec4<f32>,
    normal: vec4<f32>,
    tangent: vec4<f32>,
}

struct SkinParams {
    vertices: u32,
    // The item's row of the fill table.
    slot: u32,
    points_per_cell: u32,
    // Records in `points` and in `rest`.
    point_count: u32,
    // The fill grid's first corner (matter_domain fill_min_*).
    fill_min: vec3<f32>,
    cell_size: f32,
    // A Box fill's box as authored; box_fill 1 maps it onto the box the
    // row seeds, its sides on cell lines.
    box_lo: vec3<f32>,
    box_fill: u32,
    box_hi: vec3<f32>,
    // The display blend between frames A and B.
    blend: f32,
    // 1 when the ring's previous slot holds the last tick.
    has_previous: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: SkinParams;
// The rest mesh, welded, in world space at the fill.
@group(0) @binding(1) var<storage, read> rest_vertices: array<MeshVertex>;
// node.matter_fill's seeds: each point where it was born.
@group(0) @binding(2) var<storage, read> seeds: array<MatterPoint>;
@group(0) @binding(3) var<storage, read> fills: array<u32>;
// Vertex v's bonds at v·BONDS: offset from the seed (xyz) and weight (w).
@group(0) @binding(4) var<storage, read_write> bonds: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> bond_points: array<u32>;
@group(0) @binding(6) var<storage, read> points: array<MatterPoint>;
@group(0) @binding(7) var<storage, read> stats: array<u32>;
@group(0) @binding(8) var<storage, read> previous: array<Skinned>;
@group(0) @binding(9) var<storage, read_write> skinned: array<Skinned>;
// Smoothing groups: vertex v's group, each group's rest face normal sum,
// and each group's triangles (CSR: group g's run in group_triangles is
// group_start[g] .. group_start[g + 1]).
@group(0) @binding(10) var<storage, read> vertex_group: array<u32>;
@group(0) @binding(11) var<storage, read> group_rest_normal: array<vec4<f32>>;
@group(0) @binding(12) var<storage, read> group_start: array<u32>;
@group(0) @binding(13) var<storage, read> group_triangles: array<u32>;
@group(0) @binding(14) var<storage, read> indices: array<u32>;
@group(0) @binding(15) var<storage, read> frame_a: array<Skinned>;
@group(0) @binding(16) var<storage, read> frame_b: array<Skinned>;
@group(0) @binding(17) var<storage, read_write> out: array<MeshVertex>;

@compute @workgroup_size(64)
fn bind_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let v = gid.x;
    if v >= params.vertices {
        return;
    }
    let row = params.slot * FILL_ROW;
    let lo = vec3<i32>(i32(fills[row]), i32(fills[row + 2u]), i32(fills[row + 4u]));
    let hi = vec3<i32>(i32(fills[row + 1u]), i32(fills[row + 3u]), i32(fills[row + 5u]));
    let first = fills[row + 7u];
    let size = hi - lo;
    let ppc = params.points_per_cell;
    var x = rest_vertices[v].position;
    // A Box fill seeds the box its sides round to: carry that box, so no
    // corner sits half a cell outside the material.
    if params.box_fill == 1u {
        let seeded_lo = params.fill_min + vec3<f32>(lo) * params.cell_size;
        let seeded_hi = params.fill_min + vec3<f32>(hi) * params.cell_size;
        let t = (x - params.box_lo) / max(params.box_hi - params.box_lo, vec3<f32>(1e-6));
        x = mix(seeded_lo, seeded_hi, t);
    }
    let home = vec3<i32>(floor((x - params.fill_min) / params.cell_size));
    var best_d: array<f32, 8>;
    var best_i: array<u32, 8>;
    for (var k = 0u; k < BONDS; k = k + 1u) {
        best_d[k] = 3.4e38;
        best_i[k] = 0xffffffffu;
    }
    var found = 0u;
    for (var ring = 1; ring <= MAX_RING && found == 0u; ring = ring + 1) {
        let c0 = max(home - vec3<i32>(ring), lo);
        let c1 = min(home + vec3<i32>(ring), hi - vec3<i32>(1));
        for (var cz = c0.z; cz <= c1.z; cz = cz + 1) {
            for (var cy = c0.y; cy <= c1.y; cy = cy + 1) {
                for (var cx = c0.x; cx <= c1.x; cx = cx + 1) {
                    let c = vec3<i32>(cx, cy, cz) - lo;
                    let cell = u32((c.z * size.y + c.y) * size.x + c.x);
                    let base = first + cell * ppc;
                    for (var s = 0u; s < ppc; s = s + 1u) {
                        let i = base + s;
                        if i >= params.point_count {
                            continue;
                        }
                        let seed = seeds[i];
                        if seed.id == 0u {
                            continue;
                        }
                        let d = distance(seed.position, x);
                        if d >= best_d[BONDS - 1u] {
                            continue;
                        }
                        // Insert, keeping the list sorted nearest first.
                        var k = BONDS - 1u;
                        while k > 0u && best_d[k - 1u] > d {
                            best_d[k] = best_d[k - 1u];
                            best_i[k] = best_i[k - 1u];
                            k = k - 1u;
                        }
                        best_d[k] = d;
                        best_i[k] = i;
                        found = found + 1u;
                    }
                }
            }
        }
    }
    // A kernel that reaches zero just past the farthest bond, so a bond that
    // would swap with the next nearest point weighs almost nothing.
    var far = 0.0;
    for (var k = 0u; k < BONDS; k = k + 1u) {
        if best_i[k] != 0xffffffffu {
            far = best_d[k];
        }
    }
    let reach = far * 1.25 + 1e-6;
    for (var k = 0u; k < BONDS; k = k + 1u) {
        let i = best_i[k];
        var bond = vec4<f32>(0.0);
        if i != 0xffffffffu {
            let q = best_d[k] / reach;
            let w = (1.0 - q * q);
            bond = vec4<f32>(x - seeds[i].position, w * w);
        }
        bonds[v * BONDS + k] = bond;
        bond_points[v * BONDS + k] = i;
    }
}

@compute @workgroup_size(64)
fn skin_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let v = gid.x;
    if v >= params.vertices {
        return;
    }
    let rest = rest_vertices[v];
    var held = Skinned(vec4<f32>(rest.position, 1.0), vec4<f32>(rest.normal, 0.0), rest.tangent);
    if params.has_previous == 1u {
        held = previous[v];
    }
    if stats[0] != 0u {
        skinned[v] = held;
        return;
    }
    // The weighted means of the seeds' offsets from the vertex (a bond holds
    // the vertex's offset from its seed, so the seed's is minus it), of the
    // points and of their F.
    var weight = 0.0;
    var seed_mean = vec3<f32>(0.0);
    var point_mean = vec3<f32>(0.0);
    var f_mean = mat3x3<f32>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
    for (var k = 0u; k < BONDS; k = k + 1u) {
        let bond = bonds[v * BONDS + k];
        let i = bond_points[v * BONDS + k];
        if i >= params.point_count || bond.w <= 0.0 {
            continue;
        }
        let p = points[i];
        if p.id == 0u {
            continue;
        }
        weight = weight + bond.w;
        seed_mean = seed_mean - bond.w * bond.xyz;
        point_mean = point_mean + bond.w * p.position;
        f_mean = f_mean + transpose(mat3x3<f32>(p.f_x.xyz, p.f_y.xyz, p.f_z.xyz)) * bond.w;
    }
    if weight <= 0.0 {
        skinned[v] = held;
        return;
    }
    seed_mean = seed_mean / weight;
    point_mean = point_mean / weight;
    f_mean = f_mean * (1.0 / weight);
    // Moments about the means: seeds with seeds, points with seeds.
    var seeds = mat3x3<f32>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
    var moved = mat3x3<f32>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
    for (var k = 0u; k < BONDS; k = k + 1u) {
        let bond = bonds[v * BONDS + k];
        let i = bond_points[v * BONDS + k];
        if i >= params.point_count || bond.w <= 0.0 {
            continue;
        }
        let p = points[i];
        if p.id == 0u {
            continue;
        }
        let d = -bond.xyz - seed_mean;
        let x = p.position - point_mean;
        seeds = seeds + outer(d * bond.w, d);
        moved = moved + outer(x * bond.w, d);
    }
    // A = (moved + λ F̄)(seeds + λ I)⁻¹: λ, a tenth of the seeds' mean
    // spread, decides only the directions the seeds do not span.
    let lambda = 0.1 * (seeds[0].x + seeds[1].y + seeds[2].z) / 3.0 + 1e-12;
    let eye = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    let a = (moved + f_mean * lambda) * inverse3(seeds + eye * lambda);
    skinned[v] = Skinned(vec4<f32>(point_mean - a * seed_mean, 1.0), held.normal, held.tangent);
}

// The outer product a bᵀ.
fn outer(a: vec3<f32>, b: vec3<f32>) -> mat3x3<f32> {
    return mat3x3<f32>(a * b.x, a * b.y, a * b.z);
}

// The inverse of `m`, from its columns' cross products; the fit's matrix is
// symmetric positive definite, its determinant above zero.
fn inverse3(m: mat3x3<f32>) -> mat3x3<f32> {
    let r0 = cross(m[1], m[2]);
    let r1 = cross(m[2], m[0]);
    let r2 = cross(m[0], m[1]);
    return transpose(mat3x3<f32>(r0, r1, r2)) * (1.0 / dot(m[0], r0));
}

// The face normal of triangle t, its length twice the area, from this tick's
// skinned positions.
fn face_normal(t: u32) -> vec3<f32> {
    let a = skinned[indices[3u * t]].position.xyz;
    let b = skinned[indices[3u * t + 1u]].position.xyz;
    let c = skinned[indices[3u * t + 2u]].position.xyz;
    return cross(b - a, c - a);
}

// `v` turned by the rotation that takes unit `a` onto unit `b`
// (Rodrigues); a half turn, which has no one axis, leaves it.
fn turn(v: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> vec3<f32> {
    let c = dot(a, b);
    if c <= -0.999 {
        return v;
    }
    let axis = cross(a, b);
    return v + cross(axis, v) + cross(axis, cross(axis, v)) / (1.0 + c);
}

@compute @workgroup_size(64)
fn normal_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let v = gid.x;
    if v >= params.vertices {
        return;
    }
    let g = vertex_group[v];
    var deformed = vec3<f32>(0.0);
    for (var j = group_start[g]; j < group_start[g + 1u]; j = j + 1u) {
        deformed = deformed + face_normal(group_triangles[j]);
    }
    let rest = rest_vertices[v];
    let was = group_rest_normal[g].xyz;
    // Only the normal and tangent: other threads read this vertex's position.
    if dot(deformed, deformed) > 1e-20 && dot(was, was) > 0.5 {
        let now = normalize(deformed);
        skinned[v].normal = vec4<f32>(normalize(turn(rest.normal, was, now)), 0.0);
        skinned[v].tangent = vec4<f32>(turn(rest.tangent.xyz, was, now), rest.tangent.w);
    }
}

@compute @workgroup_size(64)
fn blend_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let v = gid.x;
    if v >= params.vertices {
        return;
    }
    let a = frame_a[v];
    let b = frame_b[v];
    let t = params.blend;
    var m = rest_vertices[v];
    m.position = mix(a.position.xyz, b.position.xyz, t);
    let n = mix(a.normal.xyz, b.normal.xyz, t);
    if dot(n, n) > 1e-12 {
        m.normal = normalize(n);
    }
    let tangent = mix(a.tangent.xyz, b.tangent.xyz, t);
    if dot(tangent, tangent) > 1e-12 {
        m.tangent = vec4<f32>(normalize(tangent), m.tangent.w);
    }
    out[v] = m;
}
