// Surface weights: per lattice node, how much smoothing the surface gets
// (1 in bulk water, 0 on thin sheets and drops) and the water's velocity.
// A pure function of this frame's particles and the re-distanced field.
//
// Two passes, in command order. cs_counts gives every particle the number of
// live neighbours within count_reach. cs_weights gathers, for every node
// inside the re-distanced band, the poly6-weighted mean count and velocity of
// the particles within carry_reach, and fades the count into a weight.

struct Params {
    lattice_min: vec3<f32>,
    cell: f32,
    spacing: vec3<f32>,
    bin_size: f32,
    nodes: vec3<u32>,
    particle_capacity: u32,
    bins: vec3<i32>,
    node_count: u32,
    count_reach: f32,
    carry_reach: f32,
    band: f32,
    fade_start: f32,
    fade_end: f32,
    inv_bin: f32,
    pad_a: f32,
    pad_b: f32,
}

struct Particle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

struct CellRange {
    start: u32,
    count: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> cell_ranges: array<CellRange>;
@group(0) @binding(3) var<storage, read> reference: array<f32>;
@group(0) @binding(4) var<storage, read_write> counts: array<f32>;
@group(0) @binding(5) var<storage, read_write> weights: array<f32>;
@group(0) @binding(6) var<storage, read_write> velocity: array<f32>;

struct BinBox {
    lo: vec3<i32>,
    hi: vec3<i32>,
}

// The bins a ball of `reach` around `p` can touch, clamped to the grid. The
// sort bins by (p - min) * inv_bin; the slack covers rounding in that product
// so a searched range never falls one bin short. It only widens the search,
// the distance tests below stay exact.
fn bin_box(p: vec3<f32>, reach: f32) -> BinBox {
    let rel = p - params.lattice_min;
    let slack = 1.0e-5 * (abs(rel) + abs(params.lattice_min) + vec3<f32>(reach + params.bin_size));
    let top = params.bins - vec3<i32>(1);
    var reached: BinBox;
    reached.lo = clamp(vec3<i32>(floor((rel - vec3<f32>(reach) - slack) * params.inv_bin)), vec3<i32>(0), top);
    reached.hi = clamp(vec3<i32>(floor((rel + vec3<f32>(reach) + slack) * params.inv_bin)), vec3<i32>(0), top);
    return reached;
}

@compute @workgroup_size(64)
fn cs_counts(@builtin(global_invocation_id) gid: vec3<u32>) {
    let k = gid.x;
    if k >= params.particle_capacity {
        return;
    }
    let self_particle = particles[k];
    if !(self_particle.position_radius.w > 0.0) {
        counts[k] = 0.0;
        return;
    }
    let p = self_particle.position_radius.xyz;
    let reach2 = params.count_reach * params.count_reach;
    let search = bin_box(p, params.count_reach);
    var found = 0u;
    for (var z = search.lo.z; z <= search.hi.z; z = z + 1) {
        for (var y = search.lo.y; y <= search.hi.y; y = y + 1) {
            for (var x = search.lo.x; x <= search.hi.x; x = x + 1) {
                let range = cell_ranges[u32(x + params.bins.x * (y + params.bins.y * z))];
                for (var j = range.start; j < range.start + range.count; j = j + 1u) {
                    if j == k {
                        continue;
                    }
                    let other = particles[j];
                    if !(other.position_radius.w > 0.0) {
                        continue;
                    }
                    let d = other.position_radius.xyz - p;
                    if dot(d, d) < reach2 {
                        found = found + 1u;
                    }
                }
            }
        }
    }
    counts[k] = f32(found);
}

@compute @workgroup_size(64)
fn cs_weights(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = gid.x;
    if n >= params.node_count {
        return;
    }
    if abs(reference[n]) >= params.band {
        weights[n] = 0.0;
        velocity[3u * n] = 0.0;
        velocity[3u * n + 1u] = 0.0;
        velocity[3u * n + 2u] = 0.0;
        return;
    }
    let nx = params.nodes.x;
    let ny = params.nodes.y;
    let ijk = vec3<u32>(n % nx, (n / nx) % ny, n / (nx * ny));
    let x_node = params.lattice_min + vec3<f32>(ijk) * params.spacing;
    let carry2 = params.carry_reach * params.carry_reach;
    let search = bin_box(x_node, params.carry_reach);
    var sw = 0.0;
    var sn = 0.0;
    var sv = vec3<f32>(0.0);
    for (var z = search.lo.z; z <= search.hi.z; z = z + 1) {
        for (var y = search.lo.y; y <= search.hi.y; y = y + 1) {
            for (var x = search.lo.x; x <= search.hi.x; x = x + 1) {
                let range = cell_ranges[u32(x + params.bins.x * (y + params.bins.y * z))];
                for (var k = range.start; k < range.start + range.count; k = k + 1u) {
                    let particle = particles[k];
                    if !(particle.position_radius.w > 0.0) {
                        continue;
                    }
                    let d = particle.position_radius.xyz - x_node;
                    let r2 = dot(d, d) / carry2;
                    if !(r2 < 1.0) {
                        continue;
                    }
                    let t = 1.0 - r2;
                    let w = t * t * t;
                    sw = sw + w;
                    sn = sn + w * counts[k];
                    sv = sv + w * particle.velocity;
                }
            }
        }
    }
    var nbar = 0.0;
    var v = vec3<f32>(0.0);
    if sw > 0.0 {
        nbar = sn / sw;
        v = sv / sw;
    }
    weights[n] = clamp((nbar - params.fade_start) / (params.fade_end - params.fade_start), 0.0, 1.0);
    velocity[3u * n] = v.x;
    velocity[3u * n + 1u] = v.y;
    velocity[3u * n + 2u] = v.z;
}
