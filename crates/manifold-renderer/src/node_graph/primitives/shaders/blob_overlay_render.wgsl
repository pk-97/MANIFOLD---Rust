// node.blob_overlay_render — draw hollow rectangles for each blob in
// the input Array<Blob> on top of a source Texture2D.
//
// Per-pixel approach: each output texel iterates the blob list and
// checks whether it sits on a blob's border (inside the box but not
// inside the smaller inset box). Border pixels get the overlay
// color blended at the configured alpha; interior + outside pixels
// pass the source through unchanged.
//
// O(width × height × MAX_BLOB_CAP) — fine because MAX_BLOB_CAP = 32
// and the inner test is just a handful of compares.

const MAX_BLOB_CAP: u32 = 32u;

struct Blob {
    x:      f32,
    y:      f32,
    width:  f32,
    height: f32,
};

struct Uniforms {
    overlay_color:  vec3<f32>,
    alpha:          f32,
    border_width:   f32,    // border thickness in UV units (e.g. 0.003 ≈ ~2px @ 720p)
    blob_count:     u32,    // valid blobs in src buffer
    _pad0:          u32,
    _pad1:          u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> blobs: array<Blob>;
@group(0) @binding(2) var source_tex: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    var src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    var on_border = false;
    let n = min(u.blob_count, MAX_BLOB_CAP);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let b = blobs[i];
        // Skip zero-size slots (inactive entries from the FFI worker).
        if b.width <= 0.0001 || b.height <= 0.0001 {
            continue;
        }
        let x0 = b.x;
        let y0 = b.y;
        let x1 = b.x + b.width;
        let y1 = b.y + b.height;
        let bw = u.border_width;
        let inside_outer = uv.x >= x0 && uv.x <= x1 && uv.y >= y0 && uv.y <= y1;
        let inside_inner = uv.x >= x0 + bw && uv.x <= x1 - bw
                        && uv.y >= y0 + bw && uv.y <= y1 - bw;
        if inside_outer && !inside_inner {
            on_border = true;
            break;
        }
    }

    if on_border {
        let a = u.alpha;
        src = vec4<f32>(
            mix(src.r, u.overlay_color.r, a),
            mix(src.g, u.overlay_color.g, a),
            mix(src.b, u.overlay_color.b, a),
            src.a,
        );
    }
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
