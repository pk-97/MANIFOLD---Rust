// node.shatter_mesh — fusable BUFFER body (freeze section 12, buffer domain), GATHER.
// Per-triangle face-normal explosion on a flat triangle list: thread `idx`
// reads its triangle's three vertices from `buf_in`, computes the face normal,
// and displaces all three verts along that normal by `amount * hash(tri_id)`.
// Output normals are set to the face normal. Trailing partial triangles pass
// through unchanged. `weights` is COINCIDENT.
//
// ABI: `in` is BufferGather, so the body indexes `buf_in` directly; `weights`
// is coincident (`e_weights`). `weights_len` is a derived uniform.
fn body(
    idx: u32,
    count: u32,
    e_weights: f32,
    amount: f32,
    seed: f32,
    weights_len: u32,
) -> Element {
    let self_v = buf_in[idx];
    let tri_id = idx / 3u;
    let base = tri_id * 3u;

    if base + 2u < count {
        let v0 = buf_in[base].position;
        let v1 = buf_in[base + 1u].position;
        let v2 = buf_in[base + 2u].position;
        let n = normalize(cross(v1 - v0, v2 - v0));

        let w = select(1.0, e_weights, idx < weights_len);
        let key = tri_id + u32(seed);
        let h = hash_u32(key);
        let displaced = self_v.position + n * (amount * h * w);
        return Element(displaced, n, self_v.uv, self_v.tangent);
    }

    return self_v;
}
