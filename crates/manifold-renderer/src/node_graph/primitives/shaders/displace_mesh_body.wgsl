// node.displace_mesh — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT + REQUIRED TEXTURE. Per-vertex height displacement of a
// MeshVertex grid by a height Texture2D sampled at the vertex's own UV.
// Matches displace_mesh.wgsl.
//
// ABI: `in` (MeshVertex) coincident → e_in. NOT aliased — run() binds a
// separate src buffer to read slot 1 and the dst buffer to read_write slot 4
// (generated out global is `buf_out`). The REQUIRED `height` Texture2D is
// bound as `tex_height` + the shared `samp` (no use-flag — always present).
// `cols` / `rows` are Int params (passed i32) describing the source grid
// topology; vertices past cols*rows are inactive and pass through unchanged
// (return e_in reproduces the hand kernel's `dst[i] = src[i]`). Element = the
// MeshVertex struct {position, normal, uv}.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    tex_height: texture_2d<f32>,
    samp: sampler,
    cols: i32,
    rows: i32,
    displacement: f32,
    height_bias: f32,
) -> Element {
    let active_count = u32(cols) * u32(rows);
    if idx >= active_count {
        return e_in;
    }

    let uv = e_in.uv;
    let h_raw = textureSampleLevel(tex_height, samp, uv, 0.0).r;
    let displaced_y = e_in.position.y + (h_raw - height_bias) * displacement;

    return Element(
        vec3<f32>(e_in.position.x, displaced_y, e_in.position.z),
        e_in.normal,
        e_in.uv,
    );
}
