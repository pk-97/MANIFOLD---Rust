// Separable 3D Gaussian blur for volumetric fields.
// Two entry points:
//   blur_scalar: blur Rgba16Float density volume (X, Y, or Z pass)
//   blur_vector: blur Rgba16Float vector volume (X, Y, or Z pass)
// Ping-pongs between source and dest textures per axis.
// Uses bilinear tap pairing via textureSampleLevel — pairs adjacent integer
// offsets (j, j+1) into a single weighted-midpoint fetch, halving sample count.
// Repeat-mode sampler handles toroidal wrap.

struct BlurUniforms {
    vol_res: u32,
    axis: u32,       // 0=X, 1=Y, 2=Z
    radius: f32,
    _pad: u32,
};

// ── Scalar blur (Rgba16Float density) ──

@group(0) @binding(0) var<uniform> params: BlurUniforms;
@group(0) @binding(1) var src_scalar: texture_3d<f32>;
@group(0) @binding(2) var s_scalar: sampler;
@group(0) @binding(3) var dst_scalar: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(4, 4, 4)
fn blur_scalar(@builtin(global_invocation_id) id: vec3<u32>) {
    let vr = params.vol_res;
    if id.x >= vr || id.y >= vr || id.z >= vr {
        return;
    }

    let sigma = max(params.radius / 2.5, 0.5);
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);
    let radius_int = i32(params.radius);
    let texel = 1.0 / f32(vr);

    // UV center for this voxel (half-texel offset for texel center)
    let uv = (vec3<f32>(id) + 0.5) * texel;

    // Axis direction vector
    var axis_dir = vec3<f32>(0.0);
    if params.axis == 0u { axis_dir.x = texel; }
    else if params.axis == 1u { axis_dir.y = texel; }
    else { axis_dir.z = texel; }

    // Center tap
    var result = textureSampleLevel(src_scalar, s_scalar, uv, 0.0).r;
    var total_weight = 1.0;

    // Bilinear tap pairing: pair (j, j+1) into a single weighted-midpoint fetch
    var j: i32 = 1;
    loop {
        if j > radius_int { break; }

        let fj = f32(j);
        let w_a = exp(-(fj * fj) * inv_two_sigma_sq);

        if j + 1 <= radius_int {
            let fj1 = f32(j + 1);
            let w_b = exp(-(fj1 * fj1) * inv_two_sigma_sq);
            let w_ab = w_a + w_b;
            let offset = fj + w_b / w_ab;

            result += textureSampleLevel(src_scalar, s_scalar, uv + axis_dir * offset, 0.0).r * w_ab;
            result += textureSampleLevel(src_scalar, s_scalar, uv - axis_dir * offset, 0.0).r * w_ab;
            total_weight += w_ab * 2.0;
        } else {
            // Unpaired last tap (odd radius)
            result += textureSampleLevel(src_scalar, s_scalar, uv + axis_dir * fj, 0.0).r * w_a;
            result += textureSampleLevel(src_scalar, s_scalar, uv - axis_dir * fj, 0.0).r * w_a;
            total_weight += w_a * 2.0;
        }

        j += 2;
    }

    textureStore(dst_scalar, vec3<i32>(id), vec4<f32>(result / total_weight, 0.0, 0.0, 1.0));
}

// ── Vector blur (Rgba16Float force field) ──

@group(0) @binding(0) var<uniform> vec_params: BlurUniforms;
@group(0) @binding(1) var src_vector: texture_3d<f32>;
@group(0) @binding(2) var s_vector: sampler;
@group(0) @binding(3) var dst_vector: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(4, 4, 4)
fn blur_vector(@builtin(global_invocation_id) id: vec3<u32>) {
    let vr = vec_params.vol_res;
    if id.x >= vr || id.y >= vr || id.z >= vr {
        return;
    }

    let sigma = max(vec_params.radius / 2.5, 0.5);
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);
    let radius_int = i32(vec_params.radius);
    let texel = 1.0 / f32(vr);

    let uv = (vec3<f32>(id) + 0.5) * texel;

    var axis_dir = vec3<f32>(0.0);
    if vec_params.axis == 0u { axis_dir.x = texel; }
    else if vec_params.axis == 1u { axis_dir.y = texel; }
    else { axis_dir.z = texel; }

    // Center tap
    var result = textureSampleLevel(src_vector, s_vector, uv, 0.0);
    var total_weight = 1.0;

    var j: i32 = 1;
    loop {
        if j > radius_int { break; }

        let fj = f32(j);
        let w_a = exp(-(fj * fj) * inv_two_sigma_sq);

        if j + 1 <= radius_int {
            let fj1 = f32(j + 1);
            let w_b = exp(-(fj1 * fj1) * inv_two_sigma_sq);
            let w_ab = w_a + w_b;
            let offset = fj + w_b / w_ab;

            result += textureSampleLevel(src_vector, s_vector, uv + axis_dir * offset, 0.0) * w_ab;
            result += textureSampleLevel(src_vector, s_vector, uv - axis_dir * offset, 0.0) * w_ab;
            total_weight += w_ab * 2.0;
        } else {
            result += textureSampleLevel(src_vector, s_vector, uv + axis_dir * fj, 0.0) * w_a;
            result += textureSampleLevel(src_vector, s_vector, uv - axis_dir * fj, 0.0) * w_a;
            total_weight += w_a * 2.0;
        }

        j += 2;
    }

    textureStore(dst_vector, vec3<i32>(id), result / total_weight);
}
