// Hand-authored parity oracle for `node.draw_connections` (D3, BUG-114). NOT
// the runtime kernel — `run()` builds its pipeline from `standalone_for_spec`
// (see `shaders/draw_connections_body.wgsl`). Kept byte-for-byte identical
// to the pre-conversion kernel so the generated-vs-hand parity test proves
// the codegen path reproduces this exactly.

struct U {
    color: vec3<f32>,
    alpha: f32,
    thickness_px: f32,
    dash_period_px: f32,
    dash_fill: f32,
    midpoint_radius_px: f32,
};

struct Detection {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
};

struct Edge {
    a_index: u32,
    b_index: u32,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> detections: array<Detection>;
@group(0) @binding(2) var<storage, read> edges: array<Edge>;
@group(0) @binding(3) var source_tex: texture_2d<f32>;
@group(0) @binding(4) var src_sampler: sampler;
@group(0) @binding(5) var output_tex: texture_storage_2d<rgba16float, write>;

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
    let thickness = u.thickness_px * px_u;
    let dash_period = u.dash_period_px * px_u;
    let mid_radius = u.midpoint_radius_px * px_u;
    let det_count = arrayLength(&detections);

    var line_cov = 0.0;
    var mid_cov = 0.0;
    let n = arrayLength(&edges);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let e = edges[i];
        if e.a_index == 0xFFFFFFFFu { continue; }
        if e.a_index >= det_count || e.b_index >= det_count { continue; }
        let da = detections[e.a_index];
        let db = detections[e.b_index];
        let center_a = vec2<f32>(da.x + da.width * 0.5, da.y + da.height * 0.5);
        let center_b = vec2<f32>(db.x + db.width * 0.5, db.y + db.height * 0.5);

        let ba = center_b - center_a;
        let len_sq = dot(ba, ba);
        if len_sq < 0.000001 { continue; }
        let pa = uv - center_a;
        let t_val = saturate(dot(pa, ba) / len_sq);
        let len = sqrt(len_sq);
        let dash_phase = fract(t_val * len / dash_period);
        let dash_mask = step(u.dash_fill, dash_phase);

        line_cov = max(line_cov, line_seg(uv, center_a, center_b, thickness) * 0.5 * dash_mask);

        if mid_radius > 0.0 {
            let mid = (center_a + center_b) * 0.5;
            let mid_dist = length(uv - mid);
            mid_cov = max(mid_cov, (1.0 - saturate(mid_dist / mid_radius)) * 0.4);
        }
    }

    let add = (line_cov + mid_cov) * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
