// node.facet_normals — fusable BUFFER body (freeze §12, buffer domain), GATHER.
// Per-VERTEX flat normal: thread `idx` reads its triangle's 3 verts from the
// input array global `buf_in` (base = 3*(idx/3)), computes the cross-product
// normal, and writes vertex `idx` with that normal (position + uv unchanged).
// A trailing partial triangle (base+2 >= count) passes through unchanged — no
// triangle to compute a normal from. GATHER form → fusion boundary (standalone
// single-source only, like neighbor_smooth). Matches facet_normals.wgsl.
//
// ABI: no coincident element arg (gather); the body indexes buf_in directly.
// `count` is the wrapper's dispatch_count = the total vertex count. No params.
fn body(idx: u32, count: u32) -> Element {
    let base = (idx / 3u) * 3u;
    let e_self = buf_in[idx];

    if base + 2u < count {
        let v0 = buf_in[base].position;
        let v1 = buf_in[base + 1u].position;
        let v2 = buf_in[base + 2u].position;
        let n = normalize(cross(v1 - v0, v2 - v0));
        return Element(e_self.position, n, e_self.uv);
    }

    return e_self;
}
