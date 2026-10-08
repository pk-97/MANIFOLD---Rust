// node.mask_extrema — signed one-axis morphology over coverage.
//
// The input is read with textureLoad so coverage remains exact and no sampler
// can interpolate a label-like value. Positive radius is a max, negative
// radius is a min. Out-of-image taps contribute zero, which expands positive
// coverage normally and erodes negative coverage at the border.
struct Params {
    radius: f32,
    axis: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var destination: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(destination);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }

    let coord = vec2<i32>(id.xy);
    let radius = clamp(i32(round(params.radius)), -32, 32);
    if radius == 0 {
        textureStore(destination, coord, textureLoad(source, coord, 0));
        return;
    }

    let extent = abs(radius);
    var result = textureLoad(source, coord, 0).r;
    for (var offset: i32 = -32; offset <= 32; offset = offset + 1) {
        if abs(offset) > extent {
            continue;
        }
        var sample_coord = coord;
        if params.axis == 0u {
            sample_coord.x = sample_coord.x + offset;
        } else {
            sample_coord.y = sample_coord.y + offset;
        }

        let inside = sample_coord.x >= 0 && sample_coord.y >= 0
            && sample_coord.x < i32(dims.x) && sample_coord.y < i32(dims.y);
        if !inside {
            if radius < 0 {
                result = 0.0;
            }
            continue;
        }

        let sample = textureLoad(source, sample_coord, 0).r;
        if radius > 0 {
            result = max(result, sample);
        } else {
            result = min(result, sample);
        }
    }

    textureStore(destination, coord, vec4<f32>(result, result, result, 1.0));
}
