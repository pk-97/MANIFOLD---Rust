// 3D particle simulation: sample 3D vector field, apply turbulence/diffusion/respawn,
// container SDF enforcement, flatten toward camera plane, Euler integration.

struct SimUniforms {
    active_count: u32,
    vol_res: u32,
    frame_count: u32,
    container: f32,
    ctr_scale: f32,
    speed: f32,
    turbulence: f32,
    anti_clump: f32,
    wander: f32,
    respawn_rate: f32,
    dense_respawn: f32,
    dt: f32,
    flatten: f32,
    cam_tilt: f32,
    cam_dist: f32,
    time_speed: f32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var t_field: texture_3d<f32>;
@group(0) @binding(2) var s_field: sampler;
@group(0) @binding(3) var t_density: texture_3d<f32>;
@group(0) @binding(4) var<uniform> params: SimUniforms;

const PI: f32 = 3.14159265;

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn hash_float(seed: u32) -> f32 {
    return f32(wang_hash(seed)) / 4294967296.0;
}

fn simplex_noise_2d(p: vec2<f32>) -> f32 {
    let K1: f32 = 0.366025403784;
    let K2: f32 = 0.211324865405;

    let i = floor(p + (p.x + p.y) * K1);
    let a = p - i + (i.x + i.y) * K2;
    let o = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), a.x > a.y);
    let b = a - o + K2;
    let c = a - 1.0 + 2.0 * K2;

    let h = max(vec3<f32>(0.5) - vec3<f32>(dot(a, a), dot(b, b), dot(c, c)), vec3<f32>(0.0));
    let h4 = h * h * h * h;

    let seed0 = u32(i.x * 73856093.0 + i.y * 19349663.0);
    let seed1 = u32((i.x + o.x) * 73856093.0 + (i.y + o.y) * 19349663.0);
    let seed2 = u32((i.x + 1.0) * 73856093.0 + (i.y + 1.0) * 19349663.0);

    let h2a = vec2<f32>(f32(wang_hash(seed0)), f32(wang_hash(wang_hash(seed0)))) / 4294967296.0;
    let h2b = vec2<f32>(f32(wang_hash(seed1)), f32(wang_hash(wang_hash(seed1)))) / 4294967296.0;
    let h2c = vec2<f32>(f32(wang_hash(seed2)), f32(wang_hash(wang_hash(seed2)))) / 4294967296.0;

    let g0 = h2a * 2.0 - 1.0;
    let g1 = h2b * 2.0 - 1.0;
    let g2 = h2c * 2.0 - 1.0;

    let n = vec3<f32>(dot(g0, a), dot(g1, b), dot(g2, c));
    return dot(h4, n) * 70.0;
}

// Container SDFs
fn sd_box(p: vec3<f32>, half_size: vec3<f32>) -> f32 {
    let q = abs(p) - half_size;
    return length(max(q, vec3<f32>(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}

fn sd_sphere(p: vec3<f32>, r: f32) -> f32 {
    return length(p) - r;
}

fn sd_torus(p: vec3<f32>, big_r: f32, small_r: f32) -> f32 {
    let q = vec2<f32>(length(p.xz) - big_r, p.y);
    return length(q) - small_r;
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.active_count {
        return;
    }

    var p = particles[id.x];
    let rng_base = wang_hash(id.x * 1299721u + params.frame_count * 6291469u);

    // Sample 3D vector field at particle position
    let field_uv = vec3<f32>(
        fract(p.position.x + 1.0),
        fract(p.position.y + 1.0),
        fract(p.position.z + 1.0),
    );
    let field_force = textureSampleLevel(t_field, s_field, field_uv, 0.0).rgb;

    // Sample 3D density at particle position (Rgba16Float supports filtering)
    let density_val = textureSampleLevel(t_density, s_field, field_uv, 0.0).r;
    let capped_density = min(density_val, 5.0);

    // 3D simplex noise via 3 orthogonal 2D slices (YZ, XZ, XY)
    let noise_scale = 8.0;
    let time_offset = f32(params.frame_count) * 0.01;
    let noise_x = simplex_noise_2d(p.position.yz * noise_scale + vec2<f32>(time_offset, 0.0));
    let noise_y = simplex_noise_2d(p.position.xz * noise_scale + vec2<f32>(17.0 + time_offset, 31.0));
    let noise_z = simplex_noise_2d(p.position.xy * noise_scale + vec2<f32>(43.0 + time_offset, 59.0));

    let anti_clump_gain = params.anti_clump * 10.0;
    let turb_amplitude = params.turbulence * (1.0 + capped_density * anti_clump_gain);
    let turb_force = vec3<f32>(noise_x, noise_y, noise_z) * turb_amplitude;

    // Diffusion: wang hash random kick, density-adaptive
    let rng1 = wang_hash(rng_base);
    let rng2 = wang_hash(rng1);
    let rng3 = wang_hash(rng2);
    let diff_x = f32(rng1) / 4294967296.0 * 2.0 - 1.0;
    let diff_y = f32(rng2) / 4294967296.0 * 2.0 - 1.0;
    let diff_z = f32(rng3) / 4294967296.0 * 2.0 - 1.0;
    let diff_amplitude = params.wander * (1.0 + capped_density * 10.0);
    let diffusion = vec3<f32>(diff_x, diff_y, diff_z) * diff_amplitude;

    // Respawn check
    let rng4 = wang_hash(rng3);
    let rng_respawn = f32(rng4) / 4294967296.0;
    var effective_respawn = params.respawn_rate;
    if params.respawn_rate > 0.0 {
        effective_respawn = params.respawn_rate * (1.0 + capped_density * params.dense_respawn / params.respawn_rate);
    }

    let container_mode = i32(params.container + 0.5);

    if p.life <= 0.0 || rng_respawn < effective_respawn {
        // Respawn at random position in volume
        let rng5 = wang_hash(rng4);
        let rng6 = wang_hash(rng5);
        let rng7 = wang_hash(rng6);
        let rng8 = wang_hash(rng7);

        if container_mode > 0 {
            // Respawn on container surface
            p.position = vec3<f32>(
                hash_float(rng5),
                hash_float(rng6),
                hash_float(rng7),
            );
        } else {
            // Respawn at random edge (face of unit cube)
            let face = hash_float(rng5);
            let u1 = hash_float(rng6);
            let u2 = hash_float(rng7);
            if face < 1.0 / 6.0 {
                p.position = vec3<f32>(0.0, u1, u2);
            } else if face < 2.0 / 6.0 {
                p.position = vec3<f32>(1.0, u1, u2);
            } else if face < 3.0 / 6.0 {
                p.position = vec3<f32>(u1, 0.0, u2);
            } else if face < 4.0 / 6.0 {
                p.position = vec3<f32>(u1, 1.0, u2);
            } else if face < 5.0 / 6.0 {
                p.position = vec3<f32>(u1, u2, 0.0);
            } else {
                p.position = vec3<f32>(u1, u2, 1.0);
            }
        }

        p.velocity = vec3<f32>(0.0);
        p.life = 0.5 + hash_float(rng8) * 0.5;
        p.age = 0.0;
    } else {
        // Total force
        var total_force = (field_force + turb_force + diffusion) * params.speed;

        // Container SDF enforcement
        if container_mode > 0 {
            let center = p.position - 0.5;
            let margin = 0.02;
            var sdf = 0.0;
            var normal = vec3<f32>(0.0);

            if container_mode == 1 {
                // Box
                let half = vec3<f32>(0.45 * params.ctr_scale);
                sdf = sd_box(center, half);
                // Approximate gradient for normal
                let eps = 0.001;
                normal = normalize(vec3<f32>(
                    sd_box(center + vec3<f32>(eps, 0.0, 0.0), half) - sd_box(center - vec3<f32>(eps, 0.0, 0.0), half),
                    sd_box(center + vec3<f32>(0.0, eps, 0.0), half) - sd_box(center - vec3<f32>(0.0, eps, 0.0), half),
                    sd_box(center + vec3<f32>(0.0, 0.0, eps), half) - sd_box(center - vec3<f32>(0.0, 0.0, eps), half),
                ));
            } else if container_mode == 2 {
                // Sphere
                let r = 0.4 * params.ctr_scale;
                sdf = sd_sphere(center, r);
                normal = normalize(center);
            } else {
                // Torus
                let big_r = 0.3 * params.ctr_scale;
                let small_r = 0.1 * params.ctr_scale;
                sdf = sd_torus(center, big_r, small_r);
                let eps = 0.001;
                normal = normalize(vec3<f32>(
                    sd_torus(center + vec3<f32>(eps, 0.0, 0.0), big_r, small_r) - sd_torus(center - vec3<f32>(eps, 0.0, 0.0), big_r, small_r),
                    sd_torus(center + vec3<f32>(0.0, eps, 0.0), big_r, small_r) - sd_torus(center - vec3<f32>(0.0, eps, 0.0), big_r, small_r),
                    sd_torus(center + vec3<f32>(0.0, 0.0, eps), big_r, small_r) - sd_torus(center - vec3<f32>(0.0, 0.0, eps), big_r, small_r),
                ));
            }

            // Soft boundary push-back
            if sdf > -margin {
                let push = smoothstep(-margin, margin, sdf) * 0.1;
                total_force -= normal * push;
            }

            // Hard boundary: reflect velocity component along normal
            if sdf > 0.0 {
                p.position = p.position - normal * sdf * 1.1;
                let v_dot_n = dot(total_force, normal);
                if v_dot_n > 0.0 {
                    total_force -= normal * v_dot_n * 1.5;
                }
            }
        }

        // Flatten: compress toward camera viewing plane
        if params.flatten > 0.0 {
            let angle = params.time_speed;
            let tilt = params.cam_tilt;
            let cam_fwd = normalize(vec3<f32>(
                -cos(angle) * cos(tilt),
                -sin(tilt),
                -sin(angle) * cos(tilt),
            ));
            let depth = dot(p.position - 0.5, cam_fwd);
            total_force -= cam_fwd * depth * params.flatten * 2.0;
        }

        // Euler integration
        if container_mode > 0 {
            // With container: clamp instead of wrap
            p.position = clamp(p.position + total_force * params.dt, vec3<f32>(0.001), vec3<f32>(0.999));
        } else {
            // Toroidal wrap
            p.position = fract(p.position + total_force * params.dt + 1.0);
        }

        p.life -= params.dt * 0.1;
        p.age += params.dt;
    }

    particles[id.x] = p;
}
