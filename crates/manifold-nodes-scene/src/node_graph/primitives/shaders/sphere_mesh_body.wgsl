// node.sphere_mesh — fusable BUFFER body (freeze section 12, buffer domain),
// SOURCE. Emit a smooth UV sphere of `radius` about the origin as a triangle
// list: a fan at each pole and two triangles per quad between, `segments`
// around and `rings` from pole to pole, 6 * segments * (rings - 1) vertices,
// outward wound. Normals point out from the centre, UVs are longitude and
// latitude (a pole vertex takes the middle of its segment), and the tangent
// runs along the longitude. The CPU mirror is `mesh_source::sphere_vertex`.
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the MeshVertex written to
// buf_vertices[idx]. Slots past the sphere are the degenerate padding vertex
// the other mesh sources write. Helpers are prefixed (sm_) to stay
// collision-safe under multi-atom fusion.
const SM_TAU: f32 = 6.283185307179586;
const SM_PI: f32 = 3.141592653589793;

fn body(idx: u32, count: u32, segments: i32, rings: i32, radius: f32) -> Element {
    let segs = u32(max(segments, 3));
    let rs = u32(max(rings, 2));
    let total = 2u * segs * (rs - 1u);
    if idx >= total * 3u {
        return Element(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(0.0, 0.0), vec2<f32>(0.0, 0.0), vec4<f32>(0.0), vec4<f32>(1.0));
    }
    let tri = idx / 3u;
    let corner = idx % 3u;
    var ring = 0u;
    var seg = 0u;
    var pole = false;
    if tri < segs {
        let s = tri;
        if corner == 0u { ring = 0u; seg = s; pole = true; }
        else if corner == 1u { ring = 1u; seg = s + 1u; }
        else { ring = 1u; seg = s; }
    } else if tri >= total - segs {
        let s = tri - (total - segs);
        if corner == 0u { ring = rs - 1u; seg = s; }
        else if corner == 1u { ring = rs - 1u; seg = s + 1u; }
        else { ring = rs; seg = s; pole = true; }
    } else {
        let band = tri - segs;
        let quad = band / 2u;
        let r = 1u + quad / segs;
        let s = quad % segs;
        if band % 2u == 0u {
            if corner == 0u { ring = r; seg = s; }
            else if corner == 1u { ring = r; seg = s + 1u; }
            else { ring = r + 1u; seg = s + 1u; }
        } else {
            if corner == 0u { ring = r; seg = s; }
            else if corner == 1u { ring = r + 1u; seg = s + 1u; }
            else { ring = r + 1u; seg = s; }
        }
    }
    var u = f32(seg) / f32(segs);
    if pole {
        u = (f32(seg) + 0.5) / f32(segs);
    }
    let v = f32(ring) / f32(rs);
    let phi = SM_TAU * u;
    let theta = SM_PI * v;
    var normal = vec3<f32>(sin(theta) * cos(phi), cos(theta), sin(theta) * sin(phi));
    if ring == 0u {
        normal = vec3<f32>(0.0, 1.0, 0.0);
    } else if ring == rs {
        normal = vec3<f32>(0.0, -1.0, 0.0);
    }
    let tangent = vec4<f32>(-sin(phi), 0.0, cos(phi), 1.0);
    return Element(normal * radius, normal, vec2<f32>(u, v), vec2<f32>(0.0, 0.0), tangent, vec4<f32>(1.0));
}
