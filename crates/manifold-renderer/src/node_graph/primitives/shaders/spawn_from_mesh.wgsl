// node.spawn_from_mesh — seed particles from a mesh's own geometry
// (Array<MeshVertex>) so an imported/procedural model can dissolve or
// explode into the existing 3D particle stack.
//
// Two modes, picked by `params.mode`:
//
//   vertices_main (mode 0) — one particle per vertex, exact silhouette.
//     Particle i = vertices[i].position for i < vertex_count; particles
//     past vertex_count (when active_count > vertex_count) are parked
//     dead. Single pass.
//
//   surface (mode 1) — area-weighted random triangle sampling, uniform
//     surface density regardless of triangulation. Three-pass dispatch,
//     same deterministic-scan spirit as seed_particles_from_texture.wgsl
//     but over triangles instead of mask texels (no atomics needed — the
//     scan here produces a monotonic cumulative-area table, not a
//     compacted index list, so there's nothing to race):
//       area_main  : one thread per triangle, writes its area into
//                    `cumulative[tri]` (raw, not yet summed).
//       scan_main  : single thread, turns `cumulative` into an inclusive
//                    prefix sum in place (cumulative[last] = total area).
//       place_main : for each active particle, draws r ~ U(0, total),
//                    binary-searches the cumulative table for the
//                    triangle it lands in, and barycentric-samples a
//                    point uniformly inside that triangle (sqrt trick).
//
// Positions are emitted in the mesh's LOCAL space — no transform is
// applied here (matches the mesh itself; a transform upstream of the
// renderer applies later, same convention as every other mesh atom).
//
// Vertices are consumed as flat triangle-list triples: triangle t reads
// vertices[t*3], vertices[t*3+1], vertices[t*3+2]. triangle_count =
// vertex_count / 3 (floor — a trailing partial triangle is ignored).

struct Params {
    mode: u32,          // 0 = vertices, 1 = surface
    active_count: u32,
    frame_seed: u32,
    vertex_count: u32,
    triangle_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read> vertices: array<MeshVertex>;
// Per-triangle scratch: raw area (area_main) rewritten in place to an
// inclusive prefix sum (scan_main). Unused (but still bound) in vertices
// mode.
@group(0) @binding(3) var<storage, read_write> cumulative: array<f32>;

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

fn dead_particle() -> Particle {
    var p: Particle;
    p.position = vec3<f32>(0.0, 0.0, 0.0);
    p.velocity = vec3<f32>(0.0, 0.0, 0.0);
    p.life = 0.0;
    p.age = -1.0;
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);
    return p;
}

// Pass (vertices mode) — one particle per vertex, exact silhouette.
@compute @workgroup_size(256, 1, 1)
fn vertices_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.active_count {
        return;
    }

    if i < params.vertex_count {
        var p: Particle;
        p.position = vertices[i].position;
        p.velocity = vec3<f32>(0.0, 0.0, 0.0);
        p.life = 1.0;
        p.age = -1.0;
        p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);
        particles[i] = p;
    } else {
        particles[i] = dead_particle();
    }
}

// Pass 1 (surface mode) — per-triangle area, one thread per triangle.
@compute @workgroup_size(64, 1, 1)
fn area_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let t = id.x;
    if t >= params.triangle_count {
        return;
    }
    let v0 = vertices[t * 3u].position;
    let v1 = vertices[t * 3u + 1u].position;
    let v2 = vertices[t * 3u + 2u].position;
    cumulative[t] = length(cross(v1 - v0, v2 - v0)) * 0.5;
}

// Pass 2 (surface mode) — single-thread inclusive prefix sum. Sequential,
// so the in-place read-then-overwrite is safe (no atomics required: this
// produces a monotonic lookup table, not a race-prone compacted list).
@compute @workgroup_size(1, 1, 1)
fn scan_main() {
    var acc = 0.0;
    for (var t = 0u; t < params.triangle_count; t = t + 1u) {
        acc = acc + cumulative[t];
        cumulative[t] = acc;
    }
}

// Pass 3 (surface mode) — area-weighted triangle pick + barycentric sample.
@compute @workgroup_size(256, 1, 1)
fn place_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.active_count {
        return;
    }

    if params.triangle_count == 0u {
        particles[i] = dead_particle();
        return;
    }

    let total = cumulative[params.triangle_count - 1u];
    if total <= 0.0 {
        // Degenerate mesh (zero surface area) — park alive on the first vertex
        // rather than silently dropping every particle.
        var p: Particle;
        p.position = vertices[0].position;
        p.velocity = vec3<f32>(0.0, 0.0, 0.0);
        p.life = 1.0;
        p.age = -1.0;
        p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);
        particles[i] = p;
        return;
    }

    let h1 = wang_hash(i ^ (params.frame_seed * 747796405u));
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
    let tri = lo;

    let v0 = vertices[tri * 3u].position;
    let v1 = vertices[tri * 3u + 1u].position;
    let v2 = vertices[tri * 3u + 2u].position;

    // Uniform barycentric sample within the triangle (sqrt trick):
    // r1, r2 ~ U(0,1) independent; u = 1 - sqrt(r1), v = r2 * sqrt(r1),
    // w = 1 - u - v gives a uniform point over the triangle's area.
    let h2 = wang_hash(h1);
    let r1 = hash_float(h2);
    let r2 = hash_float(wang_hash(h2));
    let sr1 = sqrt(r1);
    let bary_u = 1.0 - sr1;
    let bary_v = r2 * sr1;
    let bary_w = 1.0 - bary_u - bary_v;

    var p: Particle;
    p.position = v0 * bary_u + v1 * bary_v + v2 * bary_w;
    p.velocity = vec3<f32>(0.0, 0.0, 0.0);
    p.life = 1.0;
    p.age = -1.0;
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);
    particles[i] = p;
}
