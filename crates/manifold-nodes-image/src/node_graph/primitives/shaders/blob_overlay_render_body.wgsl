// `node.blob_overlay` fusable body (D3, BUG-114). The `blobs` port is tagged
// `BufferIndex` (`input_access: [Coincident, BufferIndex]`), so the codegen
// binds the storage global `buf_blobs: array<Element>` (element struct
// synthesized from the port's Channels[X, Y, WIDTH, HEIGHT] signature) and
// this body references it directly by name — no pre-read, no body arg,
// exactly `BufferGather`'s ABI, just hosted in a texture-domain kernel.
// Matches `blob_overlay_render.wgsl`'s per-blob border math verbatim.
// `blob_count` arrives as i32 (codegen maps ParamType::Int → i32); the hand
// shader's u32 comparisons are reproduced with an explicit cast.
const MAX_BLOB_CAP: u32 = 32u;

fn body(
    c_in: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    color: vec4<f32>,
    alpha: f32,
    border_width: f32,
    blob_count: i32,
) -> vec4<f32> {
    var on_border = false;
    let n = min(u32(max(blob_count, 0)), MAX_BLOB_CAP);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let b = buf_blobs[i];
        if b.width <= 0.0001 || b.height <= 0.0001 {
            continue;
        }
        let x0 = b.x;
        let y0 = b.y;
        let x1 = b.x + b.width;
        let y1 = b.y + b.height;
        let bw = border_width;
        let inside_outer = uv.x >= x0 && uv.x <= x1 && uv.y >= y0 && uv.y <= y1;
        let inside_inner = uv.x >= x0 + bw && uv.x <= x1 - bw
                        && uv.y >= y0 + bw && uv.y <= y1 - bw;
        if inside_outer && !inside_inner {
            on_border = true;
            break;
        }
    }

    if on_border {
        return vec4<f32>(
            mix(c_in.r, color.r, alpha),
            mix(c_in.g, color.g, alpha),
            mix(c_in.b, color.b, alpha),
            c_in.a,
        );
    }
    return c_in;
}
