// FluidDensityScatter — port of Unity FluidDensityScatter.compute
// Atomic scatter density accumulation + color scatter.
//
// Each entry point uses @group(0) — each is compiled into a separate pipeline.
//
// splat_main:          Each particle scatters density to nearest texel via single atomicAdd.
// resolve_main:        Converts uint accumulation → float density, clears for next frame.
// splat_color_main:    Like splat_main but scatters per-particle RGBA color (4 atomics/particle).
// resolve_color_main:  Converts 4-uint RGBA accumulation → float4 color density, clears.
//
// Nearest-neighbor scatter: sub-texel smoothing is redundant because density is
// Gaussian-blurred immediately after. Single atomic per particle = fewer atomics.

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

// ── SplatKernel ──

struct SplatUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    // pre-scaled energy: 0.005 * (splat_size/3) * (1_000_000/active_count) * 4096 + 0.5
    scaled_energy: u32,
};

@group(0) @binding(0) var<storage, read> splat_particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> splat_accum: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> splat_params: SplatUniforms;

@compute @workgroup_size(256, 1, 1)
fn splat_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= splat_params.active_count {
        return;
    }

    let p = splat_particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    // Nearest texel + toroidal wrap (uint modulus)
    // Unity: uint2 coord = uint2(p.position.xy * float2(_Width, _Height)); coord.x = coord.x % _Width
    let coord = vec2<u32>(
        u32(p.position.x * f32(splat_params.width))  % splat_params.width,
        u32(p.position.y * f32(splat_params.height)) % splat_params.height,
    );
    let idx = coord.y * splat_params.width + coord.x;
    atomicAdd(&splat_accum[idx], splat_params.scaled_energy);
}

// ── ResolveKernel ──

struct ResolveUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<storage, read_write> resolve_accum: array<atomic<u32>>;
@group(0) @binding(1) var resolve_density_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> resolve_params: ResolveUniforms;

@compute @workgroup_size(16, 16, 1)
fn resolve_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= resolve_params.width || id.y >= resolve_params.height {
        return;
    }

    let idx = id.y * resolve_params.width + id.x;
    let raw_val = atomicLoad(&resolve_accum[idx]);

    // Fixed-point to float (FIXED_POINT_MULTIPLIER = 4096.0)
    let density = f32(raw_val) / 4096.0;
    textureStore(resolve_density_out, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(density, 0.0, 0.0, 1.0));

    // Self-clearing: zero for next frame
    atomicStore(&resolve_accum[idx], 0u);
}

// ── SplatColorKernel ──
// Scatters per-particle color into RGBA accumulation buffer (4 uints per texel, interleaved).
// Particle color is determined by age: age < 0 = uncolored (white), age >= 1 = zone color.
// Zone index is stored in particle.age as (zoneIndex + 1).

struct SplatColorUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    // pre-scaled energy (same formula as scalar splat)
    scaled_energy: u32,
    // 0=mono (not dispatched), 1-5 = color mode (matches Unity _ColorMode)
    color_mode: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read> color_particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> color_accum: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> color_splat_params: SplatColorUniforms;

// Color palettes — port of Unity PaletteColor(zone, mode)
// 4 colors per palette, one per injection zone. zone=0..3, mode=1..5.
fn palette_color(zone: u32, mode: u32) -> vec3<f32> {
    switch mode {
        case 1u: { // Blush
            switch zone {
                case 0u: { return vec3<f32>(0.88, 0.62, 0.65); }
                case 1u: { return vec3<f32>(0.68, 0.62, 0.82); }
                case 2u: { return vec3<f32>(0.92, 0.78, 0.62); }
                default: { return vec3<f32>(0.65, 0.78, 0.68); }
            }
        }
        case 2u: { // Sunset
            switch zone {
                case 0u: { return vec3<f32>(0.92, 0.65, 0.58); }
                case 1u: { return vec3<f32>(0.95, 0.82, 0.55); }
                case 2u: { return vec3<f32>(0.75, 0.55, 0.72); }
                default: { return vec3<f32>(0.92, 0.72, 0.62); }
            }
        }
        case 3u: { // Ocean
            switch zone {
                case 0u: { return vec3<f32>(0.58, 0.75, 0.85); }
                case 1u: { return vec3<f32>(0.52, 0.72, 0.72); }
                case 2u: { return vec3<f32>(0.68, 0.68, 0.82); }
                default: { return vec3<f32>(0.78, 0.82, 0.82); }
            }
        }
        case 4u: { // Vivid
            switch zone {
                case 0u: { return vec3<f32>(0.78, 0.32, 0.38); }
                case 1u: { return vec3<f32>(0.28, 0.48, 0.72); }
                case 2u: { return vec3<f32>(0.72, 0.62, 0.25); }
                default: { return vec3<f32>(0.35, 0.58, 0.45); }
            }
        }
        case 5u: { // White
            switch zone {
                case 0u: { return vec3<f32>(0.95, 0.95, 0.95); }
                case 1u: { return vec3<f32>(0.92, 0.92, 0.95); }
                case 2u: { return vec3<f32>(0.95, 0.93, 0.90); }
                default: { return vec3<f32>(0.90, 0.92, 0.92); }
            }
        }
        default: { return vec3<f32>(1.0); }
    }
}

@compute @workgroup_size(256, 1, 1)
fn splat_color_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= color_splat_params.active_count {
        return;
    }

    let p = color_particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    // Per-particle color: uncolored (age < 0) -> white; colored -> palette lookup.
    // age encodes zone as (zoneIndex + 1): 1,2,3,4.
    var col = vec3<f32>(1.0);
    if p.age > 0.5 {
        let zone = u32(max(i32(p.age + 0.5) - 1, 0));
        col = palette_color(clamp(zone, 0u, 3u), color_splat_params.color_mode);
    }

    // Nearest texel + toroidal wrap
    let coord = vec2<u32>(
        u32(p.position.x * f32(color_splat_params.width))  % color_splat_params.width,
        u32(p.position.y * f32(color_splat_params.height)) % color_splat_params.height,
    );

    // Per-channel energy (fixed-point): color * energy
    let base_idx = (coord.y * color_splat_params.width + coord.x) * 4u;
    let e   = color_splat_params.scaled_energy;
    let e_r = u32(f32(e) * col.r + 0.5);
    let e_g = u32(f32(e) * col.g + 0.5);
    let e_b = u32(f32(e) * col.b + 0.5);

    if e_r > 0u { atomicAdd(&color_accum[base_idx + 0u], e_r); }
    if e_g > 0u { atomicAdd(&color_accum[base_idx + 1u], e_g); }
    if e_b > 0u { atomicAdd(&color_accum[base_idx + 2u], e_b); }
    // Total energy channel for hue normalization
    atomicAdd(&color_accum[base_idx + 3u], e);
}

// ── ResolveColorKernel ──

struct ResolveColorUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<storage, read_write> color_resolve_accum: array<atomic<u32>>;
@group(0) @binding(1) var color_density_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> color_resolve_params: ResolveColorUniforms;

@compute @workgroup_size(16, 16, 1)
fn resolve_color_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= color_resolve_params.width || id.y >= color_resolve_params.height {
        return;
    }

    let base_idx = (id.y * color_resolve_params.width + id.x) * 4u;
    let r = f32(atomicLoad(&color_resolve_accum[base_idx + 0u])) / 4096.0;
    let g = f32(atomicLoad(&color_resolve_accum[base_idx + 1u])) / 4096.0;
    let b = f32(atomicLoad(&color_resolve_accum[base_idx + 2u])) / 4096.0;
    let a = f32(atomicLoad(&color_resolve_accum[base_idx + 3u])) / 4096.0;

    // Pre-normalize hue so bilinear filtering interpolates correct colors.
    // Unity: float3 hue = a > 0.001 ? float3(r,g,b) / a : float3(1,1,1)
    let hue = select(vec3<f32>(1.0), vec3<f32>(r, g, b) / a, a > 0.001);
    textureStore(color_density_out, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(hue, a));

    // Self-clearing
    atomicStore(&color_resolve_accum[base_idx + 0u], 0u);
    atomicStore(&color_resolve_accum[base_idx + 1u], 0u);
    atomicStore(&color_resolve_accum[base_idx + 2u], 0u);
    atomicStore(&color_resolve_accum[base_idx + 3u], 0u);
}
