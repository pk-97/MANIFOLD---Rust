// Pixel sort visualization — compute dispatch variant.
// Identical math to pixel_sort_visualize.wgsl. Only the input/output mechanism changes:
//   - textureSampleLevel instead of textureSample
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment
//
// Bindings: uniform, source_tex, sampler, sort_buffer (read), output_tex (write)

struct VizParams {
    padded_width:  u32,  // _PaddedWidth — padded sort dimension (power of 2)
    width:         u32,  // _Width — actual sort dimension
    height:        u32,  // _Height — number of independent sort instances
    sort_vertical: u32,  // _SortVertical — 0=horizontal, 1=vertical
    amount:        f32,  // _Amount
    _pad0:         f32,
    _pad1:         f32,
    _pad2:         f32,
}

@group(0) @binding(0) var<uniform> params: VizParams;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var<storage, read> sort_buffer: array<vec2<u32>>;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // ComputePixelSortVisualize.shader line 56 — original color at this pixel
    let original = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    // ComputePixelSortVisualize.shader lines 62-71
    // Map output UV to sort-space coordinates.
    // Horizontal: sortIdx from UV.x, rowIdx from UV.y.
    // Vertical:   sortIdx from UV.y, rowIdx from UV.x.
    var sort_idx: u32;
    var row_idx:  u32;
    if params.sort_vertical == 0u {
        sort_idx = u32(clamp(i32(uv.x * f32(params.width)),  0, i32(params.width)  - 1));
        row_idx  = u32(clamp(i32(uv.y * f32(params.height)), 0, i32(params.height) - 1));
    } else {
        sort_idx = u32(clamp(i32(uv.y * f32(params.width)),  0, i32(params.width)  - 1));
        row_idx  = u32(clamp(i32(uv.x * f32(params.height)), 0, i32(params.height) - 1));
    }

    // ComputePixelSortVisualize.shader line 74 — read sorted entry from buffer
    let buffer_idx     = row_idx * params.padded_width + sort_idx;
    let sort_entry     = sort_buffer[buffer_idx];
    let sorted_orig_idx = sort_entry.y;

    // ComputePixelSortVisualize.shader lines 79-80
    // If pixel wasn't moved (or masked), keep original
    if sorted_orig_idx == sort_idx {
        textureStore(output_tex, vec2<i32>(gid.xy), original);
        return;
    }

    // ComputePixelSortVisualize.shader lines 85-89
    // Map sorted index back to source UV.
    // Horizontal: sorted index is X position, Y stays same.
    // Vertical:   sorted index is Y position, X stays same.
    var sorted_uv: vec2<f32>;
    if params.sort_vertical == 0u {
        sorted_uv = vec2<f32>((f32(sorted_orig_idx) + 0.5) / f32(params.width), uv.y);
    } else {
        sorted_uv = vec2<f32>(uv.x, (f32(sorted_orig_idx) + 0.5) / f32(params.width));
    }

    let sorted = textureSampleLevel(source_tex, tex_sampler, sorted_uv, 0.0);

    // ComputePixelSortVisualize.shader line 94 — blend with original based on amount
    let result = mix(original, sorted, params.amount);
    textureStore(output_tex, vec2<i32>(gid.xy), result);
}
