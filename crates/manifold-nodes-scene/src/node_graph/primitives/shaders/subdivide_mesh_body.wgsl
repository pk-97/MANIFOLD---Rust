// node.subdivide_mesh — fusable BUFFER body (freeze section 12, buffer domain), GATHER.
// One thread per OUTPUT vertex. Output triangle t comes from source triangle
// t / 4^level; its sub-index picks one of four midpoint children per level,
// most significant level first (0, 1, 2 = the corner children of a, b, c;
// 3 = the centre child). Every child keeps the source winding. The vertex is
// the barycentric blend of its source triangle's three vertices, so every
// attribute interpolates; normals and tangent directions are renormalised.
//
// ABI: `in` is a BUFFER GATHER input — the body indexes `buf_in` directly.
// `in_len` is the derived input element count; `count` is the dispatch count
// (the output vertex count). A source triangle past `in_len` writes a zero
// vertex.
fn body(idx: u32, count: u32, level: i32, in_len: u32) -> Element {
    let lv = u32(clamp(level, 0, 8));
    let per = 1u << (2u * lv);
    let tri = idx / 3u;
    let corner = idx % 3u;
    let base = (tri / per) * 3u;
    if base + 2u >= in_len {
        return Element(vec3<f32>(0.0), vec3<f32>(0.0), vec2<f32>(0.0), vec2<f32>(0.0), vec4<f32>(0.0), vec4<f32>(1.0));
    }

    let sub = tri % per;
    var a = vec3<f32>(1.0, 0.0, 0.0);
    var b = vec3<f32>(0.0, 1.0, 0.0);
    var c = vec3<f32>(0.0, 0.0, 1.0);
    for (var i = 0u; i < lv; i = i + 1u) {
        let digit = (sub >> (2u * (lv - 1u - i))) & 3u;
        let ab = (a + b) * 0.5;
        let bc = (b + c) * 0.5;
        let ca = (c + a) * 0.5;
        if digit == 0u {
            b = ab;
            c = ca;
        } else if digit == 1u {
            a = ab;
            c = bc;
        } else if digit == 2u {
            a = ca;
            b = bc;
        } else {
            a = ab;
            b = bc;
            c = ca;
        }
    }
    var w = a;
    if corner == 1u {
        w = b;
    } else if corner == 2u {
        w = c;
    }

    let v0 = buf_in[base];
    let v1 = buf_in[base + 1u];
    let v2 = buf_in[base + 2u];
    let pos = v0.position * w.x + v1.position * w.y + v2.position * w.z;
    let n = v0.normal * w.x + v1.normal * w.y + v2.normal * w.z;
    let n_len = length(n);
    let normal = select(v0.normal, n / max(n_len, 1e-20), n_len > 1e-8);
    let uv = v0.uv * w.x + v1.uv * w.y + v2.uv * w.z;
    let uv1 = v0.uv1 * w.x + v1.uv1 * w.y + v2.uv1 * w.z;
    let t = v0.tangent.xyz * w.x + v1.tangent.xyz * w.y + v2.tangent.xyz * w.z;
    let t_len = length(t);
    let tangent = vec4<f32>(select(v0.tangent.xyz, t / max(t_len, 1e-20), t_len > 1e-8), v0.tangent.w);
    let color = v0.color * w.x + v1.color * w.y + v2.color * w.z;
    return Element(pos, normal, uv, uv1, tangent, color);
}
