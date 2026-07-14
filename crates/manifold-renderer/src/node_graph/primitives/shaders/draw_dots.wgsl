// Hand-authored parity oracle for `node.draw_dots` (D3, BUG-114). NOT the
// runtime kernel — `run()` builds its pipeline from `standalone_for_spec`
// (see `shaders/draw_dots_body.wgsl`). Kept byte-for-byte identical to the
// pre-conversion kernel so the generated-vs-hand parity test proves the
// codegen path reproduces this exactly.

struct U {
    color: vec3<f32>,
    alpha: f32,
    radius_px: f32,
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

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    var src = textureSampleLevel(source_tex, src_sampler, uv, 0.0);

    let dpi_scale = f32(dims.y) / 1080.0;
    let radius = u.radius_px * (1.0 / f32(dims.x)) * dpi_scale;

    var coverage = 0.0;
    let n = arrayLength(&detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let center = vec2<f32>(d.x + d.width * 0.5, d.y + d.height * 0.5);
        let dist = length(uv - center);
        coverage = max(coverage, 1.0 - saturate(dist / radius));
    }

    let add = coverage * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
