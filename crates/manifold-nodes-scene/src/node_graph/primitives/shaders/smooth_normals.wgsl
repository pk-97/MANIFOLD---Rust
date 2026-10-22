// node.smooth_normals — welded, angle-weighted vertex normals for a flat
// triangle list. Three passes over one hash table keyed by exact position:
//   clear_main      zero every slot;
//   accumulate_main each vertex claims (or finds) the slot of its position and
//                   adds its triangle's face normal, weighted by the corner
//                   angle, in fixed point;
//   resolve_main    each vertex reads its slot's sum back as its normal.
// Every copy of a split corner (a cube's three face copies) lands in one slot,
// so all copies get the same normal: what Blender's vertex normal is.

struct Params {
    count: u32,
    slots: u32,
    _pad0: u32,
    _pad1: u32,
}

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    uv1: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
}

struct Slot {
    owner: atomic<u32>,
    x: atomic<i32>,
    y: atomic<i32>,
    z: atomic<i32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> src: array<MeshVertex>;
@group(0) @binding(2) var<storage, read_write> table: array<Slot>;
@group(0) @binding(3) var<storage, read_write> dst: array<MeshVertex>;

// Fixed-point scale of the summed normals: a full fan sums to at most 2*pi,
// far inside i32 at this scale.
const FIXED: f32 = 65536.0;
const MAX_PROBES: u32 = 64u;
const NOT_FOUND: u32 = 0xffffffffu;

// Exact bits of a position, with -0 folded into +0 so they match.
fn key_of(i: u32) -> vec3<u32> {
    let bits = bitcast<vec3<u32>>(src[i].position);
    return select(bits, vec3<u32>(0u), bits == vec3<u32>(0x80000000u));
}

fn hash(k: vec3<u32>) -> u32 {
    var h = (k.x * 73856093u) ^ (k.y * 19349663u) ^ (k.z * 83492791u);
    h = h ^ (h >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    return h;
}

// The slot holding vertex `i`'s position; `claim` takes an empty one.
fn find_slot(i: u32, claim: bool) -> u32 {
    let k = key_of(i);
    let mask = params.slots - 1u;
    var s = hash(k) & mask;
    for (var probe = 0u; probe < MAX_PROBES; probe = probe + 1u) {
        var owner = atomicLoad(&table[s].owner);
        if owner == 0u {
            if !claim {
                return NOT_FOUND;
            }
            let won = atomicCompareExchangeWeak(&table[s].owner, 0u, i + 1u);
            if won.exchanged {
                return s;
            }
            owner = won.old_value;
            if owner == 0u {
                // A spurious weak failure: try this slot again.
                continue;
            }
        }
        if all(key_of(owner - 1u) == k) {
            return s;
        }
        s = (s + 1u) & mask;
    }
    return NOT_FOUND;
}

@compute @workgroup_size(256)
fn clear_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if s >= params.slots {
        return;
    }
    atomicStore(&table[s].owner, 0u);
    atomicStore(&table[s].x, 0);
    atomicStore(&table[s].y, 0);
    atomicStore(&table[s].z, 0);
}

@compute @workgroup_size(256)
fn accumulate_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    let base = (i / 3u) * 3u;
    if i >= params.count || base + 2u >= params.count {
        return;
    }
    let corner = i - base;
    let p = src[i].position;
    let a = src[base + (corner + 1u) % 3u].position - p;
    let b = src[base + (corner + 2u) % 3u].position - p;
    var face = cross(src[base + 1u].position - src[base].position, src[base + 2u].position - src[base].position);
    let area = length(face);
    let la = length(a);
    let lb = length(b);
    if area <= 1e-12 || la <= 1e-12 || lb <= 1e-12 {
        return;
    }
    face = face / area;
    // Face the way the vertex's own normal does, whatever the winding.
    if dot(face, src[i].normal) < 0.0 {
        face = -face;
    }
    let angle = acos(clamp(dot(a, b) / (la * lb), -1.0, 1.0));
    let s = find_slot(i, true);
    if s == NOT_FOUND {
        return;
    }
    let add = vec3<i32>(round(face * angle * FIXED));
    atomicAdd(&table[s].x, add.x);
    atomicAdd(&table[s].y, add.y);
    atomicAdd(&table[s].z, add.z);
}

@compute @workgroup_size(256)
fn resolve_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    var v = src[i];
    let s = find_slot(i, false);
    if s != NOT_FOUND {
        let sum = vec3<f32>(
            f32(atomicLoad(&table[s].x)),
            f32(atomicLoad(&table[s].y)),
            f32(atomicLoad(&table[s].z)),
        );
        let len = length(sum);
        if len > 0.5 {
            v.normal = sum / len;
        }
    }
    dst[i] = v;
}
