// node.particle_thickness — additive chord-length splat kernel
// (HAND-AUTHORED standalone, the documented S6 escape for scatter/atomic
// rasterisation). Each live particle accumulates the length of the ray's
// chord through its sphere impostor (2 * sqrt(discriminant), metres of
// approximate optical thickness — a sphere-splat approximation, not an
// exact volume integral; docs/WATER_SIMULATION_DESIGN.md section 7).
// f32 accumulation is a bounded CAS loop on the bit pattern: thickness is
// non-negative, so plain magnitude addition via compare/exchange. The
// scratch is zeroed before the splat (encoder clear_buffer), so empty = 0.

struct WaterParticle {
    position_mass: vec4<f32>,
    velocity_density: vec4<f32>,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
    previous_position: vec4<f32>,
}

@group(0) @binding(0) var<uniform> splat: SplatView;
@group(0) @binding(1) var<storage, read> buf_particles: array<WaterParticle>;
@group(0) @binding(2) var<storage, read_write> buf_thickness: array<atomic<u32>>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= splat.count) {
        return;
    }
    let p = buf_particles[idx];
    if (p.position_mass.w == 0.0) {
        return; // inactive slot
    }
    let c = splat_view_center(splat, p.position_mass.xyz);
    if (!splat_accept(splat, c)) {
        return;
    }
    var bb_min: vec2<i32>;
    var bb_max: vec2<i32>;
    splat_bbox(splat, c, &bb_min, &bb_max);

    for (var y = bb_min.y; y <= bb_max.y; y = y + 1) {
        for (var x = bb_min.x; x <= bb_max.x; x = x + 1) {
            let px = vec2<i32>(x, y);
            let dir = splat_ray_dir(splat, px);
            let b = dot(dir, c);
            let disc = b * b - dot(c, c) + splat.radius * splat.radius;
            if (disc <= 0.0) {
                continue;
            }
            let chord = 2.0 * sqrt(disc);
            let flat_idx = u32(y) * splat.width + u32(x);

            // Bounded CAS add (same shape as the S4 checked accumulation):
            // thickness never wraps (non-negative, far below f32 max), the
            // loop only resolves contention. Retry bound is the S4
            // WATER_CAS_RETRIES precedent.
            var old = atomicLoad(&buf_thickness[flat_idx]);
            var settled = false;
            for (var attempt = 0u; attempt < 2048u; attempt = attempt + 1u) {
                let r = atomicCompareExchangeWeak(
                    &buf_thickness[flat_idx], old, bitcast<u32>(bitcast<f32>(old) + chord));
                if (r.exchanged) {
                    settled = true;
                    break;
                }
                old = r.old_value;
            }
            if (!settled) {
                // Contention bound hit: drop the contribution rather than
                // spin unbounded. 2048 consecutive losses at realistic
                // splat densities is unreachable; logged by the value
                // proofs if it ever fires (coverage would drop).
            }
        }
    }
}

// Shape-aware entry point. The optional shape buffer is deliberately bound
// only here so the legacy cs_main ABI remains unchanged.
@group(0) @binding(3) var<storage, read> buf_shapes: array<SurfaceShape>;
@compute @workgroup_size(256)
fn cs_anisotropic(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= splat.count) { return; }
    let s = shape_view(splat, buf_shapes[idx]);
    if (!shape_valid(s)) { return; }
    var lo: vec2<i32>; var hi: vec2<i32>;
    var sphere = splat; sphere.radius = s.bound;
    if (buf_particles[idx].position_mass.w == 0.0 || !splat_accept(sphere, s.center)) { return; }
    shape_bbox(splat, s, &lo, &hi);
    for (var y = lo.y; y <= hi.y; y++) { for (var x = lo.x; x <= hi.x; x++) {
        let hit = shape_ray_hit(s, splat_ray_dir(splat, vec2<i32>(x, y)));
        if (hit.x <= 0.0 || hit.y <= hit.x) { continue; }
        let flat = u32(y) * splat.width + u32(x);
        let chord = hit.y - hit.x;
        var old = atomicLoad(&buf_thickness[flat]);
        for (var attempt = 0u; attempt < 2048u; attempt++) {
            let r = atomicCompareExchangeWeak(&buf_thickness[flat], old, bitcast<u32>(bitcast<f32>(old) + chord));
            if (r.exchanged) { break; } old = r.old_value;
        }
    }}
}
