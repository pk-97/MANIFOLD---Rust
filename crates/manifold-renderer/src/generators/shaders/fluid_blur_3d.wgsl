// Separable 3D Gaussian blur for volumetric fields.
// Two entry points:
//   blur_scalar: blur R32Float density volume (X, Y, or Z pass)
//   blur_vector: blur Rgba16Float vector volume (X, Y, or Z pass)
// Ping-pongs between source and dest textures per axis.
// Toroidal wrap via modulo.

struct BlurUniforms {
    vol_res: u32,
    axis: u32,       // 0=X, 1=Y, 2=Z
    radius: f32,
    _pad: u32,
};

// ── Scalar blur (Rgba16Float density — matches Unity RHalf precision, filterable on Metal) ──

@group(0) @binding(0) var<uniform> params: BlurUniforms;
@group(0) @binding(1) var src_scalar: texture_3d<f32>;
@group(0) @binding(2) var dst_scalar: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(4, 4, 4)
fn blur_scalar(@builtin(global_invocation_id) id: vec3<u32>) {
    let vr = params.vol_res;
    if id.x >= vr || id.y >= vr || id.z >= vr {
        return;
    }

    let sigma = max(params.radius / 2.5, 0.5);
    let two_sigma_sq = 2.0 * sigma * sigma;
    let radius_int = i32(params.radius);

    var result = 0.0;
    var total_weight = 0.0;

    for (var j: i32 = -radius_int; j <= radius_int; j = j + 1) {
        let w = exp(-f32(j * j) / two_sigma_sq);

        var sample_coord = vec3<i32>(i32(id.x), i32(id.y), i32(id.z));
        if params.axis == 0u {
            sample_coord.x = (sample_coord.x + j + i32(vr)) % i32(vr);
        } else if params.axis == 1u {
            sample_coord.y = (sample_coord.y + j + i32(vr)) % i32(vr);
        } else {
            sample_coord.z = (sample_coord.z + j + i32(vr)) % i32(vr);
        }

        let val = textureLoad(src_scalar, sample_coord, 0).r;
        result += val * w;
        total_weight += w;
    }

    textureStore(dst_scalar, vec3<i32>(i32(id.x), i32(id.y), i32(id.z)), vec4<f32>(result / total_weight, 0.0, 0.0, 1.0));
}

// ── Vector blur (Rgba16Float force field) ──

@group(0) @binding(0) var<uniform> vec_params: BlurUniforms;
@group(0) @binding(1) var src_vector: texture_3d<f32>;
@group(0) @binding(2) var dst_vector: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(4, 4, 4)
fn blur_vector(@builtin(global_invocation_id) id: vec3<u32>) {
    let vr = vec_params.vol_res;
    if id.x >= vr || id.y >= vr || id.z >= vr {
        return;
    }

    let sigma = max(vec_params.radius / 2.5, 0.5);
    let two_sigma_sq = 2.0 * sigma * sigma;
    let radius_int = i32(vec_params.radius);

    var result = vec4<f32>(0.0);
    var total_weight = 0.0;

    for (var j: i32 = -radius_int; j <= radius_int; j = j + 1) {
        let w = exp(-f32(j * j) / two_sigma_sq);

        var sample_coord = vec3<i32>(i32(id.x), i32(id.y), i32(id.z));
        if vec_params.axis == 0u {
            sample_coord.x = (sample_coord.x + j + i32(vr)) % i32(vr);
        } else if vec_params.axis == 1u {
            sample_coord.y = (sample_coord.y + j + i32(vr)) % i32(vr);
        } else {
            sample_coord.z = (sample_coord.z + j + i32(vr)) % i32(vr);
        }

        let val = textureLoad(src_vector, sample_coord, 0);
        result += val * w;
        total_weight += w;
    }

    textureStore(dst_vector, vec3<i32>(i32(id.x), i32(id.y), i32(id.z)), result / total_weight);
}
