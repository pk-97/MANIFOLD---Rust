// Mechanical port of PixelSortKeys.compute — luminance key extraction for pixel sort.
// Reads source texture, computes BT.601 luma, writes uint2(key, index) to sort buffer.
// Pixels below threshold or beyond sort dimension are masked with sentinel 0xFFFFFFFF.
//
// Unity ref: Assets/Resources/Compute/PixelSortKeys.compute — CSExtractKeys kernel.

struct KeyParams {
    padded_width: u32,   // _PaddedWidth — power-of-two padded sort dimension
    width: u32,          // _Width — actual sort dimension
    height: u32,         // _Height — number of independent sort instances
    sort_vertical: u32,  // _SortVertical — 0=horizontal, 1=vertical
    amount: f32,         // _Amount
    threshold: f32,      // _Threshold
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> params: KeyParams;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
// sort_buffer: vec2<u32> per element — x=key, y=original index
@group(0) @binding(3) var<storage, read_write> sort_buffer: array<vec2<u32>>;

// PixelSortKeys.compute line 21 — [numthreads(256, 1, 1)]
@compute @workgroup_size(256, 1, 1)
fn cs_extract_keys(@builtin(global_invocation_id) id: vec3<u32>) {
    let sort_idx = id.x;  // position within the sort dimension
    let row_idx  = id.y;  // which independent sort instance

    // PixelSortKeys.compute line 26
    if row_idx >= params.height { return; }

    // PixelSortKeys.compute line 28
    let buffer_idx = row_idx * params.padded_width + sort_idx;

    // PixelSortKeys.compute lines 31-35 — padding pixels beyond actual sort dimension
    if sort_idx >= params.width {
        sort_buffer[buffer_idx] = vec2<u32>(0xFFFFFFFFu, sort_idx);
        return;
    }

    // PixelSortKeys.compute lines 40-46 — compute UV based on sort direction
    // Horizontal: sortIdx=x, rowIdx=y. Vertical: sortIdx=y, rowIdx=x.
    var uv: vec2<f32>;
    if params.sort_vertical == 0u {
        uv = vec2<f32>(
            (f32(sort_idx) + 0.5) / f32(params.width),
            (f32(row_idx)  + 0.5) / f32(params.height),
        );
    } else {
        uv = vec2<f32>(
            (f32(row_idx)  + 0.5) / f32(params.height),
            (f32(sort_idx) + 0.5) / f32(params.width),
        );
    }

    // PixelSortKeys.compute line 49
    let color = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    // PixelSortKeys.compute line 52 — BT.601 luminance
    let lum = dot(color.rgb, vec3<f32>(0.299, 0.587, 0.114));

    // PixelSortKeys.compute lines 55-59 — threshold check
    if lum < params.threshold {
        sort_buffer[buffer_idx] = vec2<u32>(0xFFFFFFFFu, sort_idx);
        return;
    }

    // PixelSortKeys.compute line 62 — quantize luminance to 24-bit key
    // saturate(lum) * 16777215.0
    let key = u32(clamp(lum, 0.0, 1.0) * 16777215.0);

    // PixelSortKeys.compute line 64
    sort_buffer[buffer_idx] = vec2<u32>(key, sort_idx);
}
