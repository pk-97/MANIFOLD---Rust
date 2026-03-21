// 3D particle simulation — line-by-line translation of FluidSimulation3DSimulate.compute.
//
// Density-displacement feedback loop in 3D:
//   1. Sample blurred 3D vector field at particle position
//   2. Simplex noise advection on 3 orthogonal planes
//   3. Density-adaptive diffusion (incoherent random kick)
//   4. Density-adaptive respawn with container rejection sampling
//   5. Soft container boundary repulsion
//   6. Direct Euler integration: P_next = P_current + force * _Speed  (no dt)
//   7. Containment: toroidal wrap or SDF reflect + clamp
//   8. Flatten: compress toward camera viewing plane
//
// When use_vector_field == 0 (fallback), particles move via noise only.

struct SimUniforms {
    active_count:  u32,
    frame_count:   u32,
    use_vector_field: u32,
    container:     u32,
    ctr_scale:     f32,
    speed:         f32,
    turbulence:    f32,  // _NoiseAmplitude
    anti_clump:    f32,  // _AntiClump
    wander:        f32,  // _Diffusion
    respawn_rate:  f32,  // _RefreshRate
    dense_respawn: f32,  // _DensityRefreshScale
    flatten:       f32,
    // camera forward, precomputed C#-side from cam_orbit_angle and cam_tilt_rad
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    // injection
    color_mode:    u32,
    inject_index:  i32,   // -1 = off
    inject_force:  f32,
    inject_phase:  f32,
    time2:         f32,  // ctx.Time (_Time2)
    _pad0:         f32,
    _pad1:         f32,
    _pad2:         f32,
};

// Particle layout matches particle_common.wgsl (64 bytes with WGSL vec3 implicit padding):
// position: vec3<f32> (0-11) | implicit pad (12-15) | velocity: vec3<f32> (16-27)
// | life: f32 (28) | age: f32 (32) | implicit pad (36-47) | color: vec4<f32> (48-63)
struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life:     f32,
    age:      f32,
    color:    vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var t_field:   texture_3d<f32>;   // blurred vector field (Rgba16Float, filterable)
@group(0) @binding(2) var s_field:   sampler;            // linear clamp (shared for vector field + density)
@group(0) @binding(3) var t_density: texture_3d<f32>;   // blurred density (Rgba16Float, filterable — textureSampleLevel)
@group(0) @binding(4) var<uniform>  params: SimUniforms;

// --- Wang hash (matches ParticleCommon.cginc WangHash) ---
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

fn hash_float3(seed: u32) -> vec3<f32> {
    let h0 = wang_hash(seed);
    let h1 = wang_hash(h0);
    let h2 = wang_hash(h1);
    return vec3<f32>(
        f32(h0) / 4294967296.0,
        f32(h1) / 4294967296.0,
        f32(h2) / 4294967296.0,
    );
}

// --- Simplex noise 2D (matches ParticleCommon.cginc SimplexNoise2D) ---
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

// --- Container SDFs (matches FluidSimulation3DSimulate.compute) ---
fn sd_box(p: vec3<f32>, half_size: vec3<f32>) -> f32 {
    let d = abs(p) - half_size;
    return length(max(d, vec3<f32>(0.0))) + min(max(d.x, max(d.y, d.z)), 0.0);
}

fn sd_sphere(p: vec3<f32>, r: f32) -> f32 {
    return length(p) - r;
}

fn sd_torus(p: vec3<f32>, big_r: f32, small_r: f32) -> f32 {
    // Torus in XZ plane, centered at origin
    let q = vec2<f32>(length(p.xz) - big_r, p.y);
    return length(q) - small_r;
}

// Evaluate container SDF at position p (centered: [0,1] -> [-0.5, 0.5])
// Unity containerSDF: case 1 = scale*0.5, case 2 = scale*0.5, case 3 = scale*0.3, scale*0.12
fn container_sdf(p: vec3<f32>, ctype: u32, scale: f32) -> f32 {
    let centered = p - 0.5;
    switch ctype {
        case 1u: { return sd_box(centered, vec3<f32>(scale * 0.5)); }
        case 2u: { return sd_sphere(centered, scale * 0.5); }
        case 3u: { return sd_torus(centered, scale * 0.3, scale * 0.12); }
        default: { return -1.0; }  // always inside (no container)
    }
}

// Container gradient (outward normal) via central differences
fn container_gradient(p: vec3<f32>, ctype: u32, scale: f32) -> vec3<f32> {
    let eps: f32 = 0.002;
    return normalize(vec3<f32>(
        container_sdf(p + vec3<f32>(eps, 0.0, 0.0), ctype, scale) - container_sdf(p - vec3<f32>(eps, 0.0, 0.0), ctype, scale),
        container_sdf(p + vec3<f32>(0.0, eps, 0.0), ctype, scale) - container_sdf(p - vec3<f32>(0.0, eps, 0.0), ctype, scale),
        container_sdf(p + vec3<f32>(0.0, 0.0, eps), ctype, scale) - container_sdf(p - vec3<f32>(0.0, 0.0, eps), ctype, scale),
    ));
}

// --- 3D injection zone points (tetrahedron vertices, matches Unity) ---
const INJECT_POINTS_3D: array<vec3<f32>, 4> = array<vec3<f32>, 4>(
    vec3<f32>(0.644, 0.644, 0.644),
    vec3<f32>(0.644, 0.356, 0.356),
    vec3<f32>(0.356, 0.644, 0.356),
    vec3<f32>(0.356, 0.356, 0.644),
);
const INJECT_COLOR_RADIUS_3D: f32 = 0.05;
const INJECT_FORCE_RADIUS_3D:  f32 = 0.25;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.active_count {
        return;
    }

    var p = particles[i];
    let pos = p.position;

    var force = vec3<f32>(0.0);

    // --- Sample 3D vector field (density-displacement feedback) ---
    // Unity line 132-138: if (_UseVectorField > 0)
    var local_density: f32 = 0.0;
    if params.use_vector_field > 0u {
        force += textureSampleLevel(t_field, s_field, pos, 0.0).xyz;

        // Sample blurred density for density-adaptive noise/refresh
        // Unity: _DensityTex.SampleLevel(sampler_linear_clamp, pos, 0).r
        // Density volume is Rgba16Float (filterable) — matches Unity bilinear sampling.
        local_density = textureSampleLevel(t_density, s_field, pos, 0.0).r;
    }

    // cappedDensity = localDensity / (1.0 + localDensity)  — soft clamp: 0->0, inf->1
    // Unity line 140
    let capped_density = local_density / (1.0 + local_density);

    // adaptiveAmp = _NoiseAmplitude * (1.0 + cappedDensity * _AntiClump)
    // Unity line 141
    let adaptive_amp = params.turbulence * (1.0 + capped_density * params.anti_clump);

    // --- 3D noise advection using SimplexNoise2D on 3 orthogonal planes ---
    // Unity lines 147-153:
    //   noiseTime = _Time2 * 0.1
    //   noisePos  = pos * 2.0
    //   nx = (SimplexNoise2D(noisePos.yz + noiseTime) - 0.5) * 2.0
    //   ny = (SimplexNoise2D(noisePos.xz + noiseTime + 100.0) - 0.5) * 2.0
    //   nz = (SimplexNoise2D(noisePos.xy + noiseTime + 200.0) - 0.5) * 2.0
    let noise_time = params.time2 * 0.1;
    let noise_pos = pos * 2.0;
    let nx = (simplex_noise_2d(noise_pos.yz + vec2<f32>(noise_time)) - 0.5) * 2.0;
    let ny = (simplex_noise_2d(noise_pos.xz + vec2<f32>(noise_time + 100.0)) - 0.5) * 2.0;
    let nz = (simplex_noise_2d(noise_pos.xy + vec2<f32>(noise_time + 200.0)) - 0.5) * 2.0;
    force += vec3<f32>(nx, ny, nz) * adaptive_amp;

    // --- Per-particle diffusion (incoherent 3D random kick) ---
    // Unity lines 159-160:
    //   diffSeed = i * 1664525u + _FrameCount * 747796405u
    //   force += (HashFloat3(diffSeed) - 0.5) * _Diffusion * cappedDensity
    let diff_seed = i * 1664525u + params.frame_count * 747796405u;
    force += (hash_float3(diff_seed) - 0.5) * params.wander * capped_density;

    // --- Refresh: density-adaptive respawn ---
    // Unity lines 165-196:
    //   refreshSeed = i * 196613u + _FrameCount * 2891336453u
    //   adaptiveRefresh = _RefreshRate + cappedDensity * _DensityRefreshScale
    let refresh_seed = i * 196613u + params.frame_count * 2891336453u;
    let adaptive_refresh = params.respawn_rate + capped_density * params.dense_respawn;
    if hash_float(refresh_seed) < adaptive_refresh {
        // Container-aware respawn: rejection sample until inside SDF
        var respawn_pos = hash_float3(refresh_seed + 7919u);
        if params.container > 0u {
            // Up to 8 attempts to find a point inside the container
            var attempt: i32 = 0;
            loop {
                if attempt >= 8 { break; }
                if container_sdf(respawn_pos, params.container, params.ctr_scale) < 0.0 { break; }
                let next_seed = wang_hash(refresh_seed + u32(attempt) * 31u);
                respawn_pos = hash_float3(next_seed);
                attempt += 1;
            }
        }
        // Flatten: compress respawn position toward camera viewing plane
        let cam_fwd = vec3<f32>(params.cam_fwd_x, params.cam_fwd_y, params.cam_fwd_z);
        if params.flatten > 0.0 {
            let d = dot(respawn_pos - 0.5, cam_fwd);
            respawn_pos -= cam_fwd * d * params.flatten;
        }
        p.position = respawn_pos;
        p.velocity = vec3<f32>(0.0);
        p.life     = 1.0;
        p.age      = -1.0;  // respawned particles start uncolored
        p.color    = vec4<f32>(0.005, 0.005, 0.005, 1.0);
        particles[i] = p;
        return;
    }

    // --- Soft container boundary repulsion ---
    // Unity lines 201-211:
    //   margin = 0.1
    //   if (d > -margin): t = saturate((d + margin) / margin); force -= n * (t * t * 0.15)
    if params.container > 0u {
        let d = container_sdf(pos, params.container, params.ctr_scale);
        let margin: f32 = 0.1;
        if d > -margin {
            let n = container_gradient(pos, params.container, params.ctr_scale);
            let t = clamp((d + margin) / margin, 0.0, 1.0);
            force -= n * (t * t * 0.15);
        }
    }

    // --- Integration: newPos = pos + force * _Speed  (NO dt multiplication) ---
    // Unity line 214: float3 newPos = pos + force * _Speed;
    var new_pos = pos + force * params.speed;

    // --- Containment ---
    // Unity lines 217-243
    if params.container == 0u {
        // No container: toroidal wrap on all 3 axes
        new_pos = fract(new_pos + 1.0);
    } else {
        // SDF container: reflect velocity at boundary + clamp inside
        let d = container_sdf(new_pos, params.container, params.ctr_scale);
        if d > 0.0 {
            // Particle escaped: push back inside along gradient (surface normal)
            let normal = container_gradient(new_pos, params.container, params.ctr_scale);
            new_pos -= normal * (d + 0.001);  // push inside with small margin

            // Reflect force component along normal (bounce)
            let vel = force * params.speed;
            let normal_component = dot(vel, normal);
            if normal_component > 0.0 {
                p.velocity = vel - 2.0 * normal_component * normal;
            }
        }
        // Safety clamp: ensure particle stays in [0.001, 0.999]
        new_pos = clamp(new_pos, vec3<f32>(0.001), vec3<f32>(0.999));
    }

    // --- Flatten: compress toward camera viewing plane ---
    // Unity lines 249-253: newPos -= _CamFwd * depthFromCenter * _Flatten * 0.1
    // (position offset, not force offset)
    if params.flatten > 0.0 {
        let cam_fwd = vec3<f32>(params.cam_fwd_x, params.cam_fwd_y, params.cam_fwd_z);
        let depth_from_center = dot(new_pos - 0.5, cam_fwd);
        new_pos -= cam_fwd * depth_from_center * params.flatten * 0.1;
    }

    p.position = new_pos;

    // --- Injection disturbance (3D) ---
    // Unity lines 260-310
    if params.inject_index >= 0 {
        let ipos  = p.position;
        let idx_inject = u32(params.inject_index);
        let delta = ipos - INJECT_POINTS_3D[idx_inject];
        let dist2 = dot(delta, delta);
        let force_r2 = INJECT_FORCE_RADIUS_3D * INJECT_FORCE_RADIUS_3D;

        // Force envelope: fast attack (~10% of burst), exponential decay
        let attack  = clamp(params.inject_phase * 10.0, 0.0, 1.0);
        let decay2  = exp(-params.inject_phase * 3.0);
        let envelope = attack * decay2;

        if dist2 < force_r2 && dist2 > 0.0001 && envelope > 0.001 {
            let dist   = sqrt(dist2);
            let t2     = dist / INJECT_FORCE_RADIUS_3D;
            let radial = delta / dist;

            // Spatial falloff: smooth quartic
            let ff = (1.0 - t2 * t2);
            let falloff = ff * ff;

            // Noise perturbation: breaks spherical symmetry
            let noise_angle1 = simplex_noise_2d(ipos.xy * 8.0 + vec2<f32>(params.time2 * 0.3)) * 6.28318;
            let noise_angle2 = simplex_noise_2d(ipos.yz * 8.0 + vec2<f32>(params.time2 * 0.3 + 50.0)) * 6.28318;
            let noise_dir = vec3<f32>(cos(noise_angle1), sin(noise_angle1) * cos(noise_angle2), sin(noise_angle2));
            let perturbed_radial = normalize(radial + noise_dir * 0.4 * t2);

            // Vortex ring via cross product
            var tangent = normalize(cross(radial, vec3<f32>(0.0, 1.0, 0.0)));
            if length(cross(radial, vec3<f32>(0.0, 1.0, 0.0))) < 0.001 {
                tangent = normalize(cross(radial, vec3<f32>(1.0, 0.0, 0.0)));
            }
            let curl_profile = t2 * (1.0 - t2) * 4.0;
            let curl_force_v = tangent * curl_profile;

            let strength = params.inject_force * envelope * falloff;
            let push = perturbed_radial * strength + curl_force_v * strength * 0.5;
            p.position = clamp(ipos + push, vec3<f32>(0.001), vec3<f32>(0.999));
        }

        // Color tagging: particles near injection point get zone index
        if p.age < 0.0 && envelope > 0.3 {
            let color_r = INJECT_COLOR_RADIUS_3D;
            let d2 = p.position - INJECT_POINTS_3D[idx_inject];
            if dot(d2, d2) < color_r * color_r {
                p.age = f32(params.inject_index + 1);
            }
        }
    }

    // Persistent dim color — density builds from accumulation
    // Unity line 313: p.color = float4(0.005, 0.005, 0.005, 1.0);  (unconditional, every frame)
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[i] = p;
}

// ============================================================
// SeedPatternKernel — GPU-side 3D pattern initialization.
// Translation of SeedPatternKernel in FluidSimulation3DSimulate.compute.
// ============================================================

struct SeedUniforms {
    active_count:   u32,
    pattern_type:   u32,
    trigger_count:  u32,   // DIFF-12: triggerCount * 7919 for pattern seed
    _pad0:          u32,
    // container params (needed for SeedPatternKernel container rejection)
    container:      u32,
    ctr_scale:      f32,
    flatten:        f32,
    _pad1:          f32,
    cam_fwd_x:      f32,
    cam_fwd_y:      f32,
    cam_fwd_z:      f32,
    _pad2:          f32,
};

@group(0) @binding(0) var<storage, read_write> seed_particles: array<Particle>;
@group(0) @binding(1) var<uniform> seed_params: SeedUniforms;

@compute @workgroup_size(256, 1, 1)
fn seed_pattern(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= seed_params.active_count {
        return;
    }

    // seed = i * 1664525u + _PatternSeed * 747796405u  (Unity SeedPatternKernel line 336)
    let pattern_seed = seed_params.trigger_count * 7919u;
    let seed = i * 1664525u + pattern_seed * 747796405u;
    var pos: vec3<f32>;

    switch seed_params.pattern_type {
        case 0u: {
            // Center cluster (CLT 3D Gaussian approximation)
            var sum = vec3<f32>(0.0);
            var s = seed;
            for (var j: i32 = 0; j < 4; j++) {
                sum += hash_float3(s);
                s = wang_hash(s);
            }
            pos = 0.5 + (sum - 2.0) * 0.052;
        }
        case 1u: {
            // Horizontal planes (6 slabs at fixed Y)
            let slab_idx = i % 6u;
            let xz = hash_float3(seed).xy;
            pos = vec3<f32>(xz.x, (f32(slab_idx) + 0.5) / 6.0, xz.y);
        }
        case 2u: {
            // Vertical planes (6 slabs at fixed X)
            let slab_idx = i % 6u;
            let yz = hash_float3(seed).xy;
            pos = vec3<f32>((f32(slab_idx) + 0.5) / 6.0, yz.x, yz.y);
        }
        case 3u: {
            // Concentric shells (3 spherical shells)
            let shell_idx = i % 3u;
            let h = hash_float3(seed).xy;
            let theta = h.x * 6.28318530718;
            let phi = acos(1.0 - 2.0 * h.y);
            let radius = 0.1 + f32(shell_idx) * 0.1;
            pos = vec3<f32>(
                0.5 + sin(phi) * cos(theta) * radius,
                0.5 + sin(phi) * sin(theta) * radius,
                0.5 + cos(phi) * radius,
            );
        }
        case 4u: {
            // 3D diagonal cross
            let t = hash_float(seed);
            let spread = hash_float(seed + 1u) * 0.03;
            let axis = i % 3u;
            if axis == 0u {
                pos = vec3<f32>(t, t + spread, 0.5 + spread);
            } else if axis == 1u {
                pos = vec3<f32>(t, 0.5 + spread, t + spread);
            } else {
                pos = vec3<f32>(0.5 + spread, t, t + spread);
            }
        }
        case 5u: {
            // Helix — double helix around Y axis
            let norm = f32(i) / f32(seed_params.active_count);
            let angle = norm * 6.0 * 6.28318530718;
            let y = norm;
            let radius: f32 = 0.15;
            let spread = hash_float(seed) * 0.015;
            let helix_side = select(0.0, 3.14159, (i & 1u) != 0u);
            pos = vec3<f32>(
                0.5 + cos(angle + helix_side) * (radius + spread),
                y,
                0.5 + sin(angle + helix_side) * (radius + spread),
            );
        }
        case 6u: {
            // Surface sphere — implodes inward
            let h = hash_float3(seed).xy;
            let theta = h.x * 6.28318530718;
            let phi = acos(1.0 - 2.0 * h.y);
            let spread = (hash_float(seed + 1u) - 0.5) * 0.03;
            let radius = 0.45 + spread;
            pos = vec3<f32>(
                0.5 + sin(phi) * cos(theta) * radius,
                0.5 + sin(phi) * sin(theta) * radius,
                0.5 + cos(phi) * radius,
            );
        }
        default: {
            pos = hash_float3(seed);
        }
    }

    // Clamp to valid volume range
    pos = clamp(pos, vec3<f32>(0.001), vec3<f32>(0.999));

    // Container-aware: pull toward center along normal if outside
    if seed_params.container > 0u {
        if container_sdf(pos, seed_params.container, seed_params.ctr_scale) > 0.0 {
            let n = container_gradient(pos, seed_params.container, seed_params.ctr_scale);
            let d = container_sdf(pos, seed_params.container, seed_params.ctr_scale);
            pos -= n * (d + 0.01);
            pos = clamp(pos, vec3<f32>(0.001), vec3<f32>(0.999));
        }
    }

    // Flatten: compress seeded position toward camera viewing plane
    let cam_fwd = vec3<f32>(seed_params.cam_fwd_x, seed_params.cam_fwd_y, seed_params.cam_fwd_z);
    if seed_params.flatten > 0.0 {
        let d = dot(pos - 0.5, cam_fwd);
        pos -= cam_fwd * d * seed_params.flatten;
    }

    var out_p: Particle;
    out_p.position = pos;
    out_p.velocity = vec3<f32>(0.0);
    out_p.life     = 1.0;
    out_p.age      = -1.0;
    out_p.color    = vec4<f32>(0.005, 0.005, 0.005, 1.0);
    seed_particles[i] = out_p;
}
