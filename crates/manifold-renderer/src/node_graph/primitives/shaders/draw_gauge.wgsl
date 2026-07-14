// Hand-authored parity oracle for `node.draw_gauge` (D3, BUG-114). NOT the
// runtime kernel — `run()` builds its pipeline from `standalone_for_spec`
// (see `shaders/draw_gauge_body.wgsl`). Kept byte-for-byte identical to the
// pre-conversion kernel so the generated-vs-hand parity test proves the
// codegen path reproduces this exactly.

struct U {
    color: vec3<f32>,
    alpha: f32,
    bottom_offset_px: f32,
    bar_height_px: f32,
    min_bar_width_px: f32,
    fill_scale: f32,
    thickness_px: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct Detection {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> detections: array<Detection>;
@group(0) @binding(2) var source_tex: texture_2d<f32>;
@group(0) @binding(3) var src_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

fn line_seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, thickness: f32) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let len_sq = dot(ba, ba);
    if len_sq < 0.000001 { return 0.0; }
    let h = saturate(dot(pa, ba) / len_sq);
    let d = length(pa - ba * h);
    return 1.0 - saturate(d / thickness);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    var src = textureSampleLevel(source_tex, src_sampler, uv, 0.0);

    let dpi_scale = f32(dims.y) / 1080.0;
    let px_u = (1.0 / f32(dims.x)) * dpi_scale;
    let px_v = (1.0 / f32(dims.y)) * dpi_scale;
    let thickness = u.thickness_px * px_u;
    let bar_height = u.bar_height_px * px_v;
    let bottom_offset = u.bottom_offset_px * px_v;
    let min_bar_w = u.min_bar_width_px * px_u;

    var coverage = 0.0;
    let n = arrayLength(&detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let half_size = vec2<f32>(d.width, d.height) * 0.5;
        let center = vec2<f32>(d.x + half_size.x, d.y + half_size.y);

        let origin = vec2<f32>(center.x - half_size.x, center.y + half_size.y + bottom_offset);
        let bar_w = max(d.width, min_bar_w);
        let fill_frac = saturate(d.width * d.height * u.fill_scale);

        let tl = origin;
        let tr = origin + vec2<f32>(bar_w, 0.0);
        let bl = origin + vec2<f32>(0.0, bar_height);
        let br = origin + vec2<f32>(bar_w, bar_height);
        coverage = max(coverage, line_seg(uv, tl, tr, thickness));
        coverage = max(coverage, line_seg(uv, bl, br, thickness));
        coverage = max(coverage, line_seg(uv, tl, bl, thickness));
        coverage = max(coverage, line_seg(uv, tr, br, thickness));

        let rel = uv - origin;
        if rel.x >= 0.0 && rel.x <= bar_w * fill_frac && rel.y >= 0.0 && rel.y <= bar_height {
            coverage = max(coverage, 0.4);
        }
    }

    let add = coverage * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
