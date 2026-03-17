// ParametricSurfaceBake.wgsl — Bake parametric surface SDF to 3D texture.
// Mechanical translation of ParametricSurfaceBake.compute.
//
// Volume maps to [-4, 4]^3 world space with 0.7 scaling (matching original shader).
// Output: R16Float 3D texture with signed distance values.

struct Uniforms {
    shape: f32,
    morph: f32,
    vol_res: f32,
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var volume_out: texture_storage_3d<rgba16float, write>;

// --- SDF functions (identical to ParametricSurfaceBake.compute) ---

fn sdf_gyroid(p: vec3<f32>) -> f32 {
    return sin(p.x) * cos(p.y) + sin(p.y) * cos(p.z) + sin(p.z) * cos(p.x);
}

fn sdf_schwarz_p(p: vec3<f32>) -> f32 {
    return cos(p.x) + cos(p.y) + cos(p.z);
}

fn sdf_schwarz_d(p: vec3<f32>) -> f32 {
    return cos(p.x) * cos(p.y) * cos(p.z)
         - sin(p.x) * sin(p.y) * sin(p.z);
}

fn sdf_torus_knot(p: vec3<f32>) -> f32 {
    let r1 = 2.0;
    let r2 = 0.8;
    var q = vec2<f32>(length(p.xz) - r1, p.y);
    let angle = atan2(p.z, p.x);
    q.x += sin(angle * 3.0) * 0.4;
    q.y += cos(angle * 2.0) * 0.4;
    return length(q) - r2;
}

fn sdf_klein(p: vec3<f32>) -> f32 {
    let r = 1.5;
    let angle = atan2(p.z, p.x);
    let q = vec2<f32>(length(p.xz) - r, p.y);
    let ca = cos(angle);
    let sa = sin(angle);
    let x2 = q.x * ca - q.y * sa;
    let y2 = q.x * sa + q.y * ca;
    return length(vec2<f32>(x2, y2)) - 0.5 - 0.3 * cos(angle * 2.0);
}

fn eval_sdf(shape_idx: i32, p: vec3<f32>) -> f32 {
    if shape_idx == 0 { return sdf_gyroid(p); }
    else if shape_idx == 1 { return sdf_schwarz_p(p); }
    else if shape_idx == 2 { return sdf_schwarz_d(p); }
    else if shape_idx == 3 { return sdf_torus_knot(p); }
    else { return sdf_klein(p); }
}

fn scene_sdf(p: vec3<f32>, shape: f32, morph: f32) -> f32 {
    var shape_a = i32(floor(shape));
    var shape_b = shape_a + 1;
    if shape_b > 4 { shape_b = 0; }

    let d_a = eval_sdf(shape_a, p);
    let d_b = eval_sdf(shape_b, p);
    return mix(d_a, d_b, morph);
}

@compute @workgroup_size(4, 4, 4)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let res = u32(u.vol_res);
    if id.x >= res || id.y >= res || id.z >= res {
        return;
    }

    // Map voxel to [-4, 4]^3 world space, then apply 0.7 scaling
    // Matches Unity: p = (float3(id) / float(_VolumeRes) - 0.5) * 8.0; p *= 0.7;
    let p = (vec3<f32>(id) / u.vol_res - 0.5) * 8.0 * 0.7;

    let shape = clamp(u.shape, 0.0, 4.0);
    let morph = clamp(u.morph, 0.0, 1.0);

    let d = scene_sdf(p, shape, morph);

    textureStore(volume_out, id, vec4<f32>(d, 0.0, 0.0, 0.0));
}
