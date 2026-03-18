// Mechanical port of ComputePixelSortVisualize.shader — scatter pixels by sorted indices.
// Reads the sort buffer to remap source UVs, blends with original by amount.
//
// Unity ref: Assets/Shaders/ComputePixelSortVisualize.shader — PixelSortVisualize pass.
//
// IMPORTANT: sort_buffer is read-only at fragment stage.
// wgpu requires BufferBindingType::Storage { read_only: true } visible to ShaderStages::FRAGMENT.

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
// sort_buffer is read-only at fragment stage
@group(0) @binding(3) var<storage, read> sort_buffer: array<vec2<u32>>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0)       uv:       vec2<f32>,
}

// Fullscreen triangle vertex shader (same as all other effects)
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOut {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VertexOut;
    out.position = vec4<f32>(pos[vi], 0.0, 1.0);
    out.uv       = uvs[vi];
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let uv = in.uv;

    // ComputePixelSortVisualize.shader line 56 — original color at this pixel
    let original = textureSample(source_tex, tex_sampler, uv);

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
        return original;
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

    let sorted = textureSample(source_tex, tex_sampler, sorted_uv);

    // ComputePixelSortVisualize.shader line 94 — blend with original based on amount
    return mix(original, sorted, params.amount);
}
