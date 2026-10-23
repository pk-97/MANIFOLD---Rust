// node.drop_hidden_triangles — fusable BUFFER body, `in` is a GATHER input
// (each vertex reads its whole triangle, base = 3 * (idx / 3)). A triangle
// whose three vertex alphas are all at or below `cutoff` collapses onto its
// first vertex: zero area, so nothing draws, shadows or traces it, whatever
// the material's alpha mode. Every vertex of one triangle makes the same
// call, so the triangle collapses whole. A trailing partial triangle passes
// through.
fn body(idx: u32, count: u32, cutoff: f32) -> Element {
    let base = (idx / 3u) * 3u;
    let e_self = buf_in[idx];
    if base + 2u >= count {
        return e_self;
    }
    let a = max(buf_in[base].color.a, max(buf_in[base + 1u].color.a, buf_in[base + 2u].color.a));
    if a > cutoff {
        return e_self;
    }
    return Element(buf_in[base].position, e_self.normal, e_self.uv, e_self.uv1, e_self.tangent, e_self.color);
}
