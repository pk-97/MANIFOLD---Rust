// node.polytope_vertices — fusable BUFFER body (freeze §12, buffer domain),
// SOURCE. Emit one of five Platonic-solid vertex sets as Array<MeshVertex>,
// normalised to radius 0.25 with outward-radial normals. Matches
// polytope_vertices.wgsl bit-for-bit (per-shape switch tables inlined, prefixed
// pv_/PV_ for fusion-collision safety).
//
// ABI (buffer standalone codegen): no array inputs → body(idx, count, <params>)
// returns the MeshVertex written to buf_vertices[idx]. The codegen synthesizes
//   struct Element { position: vec3<f32>, normal: vec3<f32>, uv: vec2<f32> }
// from MeshVertex's Channels signature. `dispatch_count` (= vert_capacity) is the
// wrapper guard; `nverts` is derived in-body from `shape` and slots idx >= nverts
// emit the degenerate padding vertex. `shape` is the Enum param (u32).
const PV_PHI: f32 = 1.618034;
const PV_INV_PHI: f32 = 0.618034;
const PV_POLYTOPE_DEFAULT_RADIUS: f32 = 0.25;

fn pv_tetra_vert(i: u32) -> vec3<f32> {
    switch i {
        case 0u: { return vec3<f32>( 1.0,  1.0,  1.0); }
        case 1u: { return vec3<f32>( 1.0, -1.0, -1.0); }
        case 2u: { return vec3<f32>(-1.0,  1.0, -1.0); }
        case 3u: { return vec3<f32>(-1.0, -1.0,  1.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn pv_cube_vert(i: u32) -> vec3<f32> {
    switch i {
        case 0u: { return vec3<f32>(-1.0, -1.0, -1.0); }
        case 1u: { return vec3<f32>( 1.0, -1.0, -1.0); }
        case 2u: { return vec3<f32>( 1.0,  1.0, -1.0); }
        case 3u: { return vec3<f32>(-1.0,  1.0, -1.0); }
        case 4u: { return vec3<f32>(-1.0, -1.0,  1.0); }
        case 5u: { return vec3<f32>( 1.0, -1.0,  1.0); }
        case 6u: { return vec3<f32>( 1.0,  1.0,  1.0); }
        case 7u: { return vec3<f32>(-1.0,  1.0,  1.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn pv_octa_vert(i: u32) -> vec3<f32> {
    switch i {
        case 0u: { return vec3<f32>( 1.0,  0.0,  0.0); }
        case 1u: { return vec3<f32>(-1.0,  0.0,  0.0); }
        case 2u: { return vec3<f32>( 0.0,  1.0,  0.0); }
        case 3u: { return vec3<f32>( 0.0, -1.0,  0.0); }
        case 4u: { return vec3<f32>( 0.0,  0.0,  1.0); }
        case 5u: { return vec3<f32>( 0.0,  0.0, -1.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn pv_icosa_vert(i: u32) -> vec3<f32> {
    switch i {
        case  0u: { return vec3<f32>(-1.0,  PV_PHI,  0.0); }
        case  1u: { return vec3<f32>( 1.0,  PV_PHI,  0.0); }
        case  2u: { return vec3<f32>(-1.0, -PV_PHI,  0.0); }
        case  3u: { return vec3<f32>( 1.0, -PV_PHI,  0.0); }
        case  4u: { return vec3<f32>( 0.0, -1.0,  PV_PHI); }
        case  5u: { return vec3<f32>( 0.0,  1.0,  PV_PHI); }
        case  6u: { return vec3<f32>( 0.0, -1.0, -PV_PHI); }
        case  7u: { return vec3<f32>( 0.0,  1.0, -PV_PHI); }
        case  8u: { return vec3<f32>( PV_PHI,  0.0, -1.0); }
        case  9u: { return vec3<f32>( PV_PHI,  0.0,  1.0); }
        case 10u: { return vec3<f32>(-PV_PHI,  0.0, -1.0); }
        case 11u: { return vec3<f32>(-PV_PHI,  0.0,  1.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn pv_dodeca_vert(i: u32) -> vec3<f32> {
    switch i {
        case  0u: { return vec3<f32>( 1.0,  1.0,  1.0); }
        case  1u: { return vec3<f32>( 1.0,  1.0, -1.0); }
        case  2u: { return vec3<f32>( 1.0, -1.0,  1.0); }
        case  3u: { return vec3<f32>( 1.0, -1.0, -1.0); }
        case  4u: { return vec3<f32>(-1.0,  1.0,  1.0); }
        case  5u: { return vec3<f32>(-1.0,  1.0, -1.0); }
        case  6u: { return vec3<f32>(-1.0, -1.0,  1.0); }
        case  7u: { return vec3<f32>(-1.0, -1.0, -1.0); }
        case  8u: { return vec3<f32>( 0.0,  PV_PHI,  PV_INV_PHI); }
        case  9u: { return vec3<f32>( 0.0,  PV_PHI, -PV_INV_PHI); }
        case 10u: { return vec3<f32>( 0.0, -PV_PHI,  PV_INV_PHI); }
        case 11u: { return vec3<f32>( 0.0, -PV_PHI, -PV_INV_PHI); }
        case 12u: { return vec3<f32>( PV_INV_PHI, 0.0,  PV_PHI); }
        case 13u: { return vec3<f32>( PV_INV_PHI, 0.0, -PV_PHI); }
        case 14u: { return vec3<f32>(-PV_INV_PHI, 0.0,  PV_PHI); }
        case 15u: { return vec3<f32>(-PV_INV_PHI, 0.0, -PV_PHI); }
        case 16u: { return vec3<f32>( PV_PHI,  PV_INV_PHI, 0.0); }
        case 17u: { return vec3<f32>( PV_PHI, -PV_INV_PHI, 0.0); }
        case 18u: { return vec3<f32>(-PV_PHI,  PV_INV_PHI, 0.0); }
        case 19u: { return vec3<f32>(-PV_PHI, -PV_INV_PHI, 0.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn pv_vert_count_for_shape(shape: u32) -> u32 {
    switch shape {
        case 0u: { return 4u; }
        case 1u: { return 8u; }
        case 2u: { return 6u; }
        case 3u: { return 12u; }
        case 4u: { return 20u; }
        default: { return 0u; }
    }
}

fn pv_vert_for_shape(shape: u32, i: u32) -> vec3<f32> {
    switch shape {
        case 0u: { return pv_tetra_vert(i); }
        case 1u: { return pv_cube_vert(i); }
        case 2u: { return pv_octa_vert(i); }
        case 3u: { return pv_icosa_vert(i); }
        case 4u: { return pv_dodeca_vert(i); }
        default: { return vec3<f32>(0.0); }
    }
}

fn body(idx: u32, count: u32, shape: u32) -> Element {
    let nverts = pv_vert_count_for_shape(shape);
    if idx >= nverts {
        return Element(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(0.0, 0.0));
    }

    let raw = pv_vert_for_shape(shape, idx);
    let len = length(raw);
    let pos = select(raw * (PV_POLYTOPE_DEFAULT_RADIUS / len), vec3<f32>(0.0), len < 1e-8);
    let normal = select(raw / len, vec3<f32>(0.0, 1.0, 0.0), len < 1e-8);
    let uv = vec2<f32>(f32(idx) / f32(max(nverts, 1u)), 0.0);

    return Element(pos, normal, uv);
}
