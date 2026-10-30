// node.shape_particle_blobs — one surface blob per sorted liquid particle,
// shaped by its neighbours (Yu and Turk 2013, "Reconstructing surfaces of
// particle-based fluids using anisotropic kernels"). One thread per sorted
// slot; an inactive particle (radius 0) writes zeros.
//
// The neighbours are the particles within a simulation cell
// (SPB_NEIGHBOUR_CELLS), weighted 1 − (d / that)³ and found through the
// display cells they are sorted into (`cell_ranges`, one range per pool
// element). From them:
//   - the centre moves SPB_CENTRE_SMOOTHING of the way to their weighted
//     mean, so the particles' own jitter is smoothed before any field is
//     built;
//   - their weighted spread about that mean gives the blob's axes: a sheet's
//     particles spread along it and not across, so its blobs flatten into it
//     and join into a smooth sheet instead of a row of beads; the shortest
//     axis stays at least 1 / SPB_AXIS_RATIO of the longest, and the axes
//     keep the round blob's volume;
//   - with fewer than SPB_FULL_NEIGHBOURS the stretch and the smoothing fade
//     out.
// A blob that blends with no other (no particle within its blend radius,
// node.particle_volume's 1.5 round radii) shrinks toward the particle's own
// radius, so a lone drop draws at its true size instead of Particle Scale
// times it, while a thinned sheet, whose particles sit one or two cells
// apart, keeps its size and joins up. Only a particle short of a full
// neighbourhood looks that far.
// The stretch is at most SPB_MAX_STRETCH. A lone drop's semi-axes are at
// least SPB_MIN_AXIS_CELLS display cells (thinner falls between the
// display's nodes); a joined blob's at least that many simulation cells, or
// its round radius if smaller, so a sheet is as thick at every Surface Detail
// and a finer one only draws it sharper.
//
// `counts` gets each slot's neighbours within the simulation cell (0 for an
// inactive one): node.particle_volume blends them into how much each node is
// smoothed.
//
// The record (FluidBlob) holds the centre and round radius r = Particle
// Scale × the particle's radius, the symmetric shape matrix A = V ·
// diag(r / a) · Vᵀ, a the semi-axes along V's columns (a point at offset y
// from the centre is inside the blob when |A y| < r), and the shrink `size`.
// node.particle_volume weighs the blob by |size · A y|, the stretch alone, so
// a shrunk blob still blends with neighbours as far off as a full one: sparse
// particles join into a thin film instead of a lace of beads.

struct Params {
    // The display cell, metres.
    cell_size: f32,
    sim_cell_size: f32,
    particle_scale: f32,
    count: u32,
    pool_tiles: u32,
    _pad_h: u32,
    _pad0: u32,
    _pad1: u32,
}

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

struct CellRange {
    start: u32,
    count: u32,
}

struct FluidBlob {
    center_radius: vec4<f32>,
    shape_diagonal: vec4<f32>,
    shape_off_diagonal: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> sorted: array<FluidParticle>;
@group(0) @binding(2) var<storage, read> cell_ranges: array<CellRange>;
@group(0) @binding(3) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(4) var<storage, read_write> blobs: array<FluidBlob>;
@group(0) @binding(5) var<storage, read_write> counts: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// field_build::BLOB_NEIGHBOUR_CELLS, simulation cells.
const SPB_NEIGHBOUR_CELLS: f32 = 1.0;
// field_build::BLOB_FULL_NEIGHBOURS.
const SPB_FULL_NEIGHBOURS: f32 = 3.0;
// field_build::BLOB_JOINED_NEIGHBOURS.
const SPB_JOINED_NEIGHBOURS: f32 = 2.0;
// field_build::BLOB_MAX_STRETCH.
const SPB_MAX_STRETCH: f32 = 2.0;
// field_build::BLOB_AXIS_RATIO.
const SPB_AXIS_RATIO: f32 = 4.0;
// field_build::BLOB_CENTRE_SMOOTHING.
const SPB_CENTRE_SMOOTHING: f32 = 0.9;
// field_build::BLOB_MIN_AXIS_CELLS, display cells for a lone drop and
// simulation cells for a joined blob.
const SPB_MIN_AXIS_CELLS: f32 = 0.75;
// Cyclic Jacobi sweeps: a 3×3 symmetric matrix converges in far fewer.
const SPB_SWEEPS: u32 = 6u;

// One Jacobi rotation zeroing a[p][q] of the symmetric `a`, folded into the
// eigenvectors `v` (Numerical Recipes' jacobi, columns are eigenvectors).
fn spb_rotate(a: ptr<function, mat3x3<f32>>, v: ptr<function, mat3x3<f32>>, p: u32, q: u32) {
    let apq = (*a)[q][p];
    if abs(apq) < 1e-20 {
        return;
    }
    let theta = ((*a)[q][q] - (*a)[p][p]) / (2.0 * apq);
    let t = sign(theta) / (abs(theta) + sqrt(theta * theta + 1.0)) + select(0.0, 1.0, theta == 0.0);
    let c = 1.0 / sqrt(t * t + 1.0);
    let s = t * c;
    var j = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    j[p][p] = c;
    j[q][q] = c;
    j[q][p] = s;
    j[p][q] = -s;
    *a = transpose(j) * (*a) * j;
    *v = (*v) * j;
}

// The other live particles within `radius` of `own` (slot `i`), through the
// tiles round the home tile.
fn spb_others_within(own: vec3<f32>, i: u32, radius: f32, h: f32) -> f32 {
    let lo = vec3<i32>(floor((own - vec3<f32>(radius)) / h + vec3<f32>(0.5)));
    let hi = vec3<i32>(floor((own + vec3<f32>(radius)) / h + vec3<f32>(0.5)));
    var count = 0.0;
    for (var tz = lo.z >> 3u; tz <= hi.z >> 3u; tz = tz + 1) {
        for (var ty = lo.y >> 3u; ty <= hi.y >> 3u; ty = ty + 1) {
            for (var tx = lo.x >> 3u; tx <= hi.x >> 3u; tx = tx + 1) {
                let tile = vec3<i32>(tx, ty, tz);
                let slot = ts_slot_of(tile);
                if slot == WT_NONE {
                    continue;
                }
                let a = max(lo, tile * 8);
                let c = min(hi, tile * 8 + vec3<i32>(7));
                for (var z = a.z; z <= c.z; z = z + 1) {
                    for (var y = a.y; y <= c.y; y = y + 1) {
                        for (var x = a.x; x <= c.x; x = x + 1) {
                            let range = cell_ranges[slot * 512u + ts_local(vec3<i32>(x, y, z))];
                            for (var b = range.start; b < range.start + range.count; b = b + 1u) {
                                let other = sorted[b].position_radius;
                                if other.w > 0.0 && b != i && distance(other.xyz, own) < radius {
                                    count = count + 1.0;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    return count;
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    let own = sorted[i].position_radius;
    if !(own.w > 0.0) {
        blobs[i] = FluidBlob(vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0));
        counts[i] = 0.0;
        return;
    }
    let h = params.cell_size;
    let reach = SPB_NEIGHBOUR_CELLS * params.sim_cell_size;
    // The display cells holding every particle within `reach`: cell p spans
    // (p - 1/2, p + 1/2) cells, as the sort keys it.
    let lo = vec3<i32>(floor((own.xyz - vec3<f32>(reach)) / h + vec3<f32>(0.5)));
    let hi = vec3<i32>(floor((own.xyz + vec3<f32>(reach)) / h + vec3<f32>(0.5)));
    // The particle's own tile is home, so the tiles round it (a simulation
    // cell is at most three display cells, inside the next tile) come from
    // its neighbour row; each tile's slot is looked up once.
    ts_home(ts_cell_at(vec3<i32>(floor(own.xyz / h + vec3<f32>(0.5)))));
    let tile_lo = lo >> vec3<u32>(3u);
    let tile_hi = hi >> vec3<u32>(3u);
    // Weighted sums of the offsets d = x_j − x_i and of d dᵀ, the particle
    // itself included (weight 1), so the mean and spread come from one pass.
    var weight = 0.0;
    var first = vec3<f32>(0.0);
    var second = mat3x3<f32>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
    var neighbours = 0.0;
    for (var tz = tile_lo.z; tz <= tile_hi.z; tz = tz + 1) {
        for (var ty = tile_lo.y; ty <= tile_hi.y; ty = ty + 1) {
            for (var tx = tile_lo.x; tx <= tile_hi.x; tx = tx + 1) {
                let tile = vec3<i32>(tx, ty, tz);
                let slot = ts_slot_of(tile);
                if slot == WT_NONE {
                    continue;
                }
                let a = max(lo, tile * 8);
                let c = min(hi, tile * 8 + vec3<i32>(7));
                for (var z = a.z; z <= c.z; z = z + 1) {
                    for (var y = a.y; y <= c.y; y = y + 1) {
                        for (var x = a.x; x <= c.x; x = x + 1) {
                            let range = cell_ranges[slot * 512u + ts_local(vec3<i32>(x, y, z))];
                            for (var b = range.start; b < range.start + range.count; b = b + 1u) {
                                let other = sorted[b].position_radius;
                                if !(other.w > 0.0) {
                                    continue;
                                }
                                let d = other.xyz - own.xyz;
                                let s = length(d) / reach;
                                if s >= 1.0 {
                                    continue;
                                }
                                let w = 1.0 - s * s * s;
                                weight = weight + w;
                                first = first + w * d;
                                second = second + w * mat3x3<f32>(d * d.x, d * d.y, d * d.z);
                                neighbours = neighbours + select(1.0, 0.0, b == i);
                            }
                        }
                    }
                }
            }
        }
    }
    counts[i] = neighbours;
    let mean = first / weight;
    let spread = second * (1.0 / weight)
        - mat3x3<f32>(mean * mean.x, mean * mean.y, mean * mean.z);
    // 0 alone, 1 with a full neighbourhood.
    let fill = smoothstep(0.0, SPB_FULL_NEIGHBOURS, neighbours);
    let centre = own.xyz + (SPB_CENTRE_SMOOTHING * fill) * mean;

    var a = spread;
    var v = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    for (var sweep = 0u; sweep < SPB_SWEEPS; sweep = sweep + 1u) {
        spb_rotate(&a, &v, 0u, 1u);
        spb_rotate(&a, &v, 0u, 2u);
        spb_rotate(&a, &v, 1u, 2u);
    }
    // Standard deviations along the eigenvectors, the shortest at least
    // 1 / SPB_AXIS_RATIO of the longest, scaled to keep the volume.
    var sigma = sqrt(max(vec3<f32>(a[0][0], a[1][1], a[2][2]), vec3<f32>(0.0)));
    let longest = max(max(sigma.x, sigma.y), sigma.z);
    var stretch = vec3<f32>(1.0);
    if longest > 0.0 {
        sigma = max(sigma, vec3<f32>(longest / SPB_AXIS_RATIO));
        stretch = sigma / pow(sigma.x * sigma.y * sigma.z, 1.0 / 3.0);
    }
    let r = params.particle_scale * own.w;
    // The others the blob blends with; a full neighbourhood has them.
    var blended = neighbours;
    if neighbours < SPB_FULL_NEIGHBOURS {
        blended = spb_others_within(own.xyz, i, max(1.5 * r, reach), h);
    }
    // The blob's size over the round blob's: the particle's own radius when
    // nothing blends with it, the round blob once SPB_JOINED_NEIGHBOURS do,
    // never under the thinnest the display shows (above 1 for a blob smaller
    // than that).
    let thinnest = SPB_MIN_AXIS_CELLS * h / r;
    let joined = smoothstep(0.0, SPB_JOINED_NEIGHBOURS, blended);
    let size = max(mix(max(own.w / r, thinnest), 1.0, joined), thinnest);
    let sheet_axis = max(min(SPB_MIN_AXIS_CELLS * params.sim_cell_size, r), SPB_MIN_AXIS_CELLS * h);
    let least_axis = mix(SPB_MIN_AXIS_CELLS * h, sheet_axis, joined);
    let axes = max(
        r * size * min(mix(vec3<f32>(1.0), stretch, fill), vec3<f32>(SPB_MAX_STRETCH)),
        vec3<f32>(least_axis),
    );
    let inverse = r / axes;
    let shape = v * mat3x3<f32>(
        vec3<f32>(inverse.x, 0.0, 0.0),
        vec3<f32>(0.0, inverse.y, 0.0),
        vec3<f32>(0.0, 0.0, inverse.z),
    ) * transpose(v);
    // How far the weight |size · A y| reaches over the round blob's: the
    // longest semi-axis over r · size, at most SPB_MAX_STRETCH (r · size is
    // never under the least axis).
    let weight_reach = max(max(axes.x, axes.y), axes.z) / (r * size);
    blobs[i] = FluidBlob(
        vec4<f32>(centre, r),
        vec4<f32>(shape[0][0], shape[1][1], shape[2][2], weight_reach),
        vec4<f32>(shape[1][0], shape[2][0], shape[2][1], size),
    );
}
