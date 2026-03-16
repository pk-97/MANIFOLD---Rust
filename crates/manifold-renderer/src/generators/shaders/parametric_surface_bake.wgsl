struct Uniforms {
    shape_a: f32,
    shape_b: f32,
    morph: f32,
    time_val: f32,
    speed: f32,
    scale: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var volume_out: texture_storage_3d<r32float, write>;

const VOL_SIZE: f32 = 128.0;
const HALF_EXTENT: f32 = 4.0;
const PI: f32 = 3.14159265;

fn gyroid(p: vec3<f32>) -> f32 {
    return sin(p.x) * cos(p.y) + sin(p.y) * cos(p.z) + sin(p.z) * cos(p.x);
}

fn schwarz_p(p: vec3<f32>) -> f32 {
    return cos(p.x) + cos(p.y) + cos(p.z);
}

fn schwarz_d(p: vec3<f32>) -> f32 {
    return sin(p.x) * sin(p.y) * sin(p.z)
         + sin(p.x) * cos(p.y) * cos(p.z)
         + cos(p.x) * sin(p.y) * cos(p.z)
         + cos(p.x) * cos(p.y) * sin(p.z);
}

fn torus_knot(p: vec3<f32>) -> f32 {
    // Parametric torus knot SDF approximation (p,q = 2,3)
    let r1 = 2.0;
    let r2 = 0.8;
    // Distance to torus knot curve, sampled at 64 points
    var min_d = 100.0;
    for (var i = 0; i < 32; i++) {
        let t = f32(i) / 32.0 * 2.0 * PI;
        let r = r1 + r2 * cos(3.0 * t);
        let curve = vec3<f32>(r * cos(2.0 * t), r * sin(2.0 * t), r2 * sin(3.0 * t));
        let d = length(p - curve) - 0.35;
        min_d = min(min_d, d);
    }
    return min_d;
}

fn klein_bottle(p: vec3<f32>) -> f32 {
    // Klein bottle immersion SDF approximation
    let r = 1.5;
    var min_d = 100.0;
    for (var i = 0; i < 24; i++) {
        for (var j = 0; j < 12; j++) {
            let u_val = f32(i) / 24.0 * 2.0 * PI;
            let v_val = f32(j) / 12.0 * 2.0 * PI;
            let cu = cos(u_val);
            let su = sin(u_val);
            let cv = cos(v_val);
            let sv = sin(v_val);
            // Figure-8 Klein bottle immersion
            let x = (r + cu * 0.5 * cv - su * 0.5 * sin(v_val * 2.0)) * cos(u_val);
            let y = (r + cu * 0.5 * cv - su * 0.5 * sin(v_val * 2.0)) * sin(u_val);
            let z = su * 0.5 * cv + cu * 0.5 * sin(v_val * 2.0);
            let curve = vec3<f32>(x, y, z);
            let d = length(p - curve) - 0.2;
            min_d = min(min_d, d);
        }
    }
    return min_d;
}

fn eval_sdf(shape_idx: i32, p: vec3<f32>) -> f32 {
    switch (shape_idx) {
        case 0: { return gyroid(p); }
        case 1: { return schwarz_p(p); }
        case 2: { return schwarz_d(p); }
        case 3: { return torus_knot(p); }
        case 4: { return klein_bottle(p); }
        default: { return gyroid(p); }
    }
}

@compute @workgroup_size(8, 8, 8)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= 128u || id.y >= 128u || id.z >= 128u {
        return;
    }

    // Map voxel to world-space [-4, +4]^3
    let p = (vec3<f32>(id) / VOL_SIZE - 0.5) * HALF_EXTENT * 2.0 * 0.7;

    // Animate position with time
    let t = u.time_val * u.speed;
    let anim_p = p * u.scale + vec3<f32>(sin(t * 0.3) * 0.2, cos(t * 0.2) * 0.2, sin(t * 0.5) * 0.1);

    // Evaluate two SDFs and morph between them
    let shape_a = i32(u.shape_a);
    let shape_b = i32(u.shape_b);
    let sdf_a = eval_sdf(shape_a, anim_p);
    let sdf_b = eval_sdf(shape_b, anim_p);
    let dist = mix(sdf_a, sdf_b, u.morph);

    textureStore(volume_out, id, vec4<f32>(dist, 0.0, 0.0, 0.0));
}
