// Ported from FLIP Fluids fluidsimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
// node.matter_frame — write one particle frame of the seam
// (GPU_FLUID_SURFACE_DESIGN.md section 3.1) from matter points
// (GPU_MPM_SOLVER_DESIGN.md D9, D14). Storage order is id order, so the frame
// is id-sorted. The radius is the sphere of the point's rest volume, FLIP's
// marker-radius rule (fluidsimulation.cpp `_initializeParticleRadii`). A
// removed point (id 0) publishes radius 0, the seam's unused-slot mark. A tick
// whose stats hold a non-finite value is never published: the frame repeats
// the previous one.

struct MatterPoint {
    position: vec3<f32>,
    id: u32,
    velocity: vec3<f32>,
    volume_ratio: f32,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
}

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

struct FrameParams {
    count: u32,
    previous_count: u32,
    radius_scale: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<uniform> params: FrameParams;
@group(0) @binding(1) var<storage, read> points: array<MatterPoint>;
@group(0) @binding(2) var<storage, read> stats: array<u32>;
@group(0) @binding(3) var<storage, read> previous: array<FluidParticle>;
@group(0) @binding(4) var<storage, read_write> frame: array<FluidParticle>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    var f: FluidParticle;
    if stats[0] != 0u {
        if i < params.previous_count {
            frame[i] = previous[i];
        } else {
            f.position_radius = vec4<f32>(0.0);
            f.velocity = vec3<f32>(0.0);
            f.id = 0u;
            frame[i] = f;
        }
        return;
    }
    let p = points[i];
    if p.id == 0u {
        f.position_radius = vec4<f32>(p.position, 0.0);
        f.velocity = vec3<f32>(0.0);
        f.id = 0u;
    } else {
        let radius = params.radius_scale * pow(max(p.affine_y.w, 0.0), 1.0 / 3.0);
        f.position_radius = vec4<f32>(p.position, radius);
        f.velocity = p.velocity;
        f.id = p.id;
    }
    frame[i] = f;
}
