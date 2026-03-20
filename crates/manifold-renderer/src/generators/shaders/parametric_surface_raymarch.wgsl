// parametric_surface_raymarch.wgsl — Raymarch the pre-baked volume.
// Mechanical translation of GeneratorParametricSurface.shader fragment pass.

struct Uniforms {
    time_val: f32,
    speed: f32,
    aspect_ratio: f32,
    uv_scale: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var volume_tex: texture_3d<f32>;
@group(0) @binding(2) var volume_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// Volume covers [-4*0.7, +4*0.7]^3 = [-2.8, +2.8]^3
const HALF_EXTENT: f32 = 4.0;
const VOL_SCALE: f32 = 0.7;
const MAX_STEPS: i32 = 80;
const SURFACE_THRESH: f32 = 0.01;
const NORMAL_EPS: f32 = 0.001;
const MAX_DIST: f32 = 20.0;
const MIN_STEP: f32 = 0.005;
const CAM_DIST: f32 = 6.0;

// Sample volume texture, converting world-space to [0,1]^3 UVW.
// Volume was baked at p * 0.7 scale so extent is HALF_EXTENT * VOL_SCALE.
fn sample_volume(p: vec3<f32>) -> f32 {
    let extent = HALF_EXTENT * VOL_SCALE;
    let uvw = p / (extent * 2.0) + 0.5;
    if any(uvw < vec3<f32>(0.0)) || any(uvw > vec3<f32>(1.0)) {
        return 10.0;
    }
    return textureSampleLevel(volume_tex, volume_sampler, uvw, 0.0).r;
}

// Ray-box intersection for [-extent, +extent]^3
fn ray_box(ro: vec3<f32>, rd: vec3<f32>, extent: f32) -> vec2<f32> {
    let inv_rd = 1.0 / rd;
    let t1 = (-extent - ro) * inv_rd;
    let t2 = (extent - ro) * inv_rd;
    let tmin = min(t1, t2);
    let tmax = max(t1, t2);
    let near = max(max(tmin.x, tmin.y), tmin.z);
    let far = min(min(tmax.x, tmax.y), tmax.z);
    return vec2<f32>(max(near, 0.0), far);
}

// 6-tap central difference normal
fn calc_normal(p: vec3<f32>) -> vec3<f32> {
    let e = NORMAL_EPS;
    let n = vec3<f32>(
        sample_volume(p + vec3<f32>(e, 0.0, 0.0)) - sample_volume(p - vec3<f32>(e, 0.0, 0.0)),
        sample_volume(p + vec3<f32>(0.0, e, 0.0)) - sample_volume(p - vec3<f32>(0.0, e, 0.0)),
        sample_volume(p + vec3<f32>(0.0, 0.0, e)) - sample_volume(p - vec3<f32>(0.0, 0.0, e))
    );
    return normalize(n);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Matches Unity: uv = (i.uv - 0.5) * _UVScale; uv.x *= _AspectRatio;
    var uv = (in.uv - 0.5) * u.uv_scale;
    uv.x *= u.aspect_ratio;

    // t = _Time2 * _AnimSpeed * 0.3
    let t = u.time_val * u.speed * 0.3;

    // Orbiting camera — matches Unity exactly
    let ro = vec3<f32>(cos(t) * CAM_DIST, sin(t * 0.7) * 2.0, sin(t) * CAM_DIST);
    let look_target = vec3<f32>(0.0);
    let fwd = normalize(look_target - ro);
    // Unity: right = normalize(cross(float3(0,1,0), fwd))
    let right = normalize(cross(vec3<f32>(0.0, 1.0, 0.0), fwd));
    let up = cross(fwd, right);
    // Unity: rd = normalize(fwd + uv.x * right + uv.y * up)
    let rd = normalize(fwd + uv.x * right + uv.y * up);

    // Ray-box intersection
    let extent = HALF_EXTENT * VOL_SCALE;
    let box_t = ray_box(ro, rd, extent);

    if box_t.x > box_t.y {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Sphere tracing — matches Unity loop
    var ray_t = box_t.x;
    var hit = false;
    var hit_pos = vec3<f32>(0.0);

    for (var j = 0; j < MAX_STEPS; j++) {
        let p = ro + rd * ray_t;
        let d = sample_volume(p);
        let abs_d = abs(d);

        if abs_d < SURFACE_THRESH {
            hit = true;
            hit_pos = p;
            break;
        }

        // Step by absolute distance — matches Unity: max(absd * 0.5, 0.005)
        ray_t += max(abs_d * 0.5, MIN_STEP);
        if ray_t > MAX_DIST {
            break;
        }
    }

    if !hit {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Lighting — matches Unity exactly
    let n = calc_normal(hit_pos);
    let light_dir = normalize(vec3<f32>(0.5, 0.8, -0.3));
    let diff = max(dot(n, light_dir), 0.0) * 0.6;

    // Rim: pow(1.0 - abs(dot(n, -rd)), 3.0) * 0.5
    let rim = pow(1.0 - abs(dot(n, -rd)), 3.0) * 0.5;

    let ambient = 0.15;
    let lum = clamp(diff + rim + ambient, 0.0, 1.0);

    return vec4<f32>(lum, lum, lum, 1.0);
}
