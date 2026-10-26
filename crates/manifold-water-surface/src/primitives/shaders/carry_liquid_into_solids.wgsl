// Carries the water field into drawn solids so the drawn surface meets a wall
// flush. This file declares all bindings and `clamp_liquid_solid_at`, the
// trilinear sample of the solid lattice in `buf_solid`.
//
// One thread per level-set node. Outside every solid the value is kept. Inside
// a solid whose water is carried (drawn, or every solid when no drawn mask is
// wired), a node within `carry_depth` of the solid's surface takes the level
// set sampled just inside the water at the surface point along the solid's
// normal, deeper nodes are air, and every other solid node is clipped to
// max(value, 0).

struct CarryParams {
    lattice_min: vec3<f32>,
    cell: f32,
    spacing: vec3<f32>,
    carry_depth: f32,
    nodes: vec3<u32>,
    node_count: u32,
    solid_spacing: vec3<f32>,
    air: f32,
    solid_nodes: vec3<u32>,
    drawn_len: u32,
}

@group(0) @binding(0) var<uniform> params: CarryParams;
@group(0) @binding(1) var<storage, read> buf_levelset: array<f32>;
@group(0) @binding(2) var<storage, read> buf_solid: array<f32>;
@group(0) @binding(3) var<storage, read> buf_drawn_solid: array<f32>;
@group(0) @binding(4) var<storage, read_write> buf_out: array<f32>;

fn clamp_liquid_solid_at(p: vec3<f32>, lattice_min: vec3<f32>, spacing: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let g = clamp((p - lattice_min) / spacing, vec3<f32>(0.0), vec3<f32>(nodes - vec3<u32>(1u)));
    let base = min(vec3<u32>(floor(g)), nodes - vec3<u32>(2u));
    let f = g - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let at = base + o;
        let w = select(1.0 - f.x, f.x, o.x == 1u)
            * select(1.0 - f.y, f.y, o.y == 1u)
            * select(1.0 - f.z, f.z, o.z == 1u);
        value = value + w * buf_solid[at.x + nodes.x * (at.y + nodes.y * at.z)];
    }
    return value;
}

fn carry_levelset_at(q: vec3<f32>) -> f32 {
    let g = clamp((q - params.lattice_min) / params.spacing, vec3<f32>(0.0), vec3<f32>(params.nodes - vec3<u32>(1u)));
    let base = min(vec3<u32>(floor(g)), params.nodes - vec3<u32>(2u));
    let f = g - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let at = base + o;
        let w = select(1.0 - f.x, f.x, o.x == 1u)
            * select(1.0 - f.y, f.y, o.y == 1u)
            * select(1.0 - f.z, f.z, o.z == 1u);
        value = value + w * buf_levelset[at.x + params.nodes.x * (at.y + params.nodes.y * at.z)];
    }
    return value;
}

fn carry_drawn_at(p: vec3<f32>) -> f32 {
    let nodes = params.solid_nodes;
    let g = clamp((p - params.lattice_min) / params.solid_spacing, vec3<f32>(0.0), vec3<f32>(nodes - vec3<u32>(1u)));
    let base = min(vec3<u32>(floor(g)), nodes - vec3<u32>(2u));
    let f = g - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let at = base + o;
        let w = select(1.0 - f.x, f.x, o.x == 1u)
            * select(1.0 - f.y, f.y, o.y == 1u)
            * select(1.0 - f.z, f.z, o.z == 1u);
        value = value + w * buf_drawn_solid[at.x + nodes.x * (at.y + nodes.y * at.z)];
    }
    return value;
}

fn carry_solid_at(p: vec3<f32>) -> f32 {
    return clamp_liquid_solid_at(p, params.lattice_min, params.solid_spacing, params.solid_nodes);
}

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.node_count { return; }
    let nodes = params.nodes;
    let ijk = vec3<u32>(idx % nodes.x, (idx / nodes.x) % nodes.y, idx / (nodes.x * nodes.y));
    let p = params.lattice_min + vec3<f32>(ijk) * params.spacing;
    let v = buf_levelset[idx];
    let sd = carry_solid_at(p);
    if sd >= 0.0 {
        buf_out[idx] = v;
        return;
    }
    var result = max(v, 0.0);
    let carried = params.carry_depth > 0.0 && (params.drawn_len == 0u || carry_drawn_at(p) < 0.0);
    if carried {
        if sd <= -params.carry_depth {
            result = params.air;
        } else {
            let h = 0.5 * params.solid_spacing;
            let grad = vec3<f32>(
                carry_solid_at(p + vec3<f32>(h.x, 0.0, 0.0)) - carry_solid_at(p - vec3<f32>(h.x, 0.0, 0.0)),
                carry_solid_at(p + vec3<f32>(0.0, h.y, 0.0)) - carry_solid_at(p - vec3<f32>(0.0, h.y, 0.0)),
                carry_solid_at(p + vec3<f32>(0.0, 0.0, h.z)) - carry_solid_at(p - vec3<f32>(0.0, 0.0, h.z)),
            );
            let len = length(grad);
            if len > 0.0 {
                let n = grad / len;
                result = carry_levelset_at(p - sd * n + 0.5 * params.cell * n);
            }
        }
    }
    buf_out[idx] = result;
}
