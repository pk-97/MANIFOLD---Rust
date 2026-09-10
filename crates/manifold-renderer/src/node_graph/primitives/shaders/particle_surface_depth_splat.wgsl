// node.particle_surface_depth — depth/coverage splat kernel (HAND-AUTHORED
// standalone, the documented S6 escape for scatter/atomic rasterisation:
// per-particle atomics cannot express as a barrier-free per-element body).
//
// Each live particle splats its sphere impostor: per pixel in the
// conservative bbox, the exact front ray-sphere hit becomes a raw [0,1]
// clip depth via the shared projection convention
// (raw = range * (near / view_z - 1), the exact inverse of
// depth_common.wgsl's linearize_depth). Depth testing is atomicMin on the
// f32 bit pattern (raw is non-negative, so the u32 order matches); the
// first splat at a pixel atomicMax-marks coverage. Rejected particles
// (near-plane cross, camera inside the sphere, beyond far) are skipped —
// never garbage depths.

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
@group(0) @binding(2) var<storage, read_write> buf_depth_bits: array<atomic<u32>>;
@group(0) @binding(3) var<storage, read_write> buf_coverage: array<atomic<u32>>;

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

    let range = splat.far / (splat.near - splat.far);
    for (var y = bb_min.y; y <= bb_max.y; y = y + 1) {
        for (var x = bb_min.x; x <= bb_max.x; x = x + 1) {
            let px = vec2<i32>(x, y);
            let dir = splat_ray_dir(splat, px);
            let t = splat_ray_hit(c, splat.radius, dir);
            if (t < 0.0) {
                continue;
            }
            let view_z = t * dir.z;
            if (view_z <= splat.near) {
                continue;
            }
            var raw = range * (splat.near / view_z - 1.0);
            raw = clamp(raw, 0.0, 0.99999994);
            let flat_idx = u32(y) * splat.width + u32(x);
            atomicMin(&buf_depth_bits[flat_idx], bitcast<u32>(raw));
            atomicMax(&buf_coverage[flat_idx], 1u);
        }
    }
}
