// node.scatter_on_mesh — scatter Array<InstanceTransform> across a mesh's
// surface, area-weighted so density is uniform regardless of triangulation.
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
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
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
    let v0 = vertices[t * 3u].position;
    let v1 = vertices[t * 3u + 1u].position;
    let v2 = vertices[t * 3u + 2u].position;
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

// Pass 3 — area-weighted triangle pick + barycentric sample; writes the
// instance's position/scale/rotation.
@compute @workgroup_size(256, 1, 1)
fn place_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.capacity {
        return;
    }
    // Slots beyond the live count park at zero scale so a lowered count
    // fader actually removes instances (see Params.capacity).
    if i >= params.count {
        park_zero(i);
        return;
    }
    if params.triangle_count == 0u {
        park_zero(i);
        return;
    }

    let total = cumulative[params.triangle_count - 1u];
    if total <= 0.0 {
        park_zero(i);
        return;
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
    let tri = lo;

    let p0 = vertices[tri * 3u].position;
    let p1 = vertices[tri * 3u + 1u].position;
    let p2 = vertices[tri * 3u + 2u].position;

    // Uniform barycentric sample within the triangle (sqrt trick), same as
    // spawn_from_mesh.wgsl's place_main.
    let h2 = wang_hash(h1);
    let r1 = hash_float(h2);
    let r2 = hash_float(wang_hash(h2));
    let sr1 = sqrt(r1);
    let bary_u = 1.0 - sr1;
    let bary_v = r2 * sr1;
    let bary_w = 1.0 - bary_u - bary_v;

    let surface_pos = p0 * bary_u + p1 * bary_v + p2 * bary_w;

    // Scale, hashed into [scale_min, scale_max].
    let h3 = wang_hash(h2);
    let scale_t = hash_float(h3);
    let scale = mix(params.scale_min, params.scale_max, scale_t);

    // Random yaw in [0, 2*PI).
    let h4 = wang_hash(h3);
    let yaw = hash_float(h4) * 6.28318530718;

    var euler: vec3<f32>;
    if params.align_to_normal != 0u {
        // The sampled triangle's flat face normal (not vertex-interpolated
        // — "the sampled triangle's normal" per the design, and it's
        // trivially correct on the flat triangle-list layout, same
        // reasoning as node.facet_normals).
        let raw_normal = cross(p1 - p0, p2 - p0);
        let n_len = length(raw_normal);
        var n = vec3<f32>(0.0, 1.0, 0.0);
        if n_len > 1.0e-8 {
            n = raw_normal / n_len;
        }
        // Orient to the mesh's DECLARED outward side: the winding-derived
        // face normal flips to the hemisphere of the triangle's own vertex
        // (shading) normals. Winding is not authoritative here — terrain
        // grids arrive with -Y winding but +Y vertex normals, and aligning
        // to the raw face normal planted every instance upside-down under
        // the ground. Zero/degenerate vertex normals leave the face normal
        // untouched.
        let n_vertex = vertices[tri * 3u].normal
            + vertices[tri * 3u + 1u].normal
            + vertices[tri * 3u + 2u].normal;
        if dot(n, n_vertex) < 0.0 {
            n = -n;
        }

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
    inst.pos_scale = vec4<f32>(surface_pos, scale);
    inst.rot_pad = vec4<f32>(euler, 0.0);
    instances[i] = inst;
}
