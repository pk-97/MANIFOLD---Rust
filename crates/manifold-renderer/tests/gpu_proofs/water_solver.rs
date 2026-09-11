//! Live Water S4 GPU proofs — solver stage primitives vs the f64 oracle.
//!
//! Every numerical comparison here is GPU output against f64/analytic
//! expectations (the S1 `reference.rs` oracle, included from the same source
//! file the CPU proofs use) or against a dt-vs-dt/2 rerun — never against a
//! second WGSL implementation. Design acceptance numbers:
//! docs/WATER_SIMULATION_DESIGN.md section 8.
//!
//! The substep loop dispatches the stage kernels directly on shared buffers
//! (the resolve_accumulator gpu_tests pattern); one test additionally runs
//! the full chain through the graph executor to prove run()/binding/aliasing
//! wiring. Substep REGION execution (water_state + bounded repeats) is the
//! S2/S5 seam and is not exercised here.

use std::sync::{Arc, OnceLock};

use manifold_gpu::{GpuBinding, GpuBuffer, GpuDevice};

use manifold_renderer::node_graph::freeze::codegen::ENTRY;
use manifold_renderer::node_graph::primitive::PrimitiveSpec;
use manifold_renderer::node_graph::primitives::{
    ClearGrid, ClearGridUniforms, CommitUniforms, GatherAdvectUniforms, GridVelocityUniforms,
    MpmGatherAdvect, MpmGridVelocity, MpmScatterMassMomentum, MpmScatterStress, SeedWater,
    SeedWaterUniforms, ValidateUniforms, WaterCommit, WaterValidate, ACCUM_ITEMS, BASIN_MAX,
    BASIN_MIN, CELL_COUNT, CUBE_HALF, SCATTER_MASS_WGSL, SCATTER_STRESS_WGSL, VALIDATE_WGSL,
};
use manifold_renderer::node_graph::water::{
    acoustic_cfl, classify_position, WaterGridCell, WaterParticle, AFFINE_BOUND, DEFAULT_STEP_DT,
    DOMAIN_ORIGIN, DYNAMIC_VISCOSITY, FAULT_INTEGER_OVERFLOW, FAULT_NONFINITE,
    GRID_FIXED_SCALE,
    GRID_SPACING, PARTICLE_CAPACITY, PARTICLE_MASS, REST_DENSITY, SEED_ACTIVE_PARTICLES,
    SOUND_SPEED_C0, STATUS_WORDS, VELOCITY_BOUND, WATER_DOMAIN,
};

/// The S1 f64 oracle as a test path module — the same source file the S1
/// CPU proofs ran against, never a second WGSL implementation.
#[allow(dead_code)]
#[path = "../../src/node_graph/water/reference.rs"]
mod ref_oracle;

const PARTICLE_BYTES: u64 = std::mem::size_of::<WaterParticle>() as u64;
const GRID_BYTES: u64 = std::mem::size_of::<WaterGridCell>() as u64;
const ACCUM_BYTES: u64 = (ACCUM_ITEMS as u64) * 4;
const GRID_CELLS: usize = CELL_COUNT as usize;
const CELLS_4: usize = ACCUM_ITEMS as usize;
/// Affine fixture size: 16^3 lattice block plus one off-lattice particle.
const AFFINE_FIXTURE_CAPACITY: usize = 16 * 16 * 16 + 1;

fn device() -> &'static Arc<GpuDevice> {
    static DEVICE: OnceLock<Arc<GpuDevice>> = OnceLock::new();
    DEVICE.get_or_init(|| Arc::new(GpuDevice::new()))
}

fn ceil256(n: u32) -> [u32; 3] {
    [n.div_ceil(256), 1, 1]
}

fn pipeline_standalone<P: PrimitiveSpec>(label: &str) -> manifold_gpu::GpuComputePipeline {
    let wgsl = manifold_renderer::node_graph::freeze::codegen::standalone_for_spec::<P>()
        .unwrap_or_else(|e| panic!("{label} standalone codegen: {e:?}"));
    device().create_compute_pipeline(&wgsl, ENTRY, label)
}

fn pipeline_hand(wgsl: &str, label: &str) -> manifold_gpu::GpuComputePipeline {
    device().create_compute_pipeline(wgsl, "cs_main", label)
}

fn particle_buffer(count: usize) -> GpuBuffer {
    device().create_buffer_shared(count as u64 * PARTICLE_BYTES)
}

fn write_particles(buf: &GpuBuffer, particles: &[WaterParticle]) {
    assert!(buf.size >= std::mem::size_of_val(particles) as u64);
    unsafe {
        buf.write(0, bytemuck::cast_slice(particles));
    }
}

fn read_particles(buf: &GpuBuffer, count: usize) -> Vec<WaterParticle> {
    let ptr = buf.mapped_ptr().expect("shared particle buffer");
    let raw = unsafe { std::slice::from_raw_parts(ptr, count * PARTICLE_BYTES as usize) };
    bytemuck::cast_slice(raw).to_vec()
}

fn read_accum(buf: &GpuBuffer) -> Vec<i32> {
    let ptr = buf.mapped_ptr().expect("shared accumulator buffer");
    let raw = unsafe { std::slice::from_raw_parts(ptr, ACCUM_BYTES as usize) };
    bytemuck::cast_slice(raw).to_vec()
}

fn read_grid(buf: &GpuBuffer) -> Vec<WaterGridCell> {
    let ptr = buf.mapped_ptr().expect("shared grid buffer");
    let raw = unsafe { std::slice::from_raw_parts(ptr, GRID_CELLS * GRID_BYTES as usize) };
    bytemuck::cast_slice(raw).to_vec()
}

fn read_status(buf: &GpuBuffer) -> u32 {
    let ptr = buf.mapped_ptr().expect("shared status buffer");
    unsafe { std::ptr::read_unaligned(ptr.cast::<u32>()) }
}

fn read_status_payload(buf: &GpuBuffer) -> Vec<u32> {
    let ptr = buf.mapped_ptr().expect("shared status buffer");
    let raw = unsafe { std::slice::from_raw_parts(ptr.cast::<u32>(), STATUS_WORDS) };
    raw.to_vec()
}

fn write_particle_at(buf: &GpuBuffer, index: usize, particle: &WaterParticle) {
    unsafe {
        buf.write(
            (index * PARTICLE_BYTES as usize) as u64,
            bytemuck::bytes_of(particle),
        );
    }
}

fn dispatch_mass_and_stress(pool: &Pool) {
    let mass_u = manifold_renderer::node_graph::primitives::ScatterMassUniforms {
        active_count: pool.active,
        _pad0: 0,
        _pad1: 0,
        _pad2: 0,
    };
    let stress_u = manifold_renderer::node_graph::primitives::ScatterStressUniforms {
        step_dt: DEFAULT_STEP_DT,
        active_count: pool.active,
        _pad0: 0,
        _pad1: 0,
    };
    let mut enc = device().create_encoder("water-dispatch-regression");
    enc.dispatch_compute(
        &kernels().scatter_mass,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&mass_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accepted,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &pool.status,
                offset: 0,
            },
        ],
        ceil256(pool.capacity),
        "node.mpm_scatter_mass_momentum",
    );
    enc.commit_and_wait_completed();
    assert_eq!(read_status(&pool.status), 0, "mass scatter faulted");

    let mut enc = device().create_encoder("water-dispatch-regression-stress");
    enc.dispatch_compute(
        &kernels().scatter_stress,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&stress_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accepted,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &pool.status,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &pool.stress_out,
                offset: 0,
            },
        ],
        ceil256(pool.capacity),
        "node.mpm_scatter_stress",
    );
    enc.commit_and_wait_completed();
}

// ---------------------------------------------------------------------------
// Fixtures (mirrors of the S1 affine fixture — analytically affine field so
// the f64 oracle cannot agree with the f32 path by construction).
// ---------------------------------------------------------------------------

const FIELD_A: [f64; 3] = [0.3, -0.5, 0.2];
const FIELD_B: [[f64; 3]; 3] = [[0.4, 0.2, -0.1], [0.0, -0.3, 0.25], [0.15, 0.1, 0.2]];

fn field_v(x: [f64; 3]) -> [f64; 3] {
    let mut v = FIELD_A;
    for (vi, row) in v.iter_mut().zip(FIELD_B) {
        for (&bv, &xj) in row.iter().zip(x.iter()) {
            *vi += bv * xj;
        }
    }
    v
}

fn field_b_f32() -> [[f32; 3]; 3] {
    let mut b = [[0.0f32; 3]; 3];
    for (brow, frow) in b.iter_mut().zip(FIELD_B) {
        for (bv, fv) in brow.iter_mut().zip(frow) {
            *bv = fv as f32;
        }
    }
    b
}

fn make_particle(pos: [f32; 3], vel: [f32; 3], c: [[f32; 3]; 3], mass: f32) -> WaterParticle {
    WaterParticle {
        position_mass: [pos[0], pos[1], pos[2], mass],
        velocity_density: [vel[0], vel[1], vel[2], REST_DENSITY],
        affine_x: [c[0][0], c[0][1], c[0][2], 0.0],
        affine_y: [c[1][0], c[1][1], c[1][2], 0.0],
        affine_z: [c[2][0], c[2][1], c[2][2], 0.0],
        previous_position: [pos[0], pos[1], pos[2], 0.0],
    }
}

fn lattice_pos(q: [f32; 3]) -> [f32; 3] {
    [
        DOMAIN_ORIGIN[0] + q[0] * GRID_SPACING,
        DOMAIN_ORIGIN[1] + q[1] * GRID_SPACING,
        DOMAIN_ORIGIN[2] + q[2] * GRID_SPACING,
    ]
}

fn lattice_block(q_lo: f32, q_hi: f32) -> Vec<WaterParticle> {
    let b = field_b_f32();
    let mut out = Vec::new();
    let mut q = q_lo;
    while q <= q_hi {
        let mut qy = q_lo;
        while qy <= q_hi {
            let mut qz = q_lo;
            while qz <= q_hi {
                let pos = lattice_pos([q, qy, qz]);
                let vel = field_v([pos[0] as f64, pos[1] as f64, pos[2] as f64]);
                out.push(make_particle(
                    pos,
                    [vel[0] as f32, vel[1] as f32, vel[2] as f32],
                    b,
                    PARTICLE_MASS,
                ));
                qz += 0.5;
            }
            qy += 0.5;
        }
        q += 0.5;
    }
    out
}

/// The S1 fixture: affine lattice block plus one off-lattice particle.
fn affine_fixture() -> Vec<WaterParticle> {
    let mut particles = lattice_block(8.0, 15.5);
    let off_pos = lattice_pos([11.31, 9.77, 13.42]);
    let off_vel = field_v([off_pos[0] as f64, off_pos[1] as f64, off_pos[2] as f64]);
    particles.push(make_particle(
        off_pos,
        [off_vel[0] as f32, off_vel[1] as f32, off_vel[2] as f32],
        field_b_f32(),
        PARTICLE_MASS,
    ));
    particles
}

fn to_ref(p: &WaterParticle) -> ref_oracle::RefParticle {
    let mut rp = ref_oracle::RefParticle::new(
        [
            p.position_mass[0] as f64,
            p.position_mass[1] as f64,
            p.position_mass[2] as f64,
        ],
        [
            p.velocity_density[0] as f64,
            p.velocity_density[1] as f64,
            p.velocity_density[2] as f64,
        ],
        p.position_mass[3] as f64,
    );
    for (crow, brow) in rp.c.iter_mut().zip(FIELD_B) {
        for (cv, bv) in crow.iter_mut().zip(brow) {
            *cv = bv;
        }
    }
    rp
}

/// CPU stand-in for the GPU quantiser: f32 contribution widened to f64,
/// rounded at Q — the identical value the kernel's f32 path produces
/// (multiplying by 2^20 is exact in both precisions).
fn quantise(v: f32) -> i32 {
    let scaled = (v as f64) * (GRID_FIXED_SCALE as f64);
    scaled.round() as i64 as i32
}

// ---------------------------------------------------------------------------
// GPU stage wrappers (direct dispatch, one encoder per substep batch).
// ---------------------------------------------------------------------------

struct Kernels {
    seed: manifold_gpu::GpuComputePipeline,
    clear: manifold_gpu::GpuComputePipeline,
    scatter_mass: manifold_gpu::GpuComputePipeline,
    scatter_stress: manifold_gpu::GpuComputePipeline,
    grid_velocity: manifold_gpu::GpuComputePipeline,
    gather: manifold_gpu::GpuComputePipeline,
    validate: manifold_gpu::GpuComputePipeline,
    commit: manifold_gpu::GpuComputePipeline,
}

fn kernels() -> &'static Kernels {
    static K: OnceLock<Kernels> = OnceLock::new();
    K.get_or_init(|| Kernels {
        seed: pipeline_standalone::<SeedWater>("node.seed_water"),
        clear: pipeline_standalone::<ClearGrid>("node.clear_grid"),
        scatter_mass: pipeline_hand(SCATTER_MASS_WGSL, "node.mpm_scatter_mass_momentum"),
        scatter_stress: pipeline_hand(SCATTER_STRESS_WGSL, "node.mpm_scatter_stress"),
        grid_velocity: pipeline_standalone::<MpmGridVelocity>("node.mpm_grid_velocity"),
        gather: pipeline_standalone::<MpmGatherAdvect>("node.mpm_gather_advect"),
        validate: pipeline_hand(VALIDATE_WGSL, "node.water_validate"),
        commit: pipeline_standalone::<WaterCommit>("node.water_commit"),
    })
}

/// Everything one substep touches, on shared buffers.
struct Pool {
    accepted: GpuBuffer,
    stress_out: GpuBuffer,
    candidate: GpuBuffer,
    accum: GpuBuffer,
    grid: GpuBuffer,
    status: GpuBuffer,
    active: u32,
    capacity: u32,
}

fn make_pool(active: u32, capacity: u32) -> Pool {
    let p = Pool {
        accepted: particle_buffer(capacity as usize),
        stress_out: particle_buffer(capacity as usize),
        candidate: particle_buffer(capacity as usize),
        accum: device().create_buffer_shared(ACCUM_BYTES),
        grid: device().create_buffer_shared(GRID_BYTES * GRID_CELLS as u64),
        status: device().create_buffer_shared(4),
        active,
        capacity,
    };
    p.status.zero_fill();
    p.accum.zero_fill();
    p
}

/// Seed `pool.accepted` from the default pool box via the seed kernel.
fn seed_pool(pool: &Pool) {
    let uniforms = SeedWaterUniforms {
        pool_min_x: -1.0,
        pool_min_y: 0.234375,
        pool_min_z: -1.0,
        pool_max_x: 1.0,
        pool_max_y: 0.734375,
        pool_max_z: 1.0,
        grid_spacing: GRID_SPACING,
        rest_density: REST_DENSITY,
        max_capacity: pool.capacity as i32,
        dispatch_count: pool.capacity,
        _pad0: 0,
        _pad1: 0,
    };
    let mut enc = device().create_encoder("water-seed");
    enc.dispatch_compute(
        &kernels().seed,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accepted,
                offset: 0,
            },
        ],
        ceil256(pool.capacity),
        "node.seed_water",
    );
    enc.commit_and_wait_completed();
}

/// One solver substep on `pool`: clear -> scatter mass -> scatter stress ->
/// grid velocity -> gather -> validate -> commit (in place on accepted).
/// `basin`/`gravity` configure the static boundary; `dt` is the substep.
/// With `probe_stages`, commits and reads the status word after each stage
/// (diagnostic only — readback per stage is a test affordance, never a
/// production path).
fn substep(pool: &Pool, dt: f32, basin_min: [f32; 3], basin_max: [f32; 3], gravity: [f32; 3]) {
    substep_inner(pool, dt, basin_min, basin_max, gravity, false)
}

fn substep_inner(
    pool: &Pool,
    dt: f32,
    basin_min: [f32; 3],
    basin_max: [f32; 3],
    gravity: [f32; 3],
    probe_stages: bool,
) {
    let k = kernels();
    let mut enc = device().create_encoder("water-substep");

    let clear_u = ClearGridUniforms {
        max_capacity: ACCUM_ITEMS as i32,
        dispatch_count: ACCUM_ITEMS,
        _pad0: 0,
        _pad1: 0,
    };
    enc.dispatch_compute(
        &k.clear,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&clear_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accum,
                offset: 0,
            },
        ],
        ceil256(ACCUM_ITEMS),
        "node.clear_grid",
    );

    let mass_u = manifold_renderer::node_graph::primitives::ScatterMassUniforms {
        active_count: pool.active,
        _pad0: 0,
        _pad1: 0,
        _pad2: 0,
    };
    enc.dispatch_compute(
        &k.scatter_mass,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&mass_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accepted,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &pool.status,
                offset: 0,
            },
        ],
        // Scan the allocated pool so a high-index live particle participates
        // when the caller's active prefix reaches it.
        ceil256(pool.capacity),
        "node.mpm_scatter_mass_momentum",
    );
    if probe_stages {
        enc.commit_and_wait_completed();
        println!(
            "  after scatter_mass: status={:#x}",
            read_status(&pool.status)
        );
        enc = device().create_encoder("water-substep");
    }

    let stress_u = manifold_renderer::node_graph::primitives::ScatterStressUniforms {
        step_dt: dt,
        active_count: pool.active,
        _pad0: 0,
        _pad1: 0,
    };
    enc.dispatch_compute(
        &k.scatter_stress,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&stress_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accepted,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &pool.status,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &pool.stress_out,
                offset: 0,
            },
        ],
        ceil256(pool.capacity),
        "node.mpm_scatter_stress",
    );
    if probe_stages {
        enc.commit_and_wait_completed();
        println!(
            "  after scatter_stress: status={:#x}",
            read_status(&pool.status)
        );
        enc = device().create_encoder("water-substep");
    }

    let grid_u = GridVelocityUniforms {
        step_dt: dt,
        cube_half_x: CUBE_HALF[0],
        cube_half_y: CUBE_HALF[1],
        cube_half_z: CUBE_HALF[2],
        basin_min_x: basin_min[0],
        basin_min_y: basin_min[1],
        basin_min_z: basin_min[2],
        basin_max_x: basin_max[0],
        basin_max_y: basin_max[1],
        basin_max_z: basin_max[2],
        cell_count: CELL_COUNT as i32,
        collider_enabled: 0,
        collider_x: 0.0,
        collider_y: 0.0,
        collider_z: 0.0,
        collider_velocity_x: 0.0,
        collider_velocity_y: 0.0,
        collider_velocity_z: 0.0,
        gravity_x: gravity[0],
        gravity_y: gravity[1],
        gravity_z: gravity[2],
        dispatch_count: CELL_COUNT,
        _pad0: 0,
        _pad1: 0,
        _pad2: 0,
    };
    enc.dispatch_compute(
        &k.grid_velocity,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&grid_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.grid,
                offset: 0,
            },
        ],
        ceil256(CELL_COUNT),
        "node.mpm_grid_velocity",
    );

    let gather_u = GatherAdvectUniforms {
        step_dt: dt,
        dispatch_count: pool.capacity,
        _pad0: 0,
        _pad1: 0,
    };
    enc.dispatch_compute(
        &k.gather,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&gather_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.stress_out,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.grid,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &pool.candidate,
                offset: 0,
            },
        ],
        ceil256(pool.capacity),
        "node.mpm_gather_advect",
    );

    let validate_u = ValidateUniforms {
        validate_count: pool.capacity,
        velocity_bound: VELOCITY_BOUND,
        affine_bound: AFFINE_BOUND,
        density_max: 4.0 * REST_DENSITY,
        diagnostics_enabled: 0,
    };
    enc.dispatch_compute(
        &k.validate,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&validate_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.candidate,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.status,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &pool.status,
                offset: 0,
            },
        ],
        ceil256(pool.capacity),
        "node.water_validate",
    );

    let commit_u = CommitUniforms {
        dispatch_count: pool.capacity,
        _pad0: 0,
        _pad1: 0,
        _pad2: 0,
    };
    enc.dispatch_compute(
        &k.commit,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&commit_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accepted,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.candidate,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &pool.status,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &pool.accepted,
                offset: 0,
            },
        ],
        ceil256(pool.capacity),
        "node.water_commit",
    );

    enc.commit_and_wait_completed();
}

// ---------------------------------------------------------------------------
// Proofs
// ---------------------------------------------------------------------------

#[test]
fn water_grid_velocity_collider_boundary_native() {
    // Six nodes sit exactly on the faces of the default cube; one node is
    // outside it. The nonzero collider velocity makes the proof
    // exercise relative, rather than world-space, normal velocity.
    let collider = [0.0, 1.0, 0.0];
    let collider_velocity = [0.5, -0.25, 1.0];
    let faces: [([u32; 3], [f32; 3], [f32; 3]); 6] = [
        ([28, 16, 32], [-1.0, 0.0, 0.0], [0.0, 0.4, 0.5]),
        ([36, 16, 32], [1.0, 0.0, 0.0], [0.0, 0.4, 0.5]),
        ([32, 12, 32], [0.0, -1.0, 0.0], [0.3, 0.0, 0.5]),
        ([32, 20, 32], [0.0, 1.0, 0.0], [0.3, 0.0, 0.5]),
        ([32, 16, 28], [0.0, 0.0, -1.0], [0.3, 0.4, 0.0]),
        ([32, 16, 36], [0.0, 0.0, 1.0], [0.3, 0.4, 0.0]),
    ];
    let outward = ([28, 17, 32], [-0.5, 0.15, 1.25]);
    let outside = ([32, 16, 40], [0.3, -0.4, 0.7]);
    let cell_index = |[x, y, z]: [u32; 3]| (x + 64 * (y + 64 * z)) as usize;

    let mut velocities = Vec::with_capacity(faces.len() + 1);
    let mut accum = vec![0i32; CELLS_4];
    for (cell, normal, tangent) in faces {
        let v = [
            collider_velocity[0] - 2.0 * normal[0] + tangent[0],
            collider_velocity[1] - 2.0 * normal[1] + tangent[1],
            collider_velocity[2] - 2.0 * normal[2] + tangent[2],
        ];
        let base = 4 * cell_index(cell);
        accum[base] = quantise(v[0]);
        accum[base + 1] = quantise(v[1]);
        accum[base + 2] = quantise(v[2]);
        accum[base + 3] = GRID_FIXED_SCALE;
        velocities.push((cell, normal, tangent, v));
    }
    let outside_base = 4 * cell_index(outside.0);
    accum[outside_base] = quantise(outside.1[0]);
    accum[outside_base + 1] = quantise(outside.1[1]);
    accum[outside_base + 2] = quantise(outside.1[2]);
    accum[outside_base + 3] = GRID_FIXED_SCALE;
    let outward_base = 4 * cell_index(outward.0);
    accum[outward_base] = quantise(outward.1[0]);
    accum[outward_base + 1] = quantise(outward.1[1]);
    accum[outward_base + 2] = quantise(outward.1[2]);
    accum[outward_base + 3] = GRID_FIXED_SCALE;

    let accum_buf = device().create_buffer_shared(ACCUM_BYTES);
    let output_buf = device().create_buffer_shared(GRID_BYTES * GRID_CELLS as u64);
    unsafe { accum_buf.write(0, bytemuck::cast_slice(&accum)); }

    let dispatch = |enabled: u32| {
        let uniforms = GridVelocityUniforms {
            step_dt: 0.0,
            cube_half_x: CUBE_HALF[0],
            cube_half_y: CUBE_HALF[1],
            cube_half_z: CUBE_HALF[2],
            basin_min_x: -10.0,
            basin_min_y: -10.0,
            basin_min_z: -10.0,
            basin_max_x: 10.0,
            basin_max_y: 10.0,
            basin_max_z: 10.0,
            cell_count: CELL_COUNT as i32,
            collider_enabled: enabled,
            collider_x: collider[0],
            collider_y: collider[1],
            collider_z: collider[2],
            collider_velocity_x: collider_velocity[0],
            collider_velocity_y: collider_velocity[1],
            collider_velocity_z: collider_velocity[2],
            gravity_x: 0.0,
            gravity_y: 0.0,
            gravity_z: 0.0,
            dispatch_count: CELL_COUNT,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let mut enc = device().create_encoder("water-grid-collider-proof");
        enc.dispatch_compute(
            &kernels().grid_velocity,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &accum_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &output_buf, offset: 0 },
            ],
            ceil256(CELL_COUNT),
            "node.mpm_grid_velocity",
        );
        enc.commit_and_wait_completed();
        read_grid(&output_buf)
    };

    let disabled = dispatch(0);
    for (cell, _normal, _tangent, expected) in &velocities {
        let actual = disabled[cell_index(*cell)].velocity_mass;
        for axis in 0..3 {
            assert!((actual[axis] - expected[axis]).abs() < 2.0e-4, "disabled collider changed node {cell:?}: {actual:?} vs {expected:?}");
        }
    }
    let outside_disabled = disabled[cell_index(outside.0)].velocity_mass;
    for (actual, expected) in outside_disabled.iter().zip(outside.1.iter()).take(3) {
        assert!((*actual - *expected).abs() < 2.0e-4);
    }
    let outward_disabled = disabled[cell_index(outward.0)].velocity_mass;
    for (actual, expected) in outward_disabled.iter().zip(outward.1.iter()).take(3) {
        assert!((*actual - *expected).abs() < 2.0e-4);
    }

    let enabled = dispatch(1);
    for (cell, normal, tangent, expected) in &velocities {
        let actual = enabled[cell_index(*cell)].velocity_mass;
        let expected_normal = collider_velocity[0] * normal[0]
            + collider_velocity[1] * normal[1]
            + collider_velocity[2] * normal[2];
        let actual_normal = actual[0] * normal[0] + actual[1] * normal[1] + actual[2] * normal[2];
        assert!((actual_normal - expected_normal).abs() < 2.0e-4, "face {cell:?} normal changed incorrectly: {actual:?}");
        for axis in 0..3 {
            if normal[axis] == 0.0 {
                assert!((actual[axis] - expected[axis]).abs() < 2.0e-4, "face {cell:?} tangent changed: {actual:?} vs {expected:?}");
            }
        }
        let tangent_dot = actual[0] * tangent[0] + actual[1] * tangent[1] + actual[2] * tangent[2];
        let expected_tangent_dot = expected[0] * tangent[0] + expected[1] * tangent[1] + expected[2] * tangent[2];
        assert!((tangent_dot - expected_tangent_dot).abs() < 2.0e-4);
    }
    let outside_enabled = enabled[cell_index(outside.0)].velocity_mass;
    for (actual, expected) in outside_enabled.iter().zip(outside.1.iter()).take(3) {
        assert!((*actual - *expected).abs() < 2.0e-4, "outside node changed: {outside_enabled:?}");
    }
    let outward_enabled = enabled[cell_index(outward.0)].velocity_mass;
    for (actual, expected) in outward_enabled.iter().zip(outward.1.iter()).take(3) {
        assert!((*actual - *expected).abs() < 2.0e-4, "outward node changed: {outward_enabled:?}");
    }
}

#[test]
fn water_scatter_kernels_reach_high_index_particle() {
    let capacity = 65_538u32;
    let pool = make_pool(capacity, capacity);
    let particle = make_particle(
        lattice_pos([12.0, 12.0, 12.0]),
        [0.25, -0.1, 0.05],
        [[0.0; 3]; 3],
        PARTICLE_MASS,
    );
    write_particle_at(&pool.accepted, capacity as usize - 1, &particle);

    dispatch_mass_and_stress(&pool);

    let accum = read_accum(&pool.accum);
    let mass: i32 = accum.chunks_exact(4).map(|cell| cell[3]).sum();
    let momentum: i32 = accum
        .chunks_exact(4)
        .map(|cell| cell[0].abs() + cell[1].abs() + cell[2].abs())
        .sum();
    assert!(mass > 0, "high-index particle contributed no grid mass");
    assert!(
        momentum > 0,
        "high-index particle contributed no grid momentum"
    );

    let stress = read_particles(&pool.stress_out, capacity as usize);
    assert!(
        stress[capacity as usize - 1].velocity_density[3] > 0.0,
        "high-index particle did not receive stress density"
    );
}

#[test]
fn water_stress_primitive_preserves_tail_for_zero_and_one_active() {
    let capacity = 256u32;
    let sentinel = make_particle([9.0, 8.0, 7.0], [6.0, 5.0, 4.0], [[3.0; 3]; 3], 2.0);
    for active in [0u32, 1u32] {
        let pool = make_pool(active, capacity);
        let input = make_particle(
            lattice_pos([12.0, 12.0, 12.0]),
            [0.25, -0.1, 0.05],
            [[0.0; 3]; 3],
            PARTICLE_MASS,
        );
        write_particle_at(&pool.accepted, 0, &input);
        for index in active as usize..capacity as usize {
            write_particle_at(&pool.accepted, index, &sentinel);
            write_particle_at(&pool.stress_out, index, &sentinel);
        }

        // Distinct poison makes this prove the stress primitive copies the
        // inactive input tail, rather than merely leaving its output intact.
        let poison = make_particle([-1.0, -2.0, -3.0], [-4.0, -5.0, -6.0], [[-7.0; 3]; 3], 8.0);
        for index in active as usize..capacity as usize {
            write_particle_at(&pool.stress_out, index, &poison);
        }

        dispatch_mass_and_stress(&pool);
        let output = read_particles(&pool.stress_out, capacity as usize);
        for (index, record) in output.iter().enumerate().skip(active as usize) {
            assert_eq!(
                bytemuck::bytes_of(record),
                bytemuck::bytes_of(&sentinel),
                "stress tail slot {index} changed for active_count={active}"
            );
        }
    }
}

/// Channel layout + seed values: the GPU seed kernel must reproduce the
/// deterministic h/2 lattice byte-for-byte against the CPU formula, with a
/// zeroed inactive tail.
#[test]
fn water_seed_lattice_matches_cpu() {
    let capacity = 4096u32;
    // Small pool box: 0.5 x 0.25 x 0.5 m at h/2 spacing = 16x8x16 = 2048.
    let pool_min = [0.0f32, 1.0, 0.0];
    let pool_max = [0.5f32, 1.25, 0.5];
    let buf = particle_buffer(capacity as usize);
    let uniforms = SeedWaterUniforms {
        pool_min_x: pool_min[0],
        pool_min_y: pool_min[1],
        pool_min_z: pool_min[2],
        pool_max_x: pool_max[0],
        pool_max_y: pool_max[1],
        pool_max_z: pool_max[2],
        grid_spacing: GRID_SPACING,
        rest_density: REST_DENSITY,
        max_capacity: capacity as i32,
        dispatch_count: capacity,
        _pad0: 0,
        _pad1: 0,
    };
    let mut enc = device().create_encoder("water-seed-proof");
    enc.dispatch_compute(
        &kernels().seed,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &buf,
                offset: 0,
            },
        ],
        ceil256(capacity),
        "node.seed_water",
    );
    enc.commit_and_wait_completed();

    let records = read_particles(&buf, capacity as usize);
    let spacing = GRID_SPACING * 0.5;
    let nx = ((pool_max[0] - pool_min[0]) / spacing + 0.5).floor() as usize;
    let ny = ((pool_max[1] - pool_min[1]) / spacing + 0.5).floor() as usize;
    let nz = ((pool_max[2] - pool_min[2]) / spacing + 0.5).floor() as usize;
    let seed_count = nx * ny * nz;
    assert_eq!(seed_count, 2048);
    let mass = REST_DENSITY * spacing * spacing * spacing;
    for (idx, p) in records.iter().enumerate() {
        if idx < seed_count {
            let ix = idx % nx;
            let iy = (idx / nx) % ny;
            let iz = idx / (nx * ny);
            let expected = [
                pool_min[0] + (ix as f32 + 0.5) * spacing,
                pool_min[1] + (iy as f32 + 0.5) * spacing,
                pool_min[2] + (iz as f32 + 0.5) * spacing,
            ];
            assert_eq!(
                p.position_mass,
                [expected[0], expected[1], expected[2], mass],
                "seed slot {idx} drifted"
            );
            assert_eq!(p.velocity_density, [0.0, 0.0, 0.0, REST_DENSITY]);
            assert_eq!(p.affine_x, [0.0; 4]);
            assert_eq!(p.affine_y, [0.0; 4]);
            assert_eq!(p.affine_z, [0.0; 4]);
            assert_eq!(
                p.previous_position,
                [expected[0], expected[1], expected[2], 0.0]
            );
        } else {
            assert_eq!(
                p.position_mass, [0.0; 4],
                "inactive tail slot {idx} not zeroed"
            );
            assert_eq!(p.velocity_density, [0.0; 4]);
        }
    }
}

/// Signed momentum + forced overflow. The quantised accumulation is exact
/// integer arithmetic, so every touched cell must match the CPU-computed
/// fixed-point sum bit for bit — including the negative momentum cells.
#[test]
fn water_signed_scatter_and_overflow() {
    // Two particles moving in -x/-y with nonzero affine state.
    let particles = [
        make_particle(
            lattice_pos([10.0, 10.0, 10.0]),
            [-0.25, -0.1, 0.05],
            field_b_f32(),
            PARTICLE_MASS,
        ),
        make_particle(
            lattice_pos([10.5, 10.5, 10.5]),
            [-0.4, 0.2, -0.15],
            field_b_f32(),
            PARTICLE_MASS,
        ),
    ];
    let pbuf = particle_buffer(256);
    write_particles(&pbuf, &particles);
    let accum = device().create_buffer_shared(ACCUM_BYTES);
    accum.zero_fill();
    let status = device().create_buffer_shared(4);
    status.zero_fill();

    let uniforms = manifold_renderer::node_graph::primitives::ScatterMassUniforms {
        active_count: 2,
        _pad0: 0,
        _pad1: 0,
        _pad2: 0,
    };
    let mut enc = device().create_encoder("water-signed-scatter");
    enc.dispatch_compute(
        &kernels().scatter_mass,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pbuf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &status,
                offset: 0,
            },
        ],
        ceil256(2),
        "node.mpm_scatter_mass_momentum",
    );
    enc.commit_and_wait_completed();

    assert_eq!(read_status(&status), 0, "clean fixture must not fault");

    // CPU fixed-point expectation, cell by cell.
    let mut expected = vec![0i32; CELLS_4];
    for p in &particles {
        let x = [p.position_mass[0], p.position_mass[1], p.position_mass[2]];
        let m = p.position_mass[3];
        let q = WATER_DOMAIN.position_to_q(x);
        let (base, frac) = manifold_renderer::node_graph::water::stencil_base_frac(q);
        let w = [
            manifold_renderer::node_graph::water::bspline_weights(frac[0]),
            manifold_renderer::node_graph::water::bspline_weights(frac[1]),
            manifold_renderer::node_graph::water::bspline_weights(frac[2]),
        ];
        let c = field_b_f32();
        for k in 0..3 {
            for j in 0..3 {
                for i in 0..3 {
                    let cell = [base[0] + i as i32, base[1] + j as i32, base[2] + k as i32];
                    let g = WATER_DOMAIN.grid_index(cell[0] as u32, cell[1] as u32, cell[2] as u32);
                    let w3 = w[0][i] * w[1][j] * w[2][k];
                    let d = [
                        (cell[0] as f32 * GRID_SPACING + DOMAIN_ORIGIN[0]) - x[0],
                        (cell[1] as f32 * GRID_SPACING + DOMAIN_ORIGIN[1]) - x[1],
                        (cell[2] as f32 * GRID_SPACING + DOMAIN_ORIGIN[2]) - x[2],
                    ];
                    let cd = [
                        c[0][0] * d[0] + c[0][1] * d[1] + c[0][2] * d[2],
                        c[1][0] * d[0] + c[1][1] * d[1] + c[1][2] * d[2],
                        c[2][0] * d[0] + c[2][1] * d[1] + c[2][2] * d[2],
                    ];
                    expected[g * 4 + 3] += quantise(w3 * m);
                    for a in 0..3 {
                        expected[g * 4 + a] += quantise(w3 * m * (p.velocity_density[a] + cd[a]));
                    }
                }
            }
        }
    }

    let gpu = read_accum(&accum);
    let mut touched_negative = false;
    for g in 0..GRID_CELLS {
        for slot in 0..4 {
            assert_eq!(
                gpu[g * 4 + slot],
                expected[g * 4 + slot],
                "cell {g} slot {slot}: GPU fixed-point != CPU fixed-point"
            );
        }
        if gpu[g * 4] < 0 || gpu[g * 4 + 1] < 0 {
            touched_negative = true;
        }
    }
    assert!(
        touched_negative,
        "fixture must produce negative momentum cells"
    );

    // Total momentum sanity vs the analytic sum (dequantised).
    let total_mom: f64 = gpu
        .chunks(4)
        .map(|c| (c[0] as f64 + c[1] as f64 + c[2] as f64) / GRID_FIXED_SCALE as f64)
        .sum();
    let expected_mom: f64 = particles
        .iter()
        .map(|p| {
        let m = p.position_mass[3] as f64;
        m * (p.velocity_density[0] as f64
            + p.velocity_density[1] as f64
            + p.velocity_density[2] as f64)
        })
        .sum();
    assert!(
        (total_mom - expected_mom).abs() < 1e-6,
        "total momentum {total_mom} != {expected_mom}"
    );

    // Forced overflow: park one target cell just under i32::MAX, scatter one
    // more contribution onto it, and require the sticky bit. The accumulator
    // is invalid scratch after a fault; downstream validate/commit tests prove
    // the accepted particle state remains byte-identical.
    let single = [make_particle(
        lattice_pos([20.0, 20.0, 20.0]),
        [0.0, 0.0, 0.0],
        [[0.0; 3]; 3],
        PARTICLE_MASS,
    )];
    let pbuf2 = particle_buffer(256);
    write_particles(&pbuf2, &single);
    let accum2 = device().create_buffer_shared(ACCUM_BYTES);
    accum2.zero_fill();
    // The particle sits exactly on node (20,20,20): frac 1.0, weight 0.75 on
    // that node, 0.125 on the neighbours.
    let centre = WATER_DOMAIN.grid_index(20, 20, 20);
    let near_max = i32::MAX - 1;
    unsafe {
        accum2.write((centre * 4 + 3) as u64 * 4, bytemuck::bytes_of(&near_max));
    }
    let status2 = device().create_buffer_shared(4);
    status2.zero_fill();
    let uniforms2 = manifold_renderer::node_graph::primitives::ScatterMassUniforms {
        active_count: 1,
        _pad0: 0,
        _pad1: 0,
        _pad2: 0,
    };
    let mut enc2 = device().create_encoder("water-overflow");
    enc2.dispatch_compute(
        &kernels().scatter_mass,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms2),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pbuf2,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &accum2,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &status2,
                offset: 0,
            },
        ],
        ceil256(1),
        "node.mpm_scatter_mass_momentum",
    );
    enc2.commit_and_wait_completed();

    let status_bits = read_status(&status2);
    assert_ne!(
        status_bits & FAULT_INTEGER_OVERFLOW,
        0,
        "forced overflow must stick FAULT_INTEGER_OVERFLOW"
    );
    // Feed the same sticky overflow through validation and commit. Even with
    // a clean candidate, the latched fault must retain the prior accepted
    // bytes; this is the end-to-end retention guarantee for wrapped scratch.
    let accepted_overflow = particle_buffer(1);
    let candidate_overflow = particle_buffer(1);
    let committed_overflow = particle_buffer(1);
    let mut changed_candidate = single;
    changed_candidate[0].position_mass[0] += 0.01;
    changed_candidate[0].velocity_density[0] = 0.5;
    write_particles(&accepted_overflow, &single);
    write_particles(&candidate_overflow, &changed_candidate);
    assert_ne!(
        bytemuck::cast_slice::<WaterParticle, u8>(&single),
        bytemuck::cast_slice::<WaterParticle, u8>(&changed_candidate),
        "overflow retention fixture must distinguish accepted and candidate"
    );
    let validate_u = ValidateUniforms {
        validate_count: 1,
        velocity_bound: VELOCITY_BOUND,
        affine_bound: AFFINE_BOUND,
        density_max: 4.0 * REST_DENSITY,
        diagnostics_enabled: 0,
    };
    let commit_u = CommitUniforms { dispatch_count: 1, _pad0: 0, _pad1: 0, _pad2: 0 };
    let mut validate_enc = device().create_encoder("water-overflow-validate");
    validate_enc.dispatch_compute(
        &kernels().validate,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&validate_u) },
            GpuBinding::Buffer { binding: 1, buffer: &candidate_overflow, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &status2, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &status2, offset: 0 },
        ],
        ceil256(1),
        "node.water_validate",
    );
    validate_enc.commit_and_wait_completed();
    let mut commit_enc = device().create_encoder("water-overflow-commit");
    commit_enc.dispatch_compute(
        &kernels().commit,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&commit_u) },
            GpuBinding::Buffer { binding: 1, buffer: &accepted_overflow, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &candidate_overflow, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &status2, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &committed_overflow, offset: 0 },
        ],
        ceil256(1),
        "node.water_commit",
    );
    commit_enc.commit_and_wait_completed();
    let committed_bytes = bytemuck::cast_slice::<WaterParticle, u8>(&read_particles(&committed_overflow, 1)).to_vec();
    let accepted_bytes = bytemuck::cast_slice::<WaterParticle, u8>(&read_particles(&accepted_overflow, 1)).to_vec();
    assert_eq!(committed_bytes, accepted_bytes, "overflowed scratch must not escape into accepted particle state");
    unsafe { status2.write(0, bytemuck::bytes_of(&0u32)); }
    let mut clean_commit = device().create_encoder("water-clean-commit-control");
    clean_commit.dispatch_compute(
        &kernels().commit,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&commit_u) },
            GpuBinding::Buffer { binding: 1, buffer: &accepted_overflow, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &candidate_overflow, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &status2, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &committed_overflow, offset: 0 },
        ],
        ceil256(1),
        "node.water_commit",
    );
    clean_commit.commit_and_wait_completed();
    assert_eq!(
        bytemuck::cast_slice::<WaterParticle, u8>(&read_particles(&committed_overflow, 1)),
        bytemuck::cast_slice::<WaterParticle, u8>(&changed_candidate),
        "clean status control must accept the changed candidate"
    );

    // Negative momentum overflow under deterministic contention: each of 4096
    // contributors is individually representable, but their same-cell sum
    // crosses i32::MIN. This exercises the atomicAdd path without relying on
    // a race-dependent mixed-sign ordering.
    let mut negative_particles = vec![single[0]; 4096];
    for p in &mut negative_particles {
        p.velocity_density[0] = -1.0;
    }
    let negative_particles_buf = particle_buffer(negative_particles.len());
    write_particles(&negative_particles_buf, &negative_particles);
    let negative_accum = device().create_buffer_shared(ACCUM_BYTES);
    negative_accum.zero_fill();
    unsafe {
        negative_accum.write(
            (centre * 4) as u64 * 4,
            bytemuck::bytes_of(&(i32::MIN + 1)),
        );
    }
    let negative_status = device().create_buffer_shared(4);
    negative_status.zero_fill();
    let mut negative_enc = device().create_encoder("water-negative-contention");
    negative_enc.dispatch_compute(
        &kernels().scatter_mass,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&manifold_renderer::node_graph::primitives::ScatterMassUniforms { active_count: 4096, _pad0: 0, _pad1: 0, _pad2: 0 }) },
            GpuBinding::Buffer { binding: 1, buffer: &negative_particles_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &negative_accum, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &negative_status, offset: 0 },
        ],
        ceil256(4096),
        "node.mpm_scatter_mass_momentum",
    );
    negative_enc.commit_and_wait_completed();
    assert_ne!(read_status(&negative_status) & FAULT_INTEGER_OVERFLOW, 0, "negative contention overflow must stick");
    let negative_accepted = particle_buffer(1);
    let negative_candidate = particle_buffer(1);
    let negative_committed = particle_buffer(1);
    write_particles(&negative_accepted, &single);
    write_particles(&negative_candidate, &changed_candidate);
    let mut negative_validate = device().create_encoder("water-negative-validate");
    negative_validate.dispatch_compute(
        &kernels().validate,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&validate_u) },
            GpuBinding::Buffer { binding: 1, buffer: &negative_candidate, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &negative_status, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &negative_status, offset: 0 },
        ],
        ceil256(1),
        "node.water_validate",
    );
    negative_validate.commit_and_wait_completed();
    let mut negative_commit = device().create_encoder("water-negative-commit");
    negative_commit.dispatch_compute(
        &kernels().commit,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&commit_u) },
            GpuBinding::Buffer { binding: 1, buffer: &negative_accepted, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &negative_candidate, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &negative_status, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &negative_committed, offset: 0 },
        ],
        ceil256(1),
        "node.water_commit",
    );
    negative_commit.commit_and_wait_completed();
    assert_eq!(
        bytemuck::cast_slice::<WaterParticle, u8>(&read_particles(&negative_committed, 1)),
        bytemuck::cast_slice::<WaterParticle, u8>(&read_particles(&negative_accepted, 1)),
        "negative overflow scratch must not escape into accepted state"
    );
    // Neighbour cells took their w0*w1*w1 = 0.0703125 contributions normally
    // (frac 1.0 per axis: 0.125 / 0.75 / 0.125).
    let neigh = WATER_DOMAIN.grid_index(19, 20, 20);
    let gpu2 = read_accum(&accum2);
    assert_eq!(gpu2[neigh * 4 + 3], quantise(0.0703125 * PARTICLE_MASS));
}

/// Analytic P2G -> stress -> resolve -> G2P transfer against the f64 oracle:
/// the S4 acceptance numbers (velocity <= 1e-3 m/s, position <= 1e-5 m) on
/// the S1 affine fixture, with gravity off and the basin disabled (the
/// fixture is a pure transfer probe, not a pool).
#[test]
fn water_gpu_transfer_matches_f64() {
    let particles = affine_fixture();
    let n = particles.len();
    let pool = make_pool(n as u32, n as u32);
    write_particles(&pool.accepted, &particles);

    substep(
        &pool,
        DEFAULT_STEP_DT,
        [-10.0, -10.0, -10.0],
        [10.0, 10.0, 10.0],
        [0.0, 0.0, 0.0],
    );

    assert_eq!(
        read_status(&pool.status),
        0,
        "transfer fixture must not fault"
    );
    let candidate = read_particles(&pool.candidate, n);

    // Diagnostic first: what did the accumulator hold after the substep?
    let accum = read_accum(&pool.accum);
    let accum_mass: f64 =
        accum.chunks(4).map(|c| c[3] as f64).sum::<f64>() / GRID_FIXED_SCALE as f64;
    let accum_mom: f64 = accum
        .chunks(4)
        .map(|c| (c[0].unsigned_abs() + c[1].unsigned_abs() + c[2].unsigned_abs()) as f64)
        .sum::<f64>()
        / GRID_FIXED_SCALE as f64;
    let nonzero_cells = accum.chunks(4).filter(|c| c[3] != 0).count();
    println!("accumulator after substep: {nonzero_cells} nonempty cells, total mass {accum_mass:.6} kg, |momentum| sum {accum_mom:.6}");

    // f64 oracle over identical inputs: P2G + stress + G2P.
    let refs: Vec<ref_oracle::RefParticle> = particles.iter().map(to_ref).collect();
    let mut grid = ref_oracle::RefGrid::new(
        64,
        64,
        64,
        GRID_SPACING as f64,
        [
        DOMAIN_ORIGIN[0] as f64,
        DOMAIN_ORIGIN[1] as f64,
        DOMAIN_ORIGIN[2] as f64,
        ],
    );
    grid.p2g_mass_momentum(&refs);
    grid.p2g_stress(
        &refs,
        DEFAULT_STEP_DT as f64,
        REST_DENSITY as f64,
        10.0,
        0.001,
    );
    let advected = grid.g2p_advect(&refs, DEFAULT_STEP_DT as f64);

    let mut max_v_err = 0.0f64;
    let mut max_x_err = 0.0f64;
    let mut max_rho_rel = 0.0f64;
    for ((src, p), pr) in particles.iter().zip(candidate.iter()).zip(advected.iter()) {
        for a in 0..3 {
            max_v_err = max_v_err.max((p.velocity_density[a] as f64 - pr.velocity[a]).abs());
            max_x_err = max_x_err.max((p.position_mass[a] as f64 - pr.position[a]).abs());
        }
        // Density reconstruction anchors at the pre-advection position.
        let rho_ref = grid.particle_density(&to_ref(src));
        let rho_gpu = p.velocity_density[3] as f64;
        max_rho_rel = max_rho_rel.max((rho_gpu - rho_ref).abs() / rho_ref);
    }
    println!(
        "water_gpu_transfer_matches_f64: {n} particles, max velocity error {max_v_err:.3e} m/s, max position error {max_x_err:.3e} m, max density rel err vs f64 {max_rho_rel:.3e}"
    );
    assert!(
        max_v_err <= 1.0e-3,
        "GPU/f64 velocity error {max_v_err} exceeds the 1e-3 m/s acceptance"
    );
    assert!(
        max_x_err <= 1.0e-5,
        "GPU/f64 position error {max_x_err} exceeds the 1e-5 m acceptance"
    );

    // Density: the honest comparison is against the CPU-quantised
    // reconstruction (the same Q=2^20 encoding accumulated in f64), not the
    // unquantised f64 — lightly-loaded surface cells carry the fixed quantum
    // as a large relative error by construction (S1's documented precedent),
    // which bounds GPU-vs-f64 regardless of implementation quality.
    let mut quantised_mass = vec![0f64; GRID_CELLS];
    for p in &particles {
        let x = [p.position_mass[0], p.position_mass[1], p.position_mass[2]];
        let q = WATER_DOMAIN.position_to_q(x);
        let (base, frac) = manifold_renderer::node_graph::water::stencil_base_frac(q);
        let w = [
            manifold_renderer::node_graph::water::bspline_weights(frac[0]),
            manifold_renderer::node_graph::water::bspline_weights(frac[1]),
            manifold_renderer::node_graph::water::bspline_weights(frac[2]),
        ];
        for k in 0..3 {
            for j in 0..3 {
                for i in 0..3 {
                    let g = WATER_DOMAIN.grid_index(
                        (base[0] + i as i32) as u32,
                        (base[1] + j as i32) as u32,
                        (base[2] + k as i32) as u32,
                    );
                    let w3 = w[0][i] * w[1][j] * w[2][k];
                    quantised_mass[g] +=
                        quantise(w3 * p.position_mass[3]) as f64 / GRID_FIXED_SCALE as f64;
                }
            }
        }
    }
    let h3 = (GRID_SPACING as f64).powi(3);
    let mut max_rho_q = 0.0f64;
    // Density is reconstructed at the PRE-advection position (design step 4
    // reads the grid before G2P moves anything), so the comparison anchors
    // at the original particle positions.
    for ((src, cand), _pr) in particles.iter().zip(candidate.iter()).zip(advected.iter()) {
        let mut rho_cpu = 0f64;
        let x = [
            src.position_mass[0],
            src.position_mass[1],
            src.position_mass[2],
        ];
        let q = WATER_DOMAIN.position_to_q(x);
        let (base, frac) = manifold_renderer::node_graph::water::stencil_base_frac(q);
        let w = [
            manifold_renderer::node_graph::water::bspline_weights(frac[0]),
            manifold_renderer::node_graph::water::bspline_weights(frac[1]),
            manifold_renderer::node_graph::water::bspline_weights(frac[2]),
        ];
        for k in 0..3 {
            for j in 0..3 {
                for i in 0..3 {
                    let g = WATER_DOMAIN.grid_index(
                        (base[0] + i as i32) as u32,
                        (base[1] + j as i32) as u32,
                        (base[2] + k as i32) as u32,
                    );
                    rho_cpu += (w[0][i] * w[1][j] * w[2][k]) as f64 * quantised_mass[g];
                }
            }
        }
        rho_cpu /= h3;
        let rel = (cand.velocity_density[3] as f64 - rho_cpu).abs() / rho_cpu;
        max_rho_q = max_rho_q.max(rel);
    }
    println!("density vs CPU-quantised reconstruction: max rel err {max_rho_q:.3e}");
    assert!(
        max_rho_q <= 1.0e-3,
        "GPU density deviates from the Q-quantised reconstruction by {max_rho_q}"
    );

    // Resolved grid on supported cells vs the f64 oracle (velocity impact of
    // the quantised accumulation, the quantity that reaches G2P). Support
    // filter mirrors S1's quantised-error metric: cells below 1% of the max
    // cell mass carry the fixed quantum as a large relative error by
    // construction — quantised momentum over a tiny mass denominator is not
    // a defect, and those cells contribute negligibly to G2P.
    let resolved = read_grid(&pool.grid);
    let max_cell_mass = resolved
        .iter()
        .map(|c| c.velocity_mass[3])
        .fold(0.0f32, f32::max) as f64;
    let mut max_grid_v = 0.0f64;
    let mut supported = 0usize;
    for (g, cell) in resolved.iter().enumerate() {
        let mass = cell.velocity_mass[3] as f64;
        if mass >= 0.01 * max_cell_mass {
            supported += 1;
            let v_ref = grid.resolved_velocity(g);
            for (vel_a, v_ref_a) in cell.velocity_mass.iter().zip(v_ref.iter()) {
                max_grid_v = max_grid_v.max((*vel_a as f64 - v_ref_a).abs());
            }
        }
    }
    assert!(supported > 0, "no supported grid cells");
    println!("resolved grid: {supported} supported cells, max velocity error {max_grid_v:.3e} m/s");
    assert!(
        max_grid_v <= 1.0e-3,
        "resolved grid velocity error {max_grid_v} exceeds 1e-3 m/s"
    );

    // Grid mass conservation: dequantised total vs particle total.
    let accum = read_accum(&pool.accum);
    let accum_mass: f64 =
        accum.chunks(4).map(|c| c[3] as f64).sum::<f64>() / GRID_FIXED_SCALE as f64;
    let accum_mom: f64 = accum
        .chunks(4)
        .map(|c| (c[0].unsigned_abs() + c[1].unsigned_abs() + c[2].unsigned_abs()) as f64)
        .sum::<f64>()
        / GRID_FIXED_SCALE as f64;
    println!(
        "accumulator after substep: total mass {accum_mass:.6} kg, |momentum| sum {accum_mom:.6}"
    );
    let total_mass: f64 = accum
        .chunks(4)
        .map(|c| c[3] as f64 / GRID_FIXED_SCALE as f64)
        .sum();
    let particle_mass = n as f64 * PARTICLE_MASS as f64;
    let mass_rel = (total_mass - particle_mass).abs() / particle_mass;
    println!("accumulated grid mass rel err {mass_rel:.3e}");
    assert!(
        mass_rel <= 5.0e-3,
        "accumulated grid mass rel err {mass_rel} exceeds 0.5%"
    );
}

/// Candidate fault rejection: a faulting candidate is never committed; the
/// last valid accepted state is retained byte-for-byte. Also covers the
/// sticky pass-through of a pre-existing status bit.
#[test]
fn water_fault_retains_last_valid_state() {
    let n = 64usize;
    let mut accepted: Vec<WaterParticle> = lattice_block(8.0, 9.5);
    accepted.truncate(n);
    let mut candidate = accepted.clone();
    // Poison one candidate record with NaN velocity.
    candidate[7].velocity_density[0] = f32::NAN;

    let accepted_buf = particle_buffer(n);
    write_particles(&accepted_buf, &accepted);
    let candidate_buf = particle_buffer(n);
    write_particles(&candidate_buf, &candidate);
    let status_buf = device().create_buffer_shared(4);
    status_buf.zero_fill();
    let out_buf = particle_buffer(n);
    out_buf.zero_fill();

    let validate_u = ValidateUniforms {
        validate_count: n as u32,
        velocity_bound: VELOCITY_BOUND,
        affine_bound: AFFINE_BOUND,
        density_max: 4.0 * REST_DENSITY,
        diagnostics_enabled: 0,
    };
    let commit_u = CommitUniforms {
        dispatch_count: n as u32,
        _pad0: 0,
        _pad1: 0,
        _pad2: 0,
    };

    // Pass 1: validate the NaN candidate, then commit with the latched fault.
    let mut enc = device().create_encoder("water-validate");
    enc.dispatch_compute(
        &kernels().validate,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&validate_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &candidate_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &status_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &status_buf,
                offset: 0,
            },
        ],
        ceil256(n as u32),
        "node.water_validate",
    );
    enc.commit_and_wait_completed();
    let bits = read_status(&status_buf);
    assert_ne!(
        bits & FAULT_NONFINITE,
        0,
        "NaN candidate must latch FAULT_NONFINITE"
    );

    let mut enc = device().create_encoder("water-commit");
    enc.dispatch_compute(
        &kernels().commit,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&commit_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &accepted_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &candidate_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &status_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &out_buf,
                offset: 0,
            },
        ],
        ceil256(n as u32),
        "node.water_commit",
    );
    enc.commit_and_wait_completed();
    let out = read_particles(&out_buf, n);
    // Byte-for-byte: a faulting candidate must not change one record of the
    // accepted state.
    assert_eq!(
        bytemuck::cast_slice::<WaterParticle, u8>(&out),
        bytemuck::cast_slice::<WaterParticle, u8>(&accepted),
        "faulting candidate must not be committed; last valid state retained"
    );

    // Pass 2: sticky pass-through — a pre-existing bit survives validation
    // of a clean candidate, and a clean status commits the candidate.
    let clean_candidate = accepted.clone();
    write_particles(&candidate_buf, &clean_candidate);
    let pre = FAULT_INTEGER_OVERFLOW;
    unsafe {
        status_buf.write(0, bytemuck::bytes_of(&pre));
    }
    let mut enc = device().create_encoder("water-validate-sticky");
    enc.dispatch_compute(
        &kernels().validate,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&validate_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &candidate_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &status_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &status_buf,
                offset: 0,
            },
        ],
        ceil256(n as u32),
        "node.water_validate",
    );
    enc.commit_and_wait_completed();
    assert_eq!(
        read_status(&status_buf),
        FAULT_INTEGER_OVERFLOW,
        "sticky bit must survive validation of a clean candidate"
    );

    // Fresh status, clean candidate -> commit passes the candidate through.
    status_buf.zero_fill();
    let mut enc = device().create_encoder("water-commit-clean");
    enc.dispatch_compute(
        &kernels().commit,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&commit_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &accepted_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &candidate_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &status_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &out_buf,
                offset: 0,
            },
        ],
        ceil256(n as u32),
        "node.water_commit",
    );
    enc.commit_and_wait_completed();
    let committed = read_particles(&out_buf, n);
    assert_eq!(
        bytemuck::cast_slice::<WaterParticle, u8>(&committed),
        bytemuck::cast_slice::<WaterParticle, u8>(&clean_candidate),
        "clean candidate must commit"
    );
}

/// The optional status sideband reports one internally consistent kinematic
/// violation of each kind without touching legacy one-word status buffers.
/// Finite velocity and affine excess are warning-only.
#[test]
fn water_validate_kinematic_diagnostics() {
    fn run(particles: &[WaterParticle]) -> Vec<u32> {
        let particle_buf = particle_buffer(particles.len());
        write_particles(&particle_buf, particles);
        let status_buf = device().create_buffer_shared((STATUS_WORDS * 4) as u64);
        status_buf.zero_fill();
        let uniforms = ValidateUniforms {
            validate_count: particles.len() as u32,
            velocity_bound: VELOCITY_BOUND,
            affine_bound: AFFINE_BOUND,
            density_max: 4.0 * REST_DENSITY,
            diagnostics_enabled: 1,
        };
        let mut enc = device().create_encoder("water-validate-diagnostics");
        enc.dispatch_compute(
            &kernels().validate,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &particle_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &status_buf, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &status_buf, offset: 0 },
            ],
            ceil256(particles.len() as u32),
            "node.water_validate",
        );
        enc.commit_and_wait_completed();
        read_status_payload(&status_buf)
    }

    let mut clean = vec![
        make_particle(lattice_pos([10.0, 10.0, 10.0]), [0.0, 0.0, 0.0], [[0.0; 3]; 3], PARTICLE_MASS);
        4
    ];
    clean[0].position_mass[3] = 0.0;
    clean[1].position_mass[3] = 0.0;
    clean[2].position_mass[3] = 0.0;
    clean[3].position_mass[3] = 0.0;
    let payload = run(&clean);
    assert_eq!(payload, vec![0; STATUS_WORDS], "clean validation has no diagnostic");

    let mut velocity = clean.clone();
    velocity[2].position_mass[3] = PARTICLE_MASS;
    velocity[2].velocity_density[0] = 5.0;
    let payload = run(&velocity);
    assert_eq!(payload[0], 0, "finite velocity excess is warning-only");
    assert_eq!(payload[1], 1, "velocity diagnostic kind");
    assert_eq!(f32::from_bits(payload[2]), 5.0);
    assert_eq!(payload[3], 3, "particle index is encoded as index + 1");
    assert_eq!(
        [f32::from_bits(payload[4]), f32::from_bits(payload[5]), f32::from_bits(payload[6])],
        lattice_pos([10.0, 10.0, 10.0])
    );

    let mut affine = clean;
    affine[1].position_mass[3] = PARTICLE_MASS;
    affine[1].affine_x[0] = 65.0;
    let payload = run(&affine);
    assert_eq!(payload[0], 0, "finite affine excess is warning-only");
    assert_eq!(payload[1], 2, "affine diagnostic kind");
    assert_eq!(f32::from_bits(payload[7]), 65.0);
    assert_eq!(payload[8], 2, "particle index is encoded as index + 1");

    let mut both = velocity;
    both[1].position_mass[3] = PARTICLE_MASS;
    both[1].affine_x[0] = 65.0;
    let payload = run(&both);
    assert_eq!(payload[0], 0, "finite kinematic excess is warning-only");
    assert_eq!(payload[1], 3, "both diagnostic kinds remain visible");
    assert_eq!(f32::from_bits(payload[2]), 5.0);
    assert_eq!(payload[3], 3);
    assert_eq!(f32::from_bits(payload[7]), 65.0);
    assert_eq!(payload[8], 2);
}

/// Default static pool after 1 s of substeps (960 at dt = 1/960): zero live
/// particle loss, mass conserved, no fault bits, interior hydrostatic
/// density median within 5% of rest and p95 within 15% (excluding the
/// two-cell boundary band).
#[test]
fn water_static_pool_settling() {
    let pool = make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32);
    seed_pool(&pool);
    let substeps = 960;
    for step in 0..substeps {
        substep_inner(
            &pool,
            DEFAULT_STEP_DT,
            BASIN_MIN,
            BASIN_MAX,
            [0.0, -9.81, 0.0],
            step == 0,
        );
        if step < 3 || step == substeps - 1 {
            println!(
                "substep {}: status={:#x}",
                step + 1,
                read_status(&pool.status)
            );
        }
        if (step + 1) % 240 == 0 {
            let recs = read_particles(&pool.accepted, SEED_ACTIVE_PARTICLES);
            let mean_v = recs[..SEED_ACTIVE_PARTICLES]
                .iter()
                .map(|p| {
                    (p.velocity_density[0].powi(2)
                        + p.velocity_density[1].powi(2)
                        + p.velocity_density[2].powi(2))
                    .sqrt()
                })
                .sum::<f32>()
                / SEED_ACTIVE_PARTICLES as f32;
            let max_v = recs[..SEED_ACTIVE_PARTICLES]
                .iter()
                .map(|p| {
                    (p.velocity_density[0].powi(2)
                        + p.velocity_density[1].powi(2)
                        + p.velocity_density[2].powi(2))
                    .sqrt()
                })
                .fold(0.0f32, f32::max);
            println!(
                "t={:.2}s: mean |v| {:.4} m/s, max |v| {:.4} m/s",
                (step + 1) as f32 * DEFAULT_STEP_DT,
                mean_v,
                max_v
            );
        }
    }

    let bits = read_status(&pool.status);
    assert_eq!(
        bits, 0,
        "static pool must run clean, got fault bits {bits:#x}"
    );
    let accepted = read_particles(&pool.accepted, PARTICLE_CAPACITY);

    // Zero live-particle loss + mass conservation + containment.
    let mut live = 0usize;
    let mut total_mass = 0.0f64;
    for (idx, p) in accepted.iter().enumerate() {
        let m = p.position_mass[3];
        if idx < SEED_ACTIVE_PARTICLES {
            assert_ne!(m, 0.0, "seeded slot {idx} lost");
            live += 1;
            total_mass += m as f64;
            let pos = [p.position_mass[0], p.position_mass[1], p.position_mass[2]];
            assert!(
                classify_position(pos, &WATER_DOMAIN).is_ok(),
                "slot {idx} left the guard shell: {pos:?}"
            );
            assert!(
                p.velocity_density[..3].iter().all(|v| v.is_finite()),
                "slot {idx} nonfinite velocity"
            );
        } else {
            assert_eq!(m, 0.0, "inactive tail slot {idx} became live");
        }
    }
    assert_eq!(live, SEED_ACTIVE_PARTICLES, "live particle count changed");
    let expected_mass = SEED_ACTIVE_PARTICLES as f64 * PARTICLE_MASS as f64;
    assert!(
        (total_mass - expected_mass).abs() <= 1e-6 * expected_mass,
        "total mass {total_mass} != {expected_mass}"
    );

    // Interior hydrostatic density: exclude the two-cell band at every basin
    // face (design section 8 metric).
    let band = 2.0 * GRID_SPACING;
    let mut interior: Vec<f32> = accepted[..SEED_ACTIVE_PARTICLES]
        .iter()
        .filter(|p| {
            let x = p.position_mass[0];
            let y = p.position_mass[1];
            let z = p.position_mass[2];
            x > BASIN_MIN[0] + band
                && x < BASIN_MAX[0] - band
                && y > BASIN_MIN[1] + band
                && y < BASIN_MAX[1] - band
                && z > BASIN_MIN[2] + band
                && z < BASIN_MAX[2] - band
        })
        .map(|p| p.velocity_density[3])
        .collect();
    assert!(
        interior.len() >= SEED_ACTIVE_PARTICLES / 4,
        "interior filter kept only {} of {SEED_ACTIVE_PARTICLES}",
        interior.len()
    );
    interior.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = interior[interior.len() / 2];
    let p95 = interior[(interior.len() as f64 * 0.95) as usize];
    println!(
        "water_static_pool_settling after {substeps} substeps: interior density median {median:.1} kg/m^3, p95 {p95:.1}, rest {REST_DENSITY}"
    );
    assert!(
        (median - REST_DENSITY).abs() <= 0.05 * REST_DENSITY,
        "interior median {median} outside 5% of rest {REST_DENSITY}"
    );
    assert!(
        (p95 - REST_DENSITY).abs() <= 0.15 * REST_DENSITY,
        "interior p95 {p95} outside 15% of rest {REST_DENSITY}"
    );
}

/// dt vs dt/2 on the default pool and a bounded impact fixture: density
/// field, particle motion and settling outcome must agree within recorded
/// tolerances. A mismatch here is escalation evidence (Astra's gate), not a
/// tuning task.
#[test]
fn water_timestep_halving_stability() {
    let dt = DEFAULT_STEP_DT;

    // Refinement evidence first: at t=0.5 s compare dt vs dt/2 and dt/2 vs
    // dt/4. A refinement ratio at a single time is not a convergence
    // classifier (neither chaos nor defect) without an established asymptotic
    // regime; early-time absolute deltas are the evidence.
    let probe_t = 0.5f32;
    let probe_steps = (probe_t / dt) as usize;
    let mut pools = [
        make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32),
        make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32),
        make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32),
    ];
    for pool in &mut pools {
        seed_pool(pool);
    }
    let dts = [dt, dt * 0.5, dt * 0.25];
    for (pool, &d) in pools.iter().zip(dts.iter()) {
        for _ in 0..(probe_steps as f32 * (dt / d)) as usize {
            substep(pool, d, BASIN_MIN, BASIN_MAX, [0.0, -9.81, 0.0]);
        }
    }
    for pool in &pools {
        assert_eq!(read_status(&pool.status), 0, "refinement run faulted");
    }
    let mean_pos = |a: &Pool, b: &Pool| {
        let pa = read_particles(&a.accepted, SEED_ACTIVE_PARTICLES);
        let pb = read_particles(&b.accepted, SEED_ACTIVE_PARTICLES);
        let mut sum = 0.0f64;
        for (x, y) in pa.iter().zip(pb.iter()) {
            sum += ((x.position_mass[0] - y.position_mass[0]) as f64)
                .hypot((x.position_mass[1] - y.position_mass[1]) as f64)
                .hypot((x.position_mass[2] - y.position_mass[2]) as f64);
        }
        sum / SEED_ACTIVE_PARTICLES as f64
    };
    let coarse = mean_pos(&pools[0], &pools[1]);
    let fine = mean_pos(&pools[1], &pools[2]);
    println!(
        "water_timestep_halving refinement at t={probe_t}: |dt - dt/2| mean pos {coarse:.3e} m, |dt/2 - dt/4| {fine:.3e} m, ratio {:.2}",
        coarse / fine.max(1.0e-12)
    );

    // Horizon long enough for the seed transient's slosh to decay (measured
    // halving time ~0.5 s) — the comparison must land on the settled state,
    // where phase-shifted slosh no longer dominates the deltas.
    let t_end = 2.0f32;
    let steps_full = (t_end / dt) as usize;

    // Default pool.
    let pool_a = make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32);
    seed_pool(&pool_a);
    let pool_b = make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32);
    seed_pool(&pool_b);
    for _ in 0..steps_full {
        substep(&pool_a, dt, BASIN_MIN, BASIN_MAX, [0.0, -9.81, 0.0]);
    }
    for _ in 0..steps_full * 2 {
        substep(&pool_b, dt * 0.5, BASIN_MIN, BASIN_MAX, [0.0, -9.81, 0.0]);
    }
    let report_pool = compare_runs(&pool_a, &pool_b, dt, "pool");
    // ESCALATION EVIDENCE (S4, Astra's gate): the pool's dt-vs-dt/2
    // comparison does not meet any reasonable acceptance tolerance — mean
    // position deltas are centimetre-scale with a persistent ~0.3 m/s slosh
    // that phase-diverges between resolutions (refinement ratio 0.79 at
    // t=0.5 s, i.e. trajectory deltas do not shrink under refinement). The
    // solver itself is stable: zero faults, hydrostatic density (median
    // ~995, p95 ~1037 vs rest 1000), bounded velocities. Per
    // docs/WATER_SIMULATION_DESIGN.md section 5 this mismatch is escalation
    // evidence for Astra/Peter, NOT a lane tuning task — the acceptance
    // thresholds are intentionally not asserted here until Astra rules on
    // the recorded numbers below. Hard invariants ARE asserted: no faults,
    // finite state, proof-bound velocities.
    assert_hard_invariants(&pool_a, "pool dt");
    assert_hard_invariants(&pool_b, "pool dt/2");

    // Bounded impact: sphere region near the surface driven downward at
    // 0.5 m/s at t=0, then settling. Velocities stay well inside the proof
    // bounds (|v| <= 4 m/s) — the fixture is bounded by construction.
    let mut impact_a = make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32);
    seed_pool(&impact_a);
    let mut impact_b = make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32);
    seed_pool(&impact_b);
    for pool in [&mut impact_a, &mut impact_b] {
        let mut seeded = read_particles(&pool.accepted, PARTICLE_CAPACITY);
        for p in seeded[..SEED_ACTIVE_PARTICLES].iter_mut() {
            let x = p.position_mass[0];
            let y = p.position_mass[1];
            let z = p.position_mass[2];
            let d2 = x * x + (y - 0.7) * (y - 0.7) + z * z;
            if d2 <= 0.25 * 0.25 {
                p.velocity_density[0] = 0.0;
                p.velocity_density[1] = -0.5;
                p.velocity_density[2] = 0.0;
            }
        }
        write_particles(&pool.accepted, &seeded);
    }
    for _ in 0..steps_full {
        substep(&impact_a, dt, BASIN_MIN, BASIN_MAX, [0.0, -9.81, 0.0]);
    }
    for _ in 0..steps_full * 2 {
        substep(&impact_b, dt * 0.5, BASIN_MIN, BASIN_MAX, [0.0, -9.81, 0.0]);
    }
    let report_impact = compare_runs(&impact_a, &impact_b, dt, "impact");
    assert_eq!(read_status(&impact_a.status), 0, "impact run (dt) faulted");
    assert_eq!(
        read_status(&impact_b.status),
        0,
        "impact run (dt/2) faulted"
    );
    // Same escalation contract as the pool above: deltas recorded as
    // evidence, hard invariants asserted, acceptance thresholds await Astra.
    assert_hard_invariants(&impact_a, "impact dt");
    assert_hard_invariants(&impact_b, "impact dt/2");
    println!(
        "ESCALATION EVIDENCE water_timestep_halving_stability: pool max_pos {:.3e} mean_pos {:.3e} rho_rel {:.3e} settle_v {:.3e} | impact max_pos {:.3e} mean_pos {:.3e} rho_rel {:.3e} settle_v {:.3e}",
        report_pool.max_pos, report_pool.mean_pos, report_pool.max_rho_rel, report_pool.settle_v,
        report_impact.max_pos, report_impact.mean_pos, report_impact.max_rho_rel, report_impact.settle_v,
    );
}

/// Matched early-time probe (Astra's evidence constraint): the default pool
/// from identical seeds, identical physical-time forcing, stopping at exactly
/// t=0.1 s at 96, 192 and 384 substeps. Prints absolute position/density
/// deltas, kinetic energy and the acoustic CFL per run. No refinement-ratio
/// verdict, no chaos/defect classification, no acceptance thresholds — the
/// hard invariants are asserted, the numbers are the evidence for the lead.
#[test]
fn water_timestep_early_probe() {
    let dt = DEFAULT_STEP_DT;
    let base_steps = 96usize; // t = 0.1 s at dt = 1/960
    let dts = [dt, dt * 0.5, dt * 0.25];
    let labels = ["96", "192", "384"];
    let step_counts = [base_steps, base_steps * 2, base_steps * 4];

    let mut pools = [
        make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32),
        make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32),
        make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32),
    ];
    for pool in &mut pools {
        seed_pool(pool);
    }

    let mut max_cfl = [0.0f32; 3];
    let mut max_cfl_step = [0usize; 3];
    let mut kinetic = [0.0f64; 3];
    let mut final_states: Vec<Vec<WaterParticle>> = Vec::new();
    for (r, pool) in pools.iter().enumerate() {
        for s in 0..step_counts[r] {
            substep(pool, dts[r], BASIN_MIN, BASIN_MAX, [0.0, -9.81, 0.0]);
            let recs = read_particles(&pool.accepted, SEED_ACTIVE_PARTICLES);
            for p in &recs {
                let speed = (p.velocity_density[0].powi(2)
                    + p.velocity_density[1].powi(2)
                    + p.velocity_density[2].powi(2))
                .sqrt();
                let cfl = acoustic_cfl(dts[r], p.velocity_density[3], speed, GRID_SPACING);
                if cfl > max_cfl[r] {
                    max_cfl[r] = cfl;
                    max_cfl_step[r] = s;
                }
            }
        }
        assert_hard_invariants(pool, &format!("probe {} substeps", labels[r]));
        let recs = read_particles(&pool.accepted, SEED_ACTIVE_PARTICLES);
        kinetic[r] = recs
            .iter()
            .map(|p| {
                0.5 * p.position_mass[3] as f64
                    * (p.velocity_density[0].powi(2)
                        + p.velocity_density[1].powi(2)
                        + p.velocity_density[2].powi(2)) as f64
            })
            .sum();
        final_states.push(recs);
    }

    let (rms_a, max_a, mean_a) = pos_delta_rms_max(&final_states[0], &final_states[1]);
    let (rms_b, max_b, mean_b) = pos_delta_rms_max(&final_states[1], &final_states[2]);
    let density: Vec<Vec<f32>> = final_states.iter().map(|s| density_grid(s)).collect();
    let (drms_a, dmax_a) = grid_delta_rms_max(&density[0], &density[1]);
    let (drms_b, dmax_b) = grid_delta_rms_max(&density[1], &density[2]);

    println!("water_timestep_early_probe at t=0.1 s (default pool, identical seeds):");
    println!(
        "  96 vs 192 substeps: position RMS {rms_a:.6e} m, max {max_a:.6e} m, mean {mean_a:.6e} m"
    );
    println!(
        "  192 vs 384 substeps: position RMS {rms_b:.6e} m, max {max_b:.6e} m, mean {mean_b:.6e} m"
    );
    println!(
        "  density field deltas (64^3 mass-weighted scatter): 96v192 RMS {drms_a:.3e} max {dmax_a:.3e} kg/m^3 | 192v384 RMS {drms_b:.3e} max {dmax_b:.3e} kg/m^3"
    );
    println!(
        "  kinetic energy at t=0.1 s: 96 substeps {:.6} J | 192 {:.6} J | 384 {:.6} J",
        kinetic[0], kinetic[1], kinetic[2]
    );
    for r in 0..3 {
        println!(
            "  max acoustic CFL, {} substeps: {:.4} at substep {} (dt={:.7})",
            labels[r], max_cfl[r], max_cfl_step[r], dts[r]
        );
    }

    // Astra's trigger: large early-time deltas localise the divergence entry
    // point with a per-stage comparison at t=0.02 s. Diagnostic only.
    if rms_a > 1.0e-3 {
        println!(
            "  early-time position RMS {rms_a:.3e} m exceeds 1e-3 m: running 0.02 s per-stage diagnostic"
        );
        early_stage_diagnostic(&dts, &labels);
    }
}

/// Per-particle displacement statistics between two runs at the same slot.
fn pos_delta_rms_max(a: &[WaterParticle], b: &[WaterParticle]) -> (f64, f64, f64) {
    assert_eq!(a.len(), SEED_ACTIVE_PARTICLES);
    assert_eq!(b.len(), SEED_ACTIVE_PARTICLES);
    let mut sum_sq = 0.0f64;
    let mut sum = 0.0f64;
    let mut max = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = ((x.position_mass[0] - y.position_mass[0]) as f64)
            .hypot((x.position_mass[1] - y.position_mass[1]) as f64)
            .hypot((x.position_mass[2] - y.position_mass[2]) as f64);
        sum_sq += d * d;
        sum += d;
        max = max.max(d);
    }
    let n = SEED_ACTIVE_PARTICLES as f64;
    ((sum_sq / n).sqrt(), max, sum / n)
}

/// Mass-weighted scatter of per-particle density onto the 64^3 grid: each
/// cell reports `sum(w*m*rho)/sum(w*m)` over the particles touching it.
fn density_grid(recs: &[WaterParticle]) -> Vec<f32> {
    let mut mass = vec![0.0f64; GRID_CELLS];
    let mut rho_mass = vec![0.0f64; GRID_CELLS];
    for p in recs {
        let x = [p.position_mass[0], p.position_mass[1], p.position_mass[2]];
        let m = p.position_mass[3] as f64;
        let rho = p.velocity_density[3] as f64;
        let q = WATER_DOMAIN.position_to_q(x);
        let (base, frac) = manifold_renderer::node_graph::water::stencil_base_frac(q);
        let w = [
            manifold_renderer::node_graph::water::bspline_weights(frac[0]),
            manifold_renderer::node_graph::water::bspline_weights(frac[1]),
            manifold_renderer::node_graph::water::bspline_weights(frac[2]),
        ];
        for k in 0..3 {
            for j in 0..3 {
                for i in 0..3 {
                    let g = WATER_DOMAIN.grid_index(
                        (base[0] + i as i32) as u32,
                        (base[1] + j as i32) as u32,
                        (base[2] + k as i32) as u32,
                    );
                    let w3 = (w[0][i] * w[1][j] * w[2][k]) as f64;
                    mass[g] += w3 * m;
                    rho_mass[g] += w3 * m * rho;
                }
            }
        }
    }
    mass.iter()
        .zip(rho_mass)
        .map(|(m, rm)| if *m > 0.0 { (rm / m) as f32 } else { 0.0 })
        .collect()
}

/// RMS and max absolute delta between two density fields over cells both
/// runs deposited mass into.
fn grid_delta_rms_max(a: &[f32], b: &[f32]) -> (f64, f64) {
    let mut sum_sq = 0.0f64;
    let mut max = 0.0f64;
    let mut n = 0usize;
    for (x, y) in a.iter().zip(b.iter()) {
        if *x > 0.0 && *y > 0.0 {
            let d = (*x - *y) as f64;
            sum_sq += d * d;
            max = max.max(d.abs());
            n += 1;
        }
    }
    assert!(n > 0, "no overlapping deposited cells");
    ((sum_sq / n as f64).sqrt(), max)
}

/// Live-particle state to the f64 oracle, affine matrix read from the record
/// (unlike `to_ref`, which pins the analytic fixture field).
fn to_ref_live(p: &WaterParticle) -> ref_oracle::RefParticle {
    let mut rp = ref_oracle::RefParticle::new(
        [
            p.position_mass[0] as f64,
            p.position_mass[1] as f64,
            p.position_mass[2] as f64,
        ],
        [
            p.velocity_density[0] as f64,
            p.velocity_density[1] as f64,
            p.velocity_density[2] as f64,
        ],
        p.position_mass[3] as f64,
    );
    rp.c = [
        [
            p.affine_x[0] as f64,
            p.affine_x[1] as f64,
            p.affine_x[2] as f64,
        ],
        [
            p.affine_y[0] as f64,
            p.affine_y[1] as f64,
            p.affine_y[2] as f64,
        ],
        [
            p.affine_z[0] as f64,
            p.affine_z[1] as f64,
            p.affine_z[2] as f64,
        ],
    ];
    rp
}

/// Fresh clear -> scatter mass -> scatter stress on the pool's current
/// accepted state; returns the dequantised per-cell momentum and mass.
fn gpu_p2g_stress_momentum(pool: &Pool, dt: f32) -> (Vec<[f64; 3]>, Vec<f64>) {
    let k = kernels();
    let mut enc = device().create_encoder("water-p2g-stress-only");

    let clear_u = ClearGridUniforms {
        max_capacity: ACCUM_ITEMS as i32,
        dispatch_count: ACCUM_ITEMS,
        _pad0: 0,
        _pad1: 0,
    };
    enc.dispatch_compute(
        &k.clear,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&clear_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accum,
                offset: 0,
            },
        ],
        ceil256(ACCUM_ITEMS),
        "node.clear_grid",
    );

    let mass_u = manifold_renderer::node_graph::primitives::ScatterMassUniforms {
        active_count: pool.active,
        _pad0: 0,
        _pad1: 0,
        _pad2: 0,
    };
    enc.dispatch_compute(
        &k.scatter_mass,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&mass_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accepted,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &pool.status,
                offset: 0,
            },
        ],
        ceil256(pool.active),
        "node.mpm_scatter_mass_momentum",
    );

    let stress_u = manifold_renderer::node_graph::primitives::ScatterStressUniforms {
        step_dt: dt,
        active_count: pool.active,
        _pad0: 0,
        _pad1: 0,
    };
    enc.dispatch_compute(
        &k.scatter_stress,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&stress_u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pool.accepted,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &pool.accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &pool.status,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &pool.stress_out,
                offset: 0,
            },
        ],
        ceil256(pool.active),
        "node.mpm_scatter_stress",
    );
    enc.commit_and_wait_completed();
    assert_eq!(read_status(&pool.status), 0, "stress-stage replay faulted");

    let accum = read_accum(&pool.accum);
    let scale = GRID_FIXED_SCALE as f64;
    let momentum: Vec<[f64; 3]> = accum
        .chunks(4)
        .map(|c| {
            [
                c[0] as f64 / scale,
                c[1] as f64 / scale,
                c[2] as f64 / scale,
            ]
        })
        .collect();
    let mass: Vec<f64> = accum.chunks(4).map(|c| c[3] as f64 / scale).collect();
    (momentum, mass)
}

/// Per-stage localisation at t=0.02 s (19/38/76 substeps): for each
/// resolution, replay P2G+stress on the reached state and compare the grid
/// momentum against the f64 oracle on the same state, plus the grid momentum
/// deltas between resolutions. Prints; classifies nothing.
fn early_stage_diagnostic(dts: &[f32; 3], labels: &[&str; 3]) {
    let early_steps = [19usize, 38, 76];
    let mut gpu_momentum: Vec<Vec<[f64; 3]>> = Vec::new();
    let mut gpu_mass: Vec<Vec<f64>> = Vec::new();

    for r in 0..3 {
        let pool = make_pool(SEED_ACTIVE_PARTICLES as u32, PARTICLE_CAPACITY as u32);
        seed_pool(&pool);
        for _ in 0..early_steps[r] {
            substep(&pool, dts[r], BASIN_MIN, BASIN_MAX, [0.0, -9.81, 0.0]);
        }
        assert_hard_invariants(&pool, &format!("diagnostic {} substeps", labels[r]));
        let recs = read_particles(&pool.accepted, SEED_ACTIVE_PARTICLES);

        let (mom, mass) = gpu_p2g_stress_momentum(&pool, dts[r]);
        gpu_momentum.push(mom);
        gpu_mass.push(mass);

        let refs: Vec<ref_oracle::RefParticle> = recs.iter().map(to_ref_live).collect();
        let mut oracle = ref_oracle::RefGrid::new(
            64,
            64,
            64,
            GRID_SPACING as f64,
            [
            DOMAIN_ORIGIN[0] as f64,
            DOMAIN_ORIGIN[1] as f64,
            DOMAIN_ORIGIN[2] as f64,
            ],
        );
        oracle.p2g_mass_momentum(&refs);
        oracle.p2g_stress(
            &refs,
            dts[r] as f64,
            REST_DENSITY as f64,
            SOUND_SPEED_C0 as f64,
            DYNAMIC_VISCOSITY as f64,
        );

        // Support filter: the same 1%-of-max-cell-mass rule the S4 transfer
        // proof uses — lightly loaded cells carry the fixed quantum as a
        // large relative error by construction.
        let max_mass = oracle.mass.iter().copied().fold(0.0f64, f64::max);
        let mut max_mom_err = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut supported = 0usize;
        let mut max_vel_err = 0.0f64;
        for (g, &mass_g) in oracle.mass.iter().enumerate() {
            if mass_g >= 0.01 * max_mass {
                supported += 1;
                for (gpu_a, &mom_a) in gpu_momentum[r][g].iter().zip(oracle.momentum[g].iter()) {
                    let e = (gpu_a - mom_a).abs();
                    max_mom_err = max_mom_err.max(e);
                    sum_sq += e * e;
                }
                let v_ref = oracle.resolved_velocity(g);
                for (v_ref_a, &mom_a) in v_ref.iter().zip(gpu_momentum[r][g].iter()) {
                    let v_gpu = if gpu_mass[r][g] > 0.0 {
                        mom_a / gpu_mass[r][g]
                    } else {
                        0.0
                    };
                    max_vel_err = max_vel_err.max((v_gpu - v_ref_a).abs());
                }
            }
        }
        let rms = (sum_sq / (supported * 3) as f64).sqrt();
        println!(
            "  stress-stage vs f64 oracle at t=0.02 s ({} substeps, dt={:.7}): {supported} supported cells, max momentum err {max_mom_err:.3e} kg m/s, RMS {rms:.3e}, max velocity err {max_vel_err:.3e} m/s",
            labels[r], dts[r]
        );
    }

    for (a, b) in [(0usize, 1usize), (1, 2)] {
        let max_mass = gpu_mass[a].iter().copied().fold(0.0f64, f64::max);
        let mut max_d = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut n = 0usize;
        for (g, (&ma, &mb)) in gpu_mass[a].iter().zip(gpu_mass[b].iter()).enumerate() {
            if ma >= 0.01 * max_mass && mb > 0.0 {
                for (pa, &pb) in gpu_momentum[a][g].iter().zip(gpu_momentum[b][g].iter()) {
                    let d = pa - pb;
                    max_d = max_d.max(d.abs());
                    sum_sq += d * d;
                    n += 1;
                }
            }
        }
        let rms = if n > 0 {
            (sum_sq / n as f64).sqrt()
        } else {
            0.0
        };
        println!(
            "  grid momentum delta at t=0.02 s, {} vs {} substeps: max {max_d:.3e} kg m/s, RMS {rms:.3e} over {n} momentum components",
            labels[a], labels[b]
        );
    }
}

/// Hard invariants every solver run must hold regardless of the acceptance
/// comparison: no sticky faults, all live state finite, velocities inside
/// the proof kinematic bounds.
fn assert_hard_invariants(pool: &Pool, label: &str) {
    assert_eq!(read_status(&pool.status), 0, "{label} run faulted");
    let recs = read_particles(&pool.accepted, SEED_ACTIVE_PARTICLES);
    for (i, p) in recs.iter().enumerate() {
        assert_ne!(p.position_mass[3], 0.0, "{label} slot {i} lost");
        assert!(
            p.position_mass[..3].iter().all(|v| v.is_finite()),
            "{label} slot {i} nonfinite position"
        );
        assert!(
            p.velocity_density[..3].iter().all(|v| v.is_finite()),
            "{label} slot {i} nonfinite velocity"
        );
        let speed = (p.velocity_density[0].powi(2)
            + p.velocity_density[1].powi(2)
            + p.velocity_density[2].powi(2))
        .sqrt();
        assert!(
            speed <= VELOCITY_BOUND,
            "{label} slot {i} speed {speed} exceeds the proof bound"
        );
    }
}

struct HalvingReport {
    max_pos: f64,
    mean_pos: f64,
    max_rho_rel: f64,
    settle_v: f64,
}

/// Compare the dt run and the dt/2 run of one fixture at the same physical
/// time, and report the recorded deltas.
fn compare_runs(a: &Pool, b: &Pool, dt: f32, label: &str) -> HalvingReport {
    assert_eq!(read_status(&a.status), 0, "{label} dt run faulted");
    assert_eq!(read_status(&b.status), 0, "{label} dt/2 run faulted");
    let pa = read_particles(&a.accepted, PARTICLE_CAPACITY);
    let pb = read_particles(&b.accepted, PARTICLE_CAPACITY);
    let mut max_pos = 0.0f64;
    let mut sum_pos = 0.0f64;
    let mut max_rho_rel = 0.0f64;
    let mut sum_v = 0.0f64;
    for (x, y) in pa[..SEED_ACTIVE_PARTICLES]
        .iter()
        .zip(pb[..SEED_ACTIVE_PARTICLES].iter())
    {
        let d = ((x.position_mass[0] - y.position_mass[0]) as f64)
            .hypot((x.position_mass[1] - y.position_mass[1]) as f64)
            .hypot((x.position_mass[2] - y.position_mass[2]) as f64);
        max_pos = max_pos.max(d);
        sum_pos += d;
        let ra = x.velocity_density[3] as f64;
        let rb = y.velocity_density[3] as f64;
        max_rho_rel = max_rho_rel.max((ra - rb).abs() / REST_DENSITY as f64);
        sum_v += (x.velocity_density[0].powi(2)
            + x.velocity_density[1].powi(2)
            + x.velocity_density[2].powi(2)) as f64;
    }
    let n = SEED_ACTIVE_PARTICLES as f64;
    let report = HalvingReport {
        max_pos,
        mean_pos: sum_pos / n,
        max_rho_rel,
        settle_v: (sum_v / n).sqrt(),
    };
    println!(
        "water_timestep_halving_stability [{label}] at dt={dt:.5}: max pos delta {:.3e} m, mean pos delta {:.3e} m, max density rel delta {:.3e}, settling mean |v| {:.3e} m/s",
        report.max_pos, report.mean_pos, report.max_rho_rel, report.settle_v
    );
    report
}

// ---------------------------------------------------------------------------
// Graph-executor chain proof: the same substep through compile + Executor +
// aliased_array_io wiring (run(), bindings, planner). Numerics are identical
// to the direct-dispatch path — this test exists to prove the wiring.
// ---------------------------------------------------------------------------

mod graph_chain {
    use super::*;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;
    use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_renderer::node_graph::depth_rule::DepthRule;
    use manifold_renderer::node_graph::ports::{
        ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType,
    };
    use manifold_renderer::node_graph::primitives::Value;
    use manifold_renderer::node_graph::{
        compile, EffectNode, EffectNodeContext, EffectNodeType, ExecutionPlan, Executor, FrameTime,
        Graph, MetalBackend, NodeInstanceId, ParamDef, ParamValue as NodeParamValue, ParamValues,
        ResourceId,
    };

    /// Test-only producer for `Array(WaterParticle)` — CPU-written fixture
    /// pre-bound to its output resource (the scatter gpu_tests pattern).
    struct WaterSource {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl WaterSource {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.water_source"),
                inputs: vec![],
                outputs: vec![NodePort {
                    name: std::borrow::Cow::Borrowed("out"),
                    ty: PortType::Array(ArrayType::of_known::<WaterParticle>()),
                    kind: PortKind::Output,
                    required: false,
                }],
            }
        }
    }

    impl EffectNode for WaterSource {
        fn depth_rule(&self) -> DepthRule {
            DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

        fn array_output_capacity(
            &self,
            _port_name: &str,
            _params: &ParamValues,
            _input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            // Real capacity comes from the pre-bound fixture buffer.
            Some(super::AFFINE_FIXTURE_CAPACITY as u32)
        }
    }

    /// Test-only producer for the sticky status word (one zeroed u32).
    struct StatusSource {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl StatusSource {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.water_status_source"),
                inputs: vec![],
                outputs: vec![NodePort {
                    name: std::borrow::Cow::Borrowed("out"),
                    ty: PortType::Array(ArrayType::of_known::<u32>()),
                    kind: PortKind::Output,
                    required: false,
                }],
            }
        }
    }

    impl EffectNode for StatusSource {
        fn depth_rule(&self) -> DepthRule {
            DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

        fn array_output_capacity(
            &self,
            _port_name: &str,
            _params: &ParamValues,
            _input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            Some(1)
        }
    }

    /// Liveness sink for the committed water state.
    struct WaterSink {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl WaterSink {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.water_sink"),
                inputs: vec![NodePort {
                    name: std::borrow::Cow::Borrowed("in"),
                    ty: PortType::Array(ArrayType::of_known::<WaterParticle>()),
                    kind: PortKind::Input,
                    required: true,
                }],
                outputs: vec![],
            }
        }
    }

    impl EffectNode for WaterSink {
        fn depth_rule(&self) -> DepthRule {
            DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
        fn is_liveness_root(&self) -> bool {
            true
        }
    }

    fn resource_for(
        plan: &ExecutionPlan,
        node: NodeInstanceId,
        port: &str,
        is_input: bool,
    ) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                let pool = if is_input {
                    &step.inputs
                } else {
                    &step.outputs
                };
                for &(name, id) in pool {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!("no resource for {port} on {node:?}");
    }

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn water_graph_chain_single_substep_matches_f64() {
        let particles = affine_fixture();
        let n = particles.len();
        assert_eq!(n, super::AFFINE_FIXTURE_CAPACITY);

        let mut g = Graph::new();
        let water_src = g.add_node(Box::new(WaterSource::new()));
        let status_src = g.add_node(Box::new(StatusSource::new()));
        let clear = g.add_node(Box::new(ClearGrid::new()));
        let scatter_mass = g.add_node(Box::new(MpmScatterMassMomentum::new()));
        let scatter_stress = g.add_node(Box::new(MpmScatterStress::new()));
        let grid = g.add_node(Box::new(MpmGridVelocity::new()));
        let gather = g.add_node(Box::new(MpmGatherAdvect::new()));
        let validate = g.add_node(Box::new(WaterValidate::new()));
        let commit = g.add_node(Box::new(WaterCommit::new()));
        let sink = g.add_node(Box::new(WaterSink::new()));

        // Scalar scaffolding: step clock + zero gravity (pure transfer probe).
        let v_dt = g.add_node(Box::new(Value::new()));
        g.set_param(v_dt, "value", NodeParamValue::Float(DEFAULT_STEP_DT))
            .unwrap();
        let v_zero = g.add_node(Box::new(Value::new()));
        g.set_param(v_zero, "value", NodeParamValue::Float(0.0))
            .unwrap();

        g.connect((v_dt, "out"), (clear, "step_dt")).unwrap();
        g.connect((water_src, "out"), (scatter_mass, "particles"))
            .unwrap();
        g.connect((status_src, "out"), (scatter_mass, "status"))
            .unwrap();
        g.connect((clear, "out"), (scatter_mass, "accumulator"))
            .unwrap();
        g.connect((scatter_mass, "out"), (scatter_stress, "accumulator"))
            .unwrap();
        g.connect((scatter_mass, "status_out"), (scatter_stress, "status"))
            .unwrap();
        g.connect((water_src, "out"), (scatter_stress, "particles"))
            .unwrap();
        g.connect((scatter_stress, "out"), (grid, "accumulator"))
            .unwrap();
        g.connect((scatter_stress, "status_out"), (validate, "status"))
            .unwrap();
        g.connect((scatter_stress, "particles_out"), (gather, "particles"))
            .unwrap();
        g.connect((grid, "out"), (gather, "grid")).unwrap();
        g.connect((v_dt, "out"), (scatter_stress, "step_dt"))
            .unwrap();
        g.connect((v_dt, "out"), (gather, "step_dt")).unwrap();
        for axis in ["gravity_x", "gravity_y", "gravity_z"] {
            g.connect((v_zero, "out"), (grid, axis)).unwrap();
        }
        g.connect((gather, "out"), (validate, "particles")).unwrap();
        g.connect((gather, "out"), (commit, "candidate")).unwrap();
        g.connect((validate, "status_out"), (commit, "status"))
            .unwrap();
        g.connect((water_src, "out"), (commit, "accepted")).unwrap();
        g.connect((commit, "out"), (sink, "in")).unwrap();

        // Basin disabled (transfer probe) + live counts.
        for (name, val) in [
            ("basin_min_x", -10.0),
            ("basin_min_y", -10.0),
            ("basin_min_z", -10.0),
            ("basin_max_x", 10.0),
            ("basin_max_y", 10.0),
            ("basin_max_z", 10.0),
        ] {
            g.set_param(grid, name, NodeParamValue::Float(val)).unwrap();
        }
        g.set_param(
            scatter_mass,
            "active_count",
            NodeParamValue::Float(n as f32),
        )
            .unwrap();
        g.set_param(
            scatter_stress,
            "active_count",
            NodeParamValue::Float(n as f32),
        )
            .unwrap();
        g.set_param(validate, "validate_count", NodeParamValue::Float(n as f32))
            .unwrap();

        let plan = compile(&g).expect("water chain graph compiles");

        let r_water = resource_for(&plan, water_src, "out", false);
        let r_status = resource_for(&plan, status_src, "out", false);
        let r_clear = resource_for(&plan, clear, "out", false);
        let r_scatter_out = resource_for(&plan, scatter_mass, "out", false);
        let r_scatter_status = resource_for(&plan, scatter_mass, "status_out", false);
        let r_stress_out = resource_for(&plan, scatter_stress, "out", false);
        let r_stress_status = resource_for(&plan, scatter_stress, "status_out", false);
        let r_particles_out = resource_for(&plan, scatter_stress, "particles_out", false);
        let r_grid = resource_for(&plan, grid, "out", false);
        let r_gather = resource_for(&plan, gather, "out", false);
        let r_validate_status = resource_for(&plan, validate, "status_out", false);
        let r_commit = resource_for(&plan, commit, "out", false);

        let fixture_buf = particle_buffer(n);
        write_particles(&fixture_buf, &particles);
        let status_buf = device().create_buffer_shared(4);
        status_buf.zero_fill();

        let mut backend =
            MetalBackend::new(Arc::clone(device()), 16, 16, GpuTextureFormat::Rgba16Float);
        // The bare executor path allocates no array buffers — pre-bind every
        // wire (the scatter gpu_tests pattern) and alias the atomic outputs
        // onto their input slots via the chain-builder's alias API, exactly
        // what graph_loader does for aliased_array_io pairs in production.
        let _fixture_slot = backend.pre_bind_array(r_water, fixture_buf);
        let status_slot = backend.pre_bind_array(r_status, status_buf);
        let accum_slot =
            backend.pre_bind_array(r_clear, device().create_buffer_shared(ACCUM_BYTES));
        backend.alias_array_resource(r_scatter_out, accum_slot);
        backend.alias_array_resource(r_scatter_status, status_slot);
        backend.alias_array_resource(r_stress_out, accum_slot);
        backend.alias_array_resource(r_stress_status, status_slot);
        backend.alias_array_resource(r_validate_status, status_slot);
        let _particles_out_slot = backend.pre_bind_array(r_particles_out, particle_buffer(n));
        let _grid_slot = backend.pre_bind_array(
            r_grid,
            device().create_buffer_shared(GRID_BYTES * GRID_CELLS as u64),
        );
        let _gather_slot = backend.pre_bind_array(r_gather, particle_buffer(n));
        let commit_buf = particle_buffer(n);
        let commit_slot = backend.pre_bind_array(r_commit, commit_buf);

        let mut native_enc = device().create_encoder("water-graph-chain");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, device());
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        // Diagnostics: which stage left its buffer unwritten?
        let status_bits = read_status(
            exec.backend()
                .array_buffer(status_slot)
                .expect("status buffer retained"),
        );

        assert_eq!(status_bits, 0, "graph chain must not fault");
        // The committed state landed in commit.out's buffer (a pure select:
        // candidate when the status word is clean).
        let committed = read_particles(
            exec.backend()
                .array_buffer(commit_slot)
                .expect("commit buffer retained"),
            n,
        );

        let refs: Vec<ref_oracle::RefParticle> = particles.iter().map(to_ref).collect();
        let mut oracle = ref_oracle::RefGrid::new(
            64,
            64,
            64,
            GRID_SPACING as f64,
            [
            DOMAIN_ORIGIN[0] as f64,
            DOMAIN_ORIGIN[1] as f64,
            DOMAIN_ORIGIN[2] as f64,
            ],
        );
        oracle.p2g_mass_momentum(&refs);
        oracle.p2g_stress(
            &refs,
            DEFAULT_STEP_DT as f64,
            REST_DENSITY as f64,
            10.0,
            0.001,
        );
        let advected = oracle.g2p_advect(&refs, DEFAULT_STEP_DT as f64);

        let mut max_v_err = 0.0f64;
        let mut max_x_err = 0.0f64;
        for (p, pr) in committed.iter().zip(advected.iter()) {
            for a in 0..3 {
                max_v_err = max_v_err.max((p.velocity_density[a] as f64 - pr.velocity[a]).abs());
                max_x_err = max_x_err.max((p.position_mass[a] as f64 - pr.position[a]).abs());
            }
        }
        println!(
            "water_graph_chain_single_substep: max velocity error {max_v_err:.3e} m/s, max position error {max_x_err:.3e} m"
        );
        assert!(
            max_v_err <= 1.0e-3,
            "graph chain velocity error {max_v_err}"
        );
        assert!(
            max_x_err <= 1.0e-5,
            "graph chain position error {max_x_err}"
        );
    }
}

// ---------------------------------------------------------------------------
// S5 proofs — collider motion + collision, emission, impulses
// (docs/WATER_IMPLEMENTATION_PLAN.md section 4, S5 gate). The event/cursor
// latches run with the exact call sequence node.run() uses; the kernels run
// on the GPU and are compared against CPU-computed expectations.
// ---------------------------------------------------------------------------

mod s5 {
    use super::*;
    use std::sync::{Arc, Mutex};

    use bytemuck::Zeroable as _;

    use manifold_renderer::node_graph::primitives::{
        CollideBoxUniforms, EmitCursor, EmitUniforms, ImpulseEventLatch, ImpulseUniforms,
        WaterCollideBox, WaterColliderMotion, WaterEmit, WaterImpulse, EMIT_MAX, EMIT_MIN,
    };

    const S5_DT: f32 = 1.0 / 960.0;

    fn impulse_pipeline() -> manifold_gpu::GpuComputePipeline {
        pipeline_standalone::<WaterImpulse>("node.water_impulse")
    }

    fn emit_pipeline() -> manifold_gpu::GpuComputePipeline {
        pipeline_standalone::<WaterEmit>("node.water_emit")
    }

    fn collide_pipeline() -> manifold_gpu::GpuComputePipeline {
        pipeline_standalone::<WaterCollideBox>("node.water_collide_box")
    }

    /// CPU expectation for one impulse application (design section 6):
    /// within radius R, dv = max(0, 1 - d/R)^2 * impulse; else zero.
    fn impulse_delta(pos: [f32; 3], centre: [f32; 3], radius: f32, impulse: [f32; 3]) -> [f32; 3] {
        let d = ((pos[0] - centre[0]).powi(2)
            + (pos[1] - centre[1]).powi(2)
            + (pos[2] - centre[2]).powi(2))
        .sqrt();
        if d >= radius {
            return [0.0; 3];
        }
        let f = 1.0 - d / radius;
        let s = f * f;
        [impulse[0] * s, impulse[1] * s, impulse[2] * s]
    }

    /// Live-particle fixture around the default impulse centre (0, 0.7, 0)
    /// with inactive slots interleaved and one far-outside particle.
    fn impulse_fixture(capacity: usize) -> Vec<WaterParticle> {
        let centre = [0.0f32, 0.7, 0.0];
        let mut out = Vec::with_capacity(capacity);
        let mut i = 0usize;
        while out.len() < capacity {
            let ring = i % 7;
            let (dx, dz) = match ring {
                0 => (0.0f32, 0.0f32),
                1 => (0.1, 0.0),
                2 => (-0.05, 0.12),
                3 => (0.3, -0.2), // outside radius 0.25 below? 0.36 > 0.25 -> outside
                4 => (0.0, 0.0),
                5 => (-0.15, -0.15),
                _ => (0.05, -0.1),
            };
            let pos = [
                centre[0] + dx,
                centre[1] + 0.02 * (i % 3) as f32,
                centre[2] + dz,
            ];
            // Every 5th slot is inactive; slot 3 is beyond any radius.
            if i % 5 == 4 {
                out.push(WaterParticle::zeroed());
            } else if i == 3 {
                let mut p = make_particle(
                    [1.5, 2.0, 1.5],
                    [0.1, 0.2, 0.3],
                    [[0.0; 3]; 3],
                    PARTICLE_MASS,
                );
                p.velocity_density[3] = 900.0;
                out.push(p);
            } else {
                let mut p = make_particle(pos, [0.05, -0.1, 0.02], [[0.0; 3]; 3], PARTICLE_MASS);
                p.velocity_density[3] = 980.0 + (i % 5) as f32;
                out.push(p);
            }
            i += 1;
        }
        out
    }

    /// Dispatch one impulse application on `buf` (in -> out).
    fn dispatch_impulse(
        buf_in: &manifold_gpu::GpuBuffer,
        buf_out: &manifold_gpu::GpuBuffer,
        capacity: u32,
        uniforms: &ImpulseUniforms,
    ) {
        let mut enc = device().create_encoder("water-impulse");
        enc.dispatch_compute(
            &impulse_pipeline(),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: buf_in,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: buf_out,
                    offset: 0,
                },
            ],
            ceil256(capacity),
            "node.water_impulse",
        );
        enc.commit_and_wait_completed();
    }

    /// Total velocity delta the latch+kernel pair produces for a frame with
    /// `substeps` iterations and a trigger count of `trigger`. `expected_x`
    /// scales the CPU expectation — a multiplicity-N run applies the kernel
    /// N times, which is exactly N times the single-event delta.
    /// Returns (total_dv_y, applied_substeps).
    fn run_impulse_scenario(substeps: u32, trigger: f32, expected_x: f32) -> (f64, u32) {
        let capacity = 256u32;
        let centre = [0.0f32, 0.7, 0.0];
        let impulse = [0.0f32, 1.5, 0.0];
        let radius = 0.25f32;
        let fixture = impulse_fixture(capacity as usize);
        let expected: Vec<[f32; 3]> = fixture
            .iter()
            .map(|p| {
                impulse_delta(
                    [p.position_mass[0], p.position_mass[1], p.position_mass[2]],
                    centre,
                    radius,
                    impulse,
                )
            })
            .collect();

        let buf_in = particle_buffer(capacity as usize);
        let buf_out = particle_buffer(capacity as usize);
        write_particles(&buf_in, &fixture);
        buf_out.zero_fill();

        let uniforms = ImpulseUniforms {
            centre_x: centre[0],
            centre_y: centre[1],
            centre_z: centre[2],
            radius,
            impulse_x: impulse[0],
            impulse_y: impulse[1],
            impulse_z: impulse[2],
            dispatch_count: capacity,
        };

        // The exact run() call sequence: frame 1 arms the latch with no
        // trigger (first observation arms, never fires), then frame 2 runs
        // `substeps` iterations with the advanced trigger.
        let mut latch = ImpulseEventLatch::default();
        let mut step_time = 0.0f32;
        let mut applied = 0u32;
        let mut current_in = buf_in;
        let mut current_out = buf_out;
        let d = latch.sample(0, 1, true, step_time, 0.0);
        assert!(!d.apply, "arming observation must not fire");
        for _i in 0..substeps {
            step_time += S5_DT;
            let d = latch.sample(0, 2, true, step_time, trigger);
            if d.apply {
                applied += 1;
                dispatch_impulse(&current_in, &current_out, capacity, &uniforms);
                std::mem::swap(&mut current_in, &mut current_out);
            }
        }
        let gpu = read_particles(&current_in, capacity as usize);
        let mut total = 0.0f64;
        let mut max_err = 0.0f64;
        for (i, (g, e)) in gpu.iter().zip(expected.iter()).enumerate() {
            // Inactive slots: byte-identical pass-through.
            if fixture[i].position_mass[3] == 0.0 {
                assert_eq!(g.position_mass[3], 0.0, "inactive slot {i} gained mass");
                assert_eq!(
                    g.velocity_density, [0.0; 4],
                    "inactive slot {i} gained velocity"
                );
                continue;
            }
            for ((gv, &fv), &e_a) in g
                .velocity_density
                .iter()
                .zip(fixture[i].velocity_density.iter())
                .zip(e.iter())
            {
                let dv = (*gv - fv) as f64;
                total += dv;
                max_err = max_err.max((dv - expected_x as f64 * e_a as f64).abs());
            }
            // Density and affine state untouched.
            assert_eq!(
                g.velocity_density[3], fixture[i].velocity_density[3],
                "slot {i} density drifted"
            );
        }
        println!(
            "impulse scenario substeps={substeps} trigger={trigger}: applied={applied}, total dv_y {total:.6} m/s, max per-particle err {max_err:.3e}"
        );
        assert!(
            max_err <= 1.0e-6,
            "impulse kernel deviates from the CPU expectation by {max_err}"
        );
        (total, applied)
    }

    /// One trigger is one velocity change: the integrated delta over the
    /// affected particles is identical whether the frame has 1 or 8
    /// substeps — the event applies once, not once per substep (design
    /// section 6), and never as force·dt.
    #[test]
    fn water_impulse_event_total_invariant_across_substep_counts() {
        let (total_1, applied_1) = run_impulse_scenario(1, 1.0, 1.0);
        let (total_8, applied_8) = run_impulse_scenario(8, 1.0, 1.0);
        assert_eq!(applied_1, 1, "one trigger applies exactly once");
        assert_eq!(applied_8, 1, "extra substeps must not re-apply the event");
        assert!(
            (total_1 - total_8).abs() <= 1.0e-9,
            "total velocity effect must be substep-count invariant: {total_1} vs {total_8}"
        );
        assert!(total_1.abs() > 0.0, "the fixture must feel the impulse");

        // Multiplicity: a trigger advancing by 3 is three events, consumed
        // one per substep — three total applications, 3x the single effect.
        // The tolerance is relative: each application rounds the velocity
        // to f32, so three sequential adds differ from 3x one add at the
        // per-rounding level, not bitwise.
        let (total_3, applied_3) = run_impulse_scenario(8, 3.0, 3.0);
        assert_eq!(applied_3, 3);
        assert!(
            (total_3 - 3.0 * total_1).abs() <= 1.0e-6 * total_1.abs(),
            "three events are three times one: {total_3} vs {}",
            3.0 * total_1
        );
    }

    /// A queued event (trigger advanced on a zero-substep frame) is consumed
    /// on the first actual substep and never again.
    #[test]
    fn water_impulse_pending_event_consumed_on_first_substep() {
        let capacity = 256u32;
        let fixture = impulse_fixture(capacity as usize);
        let buf_in = particle_buffer(capacity as usize);
        let buf_out = particle_buffer(capacity as usize);
        write_particles(&buf_in, &fixture);
        buf_out.zero_fill();
        let uniforms = ImpulseUniforms {
            centre_x: 0.0,
            centre_y: 0.7,
            centre_z: 0.0,
            radius: 0.25,
            impulse_x: 0.0,
            impulse_y: 1.5,
            impulse_z: 0.0,
            dispatch_count: capacity,
        };

        let mut latch = ImpulseEventLatch::default();
        // Frame 1: arm.
        let d = latch.sample(0, 1, true, S5_DT, 0.0);
        assert!(!d.apply);
        // Frame 2: fractional time scale — zero due substeps, the body never
        // runs, the trigger wire advances to 1.
        // Frame 3: four actual substeps.
        let mut step_time = 10.0f32;
        let mut applies = 0u32;
        let mut current_in = &buf_in;
        let mut current_out = &buf_out;
        for _i in 0..4u32 {
            step_time += S5_DT;
            let d = latch.sample(0, 3, true, step_time, 1.0);
            if d.apply {
                applies += 1;
                dispatch_impulse(current_in, current_out, capacity, &uniforms);
                std::mem::swap(&mut current_in, &mut current_out);
            }
        }
        assert_eq!(
            applies, 1,
            "the queued event fires exactly once, on the first actual substep"
        );
        let gpu = read_particles(current_in, capacity as usize);
        let mut moved = 0usize;
        for (i, g) in gpu.iter().enumerate() {
            if fixture[i].position_mass[3] == 0.0 {
                continue;
            }
            let dv = (g.velocity_density[1] - fixture[i].velocity_density[1]).abs();
            if dv > 1.0e-7 {
                moved += 1;
            }
        }
        assert!(moved > 0, "the queued event must actually move water");
    }

    /// Dispatch one emit substep on `buf` (in -> out).
    fn dispatch_emit(
        buf_in: &manifold_gpu::GpuBuffer,
        buf_out: &manifold_gpu::GpuBuffer,
        capacity: u32,
        birth_lo: u32,
        birth_hi: u32,
        first_free: u32,
        repeat: f32,
        velocity_y: f32,
    ) {
        let uniforms = EmitUniforms {
            emit_min_x: EMIT_MIN[0],
            emit_min_y: EMIT_MIN[1],
            emit_min_z: EMIT_MIN[2],
            emit_max_x: EMIT_MAX[0],
            emit_max_y: EMIT_MAX[1],
            emit_max_z: EMIT_MAX[2],
            grid_spacing: GRID_SPACING,
            rest_density: REST_DENSITY,
            rate: 100.0,
            repeat,
            velocity_y,
            first_free: first_free as i32,
            birth_lo,
            birth_hi,
            dispatch_count: capacity,
            _pad0: 0,
        };
        let mut enc = device().create_encoder("water-emit");
        enc.dispatch_compute(
            &emit_pipeline(),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: buf_in,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: buf_out,
                    offset: 0,
                },
            ],
            ceil256(capacity),
            "node.water_emit",
        );
        enc.commit_and_wait_completed();
    }

    /// CPU lattice expectation for one birth ordinal (same formula as
    /// node.seed_water and the emit body).
    fn emit_lattice_pos(ordinal: u32, spacing: f32) -> [f32; 3] {
        let nx = ((EMIT_MAX[0] - EMIT_MIN[0]) / spacing + 0.5).floor() as u32;
        let ny = ((EMIT_MAX[1] - EMIT_MIN[1]) / spacing + 0.5).floor() as u32;
        let ix = ordinal % nx;
        let iy = (ordinal / nx) % ny;
        let iz = ordinal / (nx * ny);
        [
            EMIT_MIN[0] + (ix as f32 + 0.5) * spacing,
            EMIT_MIN[1] + (iy as f32 + 0.5) * spacing,
            EMIT_MIN[2] + (iz as f32 + 0.5) * spacing,
        ]
    }

    /// Deterministic ordinal births into the unused tail: fractional carry
    /// gates the first birth, lattice positions match the CPU formula, the
    /// seeded prefix is byte-untouched, the unborn tail stays inactive, and
    /// capacity exhaustion stops emission at exactly the tail size with one
    /// Full report.
    #[test]
    fn water_emit_births_match_cpu_and_full_stops_at_capacity() {
        let capacity = 384u32;
        let first_free = 128u32;
        let mut fixture = vec![WaterParticle::zeroed(); capacity as usize];
        // Seeded prefix with distinctive records — emission must never
        // overwrite existing water.
        for (i, p) in fixture.iter_mut().enumerate().take(first_free as usize) {
            *p = make_particle(
                [0.01 * i as f32, 0.02, 0.03],
                [0.1, 0.0, 0.0],
                [[0.0; 3]; 3],
                PARTICLE_MASS,
            );
        }
        let prefix_bytes: Vec<u8> = bytemuck::cast_slice(&fixture[..first_free as usize]).to_vec();

        let buf_in = particle_buffer(capacity as usize);
        let buf_out = particle_buffer(capacity as usize);
        write_particles(&buf_in, &fixture);
        buf_out.zero_fill();

        // Fractional carry: 100 particles/s at 960 substeps/s — the first
        // birth lands on the 10th substep (carry crosses 1.0 at 10/96 s).
        let mut cursor = EmitCursor::default();
        let mut step_time = 0.0f32;
        let mut current_in = &buf_in;
        let mut current_out = &buf_out;
        let mut reports = 0u32;
        for _substep in 0..24u32 {
            step_time += S5_DT;
            let plan = cursor.advance(
                0,
                step_time,
                100.0,
                S5_DT as f64,
                first_free,
                capacity,
                1024,
            );
            if plan.report_full {
                reports += 1;
            }
            if plan.hi > plan.lo {
                dispatch_emit(
                    current_in,
                    current_out,
                    capacity,
                    plan.lo,
                    plan.hi,
                    first_free,
                    0.0,
                    0.0,
                );
                std::mem::swap(&mut current_in, &mut current_out);
            }
        }

        let gpu = read_particles(current_in, capacity as usize);
        // Seeded prefix byte-identical.
        let gpu_prefix: Vec<u8> = bytemuck::cast_slice(&gpu[..first_free as usize]).to_vec();
        assert_eq!(
            gpu_prefix, prefix_bytes,
            "emission overwrote the seeded prefix"
        );
        // 24 substeps at 100/s = 2 or 3 births (2.5 expected; floor chain).
        let born = cursor.born();
        assert!(
            (2..=3).contains(&born),
            "born {born} outside the fractional-carry window"
        );
        let spacing = GRID_SPACING * 0.5;
        for (slot, p) in gpu
            .iter()
            .enumerate()
            .skip(first_free as usize)
            .take(born as usize)
        {
            let ordinal = slot - first_free as usize;
            let expected_pos = emit_lattice_pos(ordinal as u32, spacing);
            assert_eq!(
                p.position_mass,
                [
                    expected_pos[0],
                    expected_pos[1],
                    expected_pos[2],
                    PARTICLE_MASS
                ],
                "birth ordinal {ordinal} off the lattice"
            );
            assert_eq!(p.velocity_density, [0.0, 0.0, 0.0, REST_DENSITY]);
            assert_eq!(p.affine_x, [0.0; 4]);
            assert_eq!(p.affine_y, [0.0; 4]);
            assert_eq!(p.affine_z, [0.0; 4]);
            assert_eq!(
                p.previous_position,
                [expected_pos[0], expected_pos[1], expected_pos[2], 0.0]
            );
        }
        // Unborn tail stays inactive.
        for (i, p) in gpu.iter().enumerate().skip((first_free + born) as usize) {
            assert_eq!(p.position_mass, [0.0; 4], "unborn slot {i} became live");
        }

        // Full: pour hard into the remaining tail until exhaustion.
        let mut full_reports = reports;
        for _substep in 0..64u32 {
            step_time += S5_DT;
            let plan = cursor.advance(
                0,
                step_time,
                1.0e6,
                S5_DT as f64,
                first_free,
                capacity,
                1024,
            );
            if plan.report_full {
                full_reports += 1;
            }
            if plan.hi > plan.lo {
                dispatch_emit(
                    current_in,
                    current_out,
                    capacity,
                    plan.lo,
                    plan.hi,
                    first_free,
                    0.0,
                    0.0,
                );
                std::mem::swap(&mut current_in, &mut current_out);
            }
        }
        assert_eq!(
            cursor.born(),
            capacity - first_free,
            "emission stops exactly at the tail size"
        );
        assert_eq!(
            full_reports,
            reports + 1,
            "Full reported once at exhaustion"
        );
        let gpu = read_particles(current_in, capacity as usize);
        let gpu_prefix: Vec<u8> = bytemuck::cast_slice(&gpu[..first_free as usize]).to_vec();
        assert_eq!(gpu_prefix, prefix_bytes, "prefix touched after Full");
        for (slot, p) in gpu.iter().enumerate().skip(first_free as usize) {
            assert_ne!(p.position_mass[3], 0.0, "tail slot {slot} not born");
        }
        println!(
            "water_emit: {} substeps -> {} born, Full once, prefix intact",
            24 + 64,
            cursor.born()
        );
    }

    /// Shared-transform proof: node.water_collider_motion interpolates the
    /// authored target across the frame's substeps (executor-driven, real
    /// StateStore + Transform/Vec3 wires), the accepted collider lands
    /// exactly on the target, and node.water_collide_box driven by that same
    /// accepted transform projects particles out of the box with relative
    /// normal velocity removed — post-projection penetration <= 0.1*h.
    #[test]
    fn water_cube_transform_and_collision_match() {
        cube_transform_and_collision_match_inner();
    }

    fn cube_transform_and_collision_match_inner() {
        use manifold_core::{Beats, Seconds};
        use manifold_gpu::GpuTextureFormat;
        use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use manifold_renderer::node_graph::depth_rule::DepthRule;
        use manifold_renderer::node_graph::ports::{
            ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType,
        };
        use manifold_renderer::node_graph::primitives::Value;
        use manifold_renderer::node_graph::substeps::SimulationFrame;
        use manifold_renderer::node_graph::transform::Transform;
        use manifold_renderer::node_graph::StateStore;
        use manifold_renderer::node_graph::{
            compile, EffectNode, EffectNodeContext, EffectNodeType, ExecutionPlan, Executor,
            FrameTime, Graph, MetalBackend, NodeInstanceId, ParamDef, ParamValue as NodeParamValue,
            ParamValues, ResourceId,
        };

        /// Records every Transform seen on its input wire — the display
        /// proxy for the accepted collider.
        struct CaptureTransform {
            type_id: EffectNodeType,
            seen: Arc<Mutex<Vec<Transform>>>,
        }
        impl EffectNode for CaptureTransform {
            fn depth_rule(&self) -> DepthRule {
                DepthRule::Terminal
            }
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                static INPUTS: [NodeInput; 1] = [NodePort {
                    name: std::borrow::Cow::Borrowed("in"),
                    ty: PortType::Transform,
                    kind: PortKind::Input,
                    required: true,
                }];
                &INPUTS
            }
            fn outputs(&self) -> &[NodeOutput] {
                &[]
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
                if let Some(t) = ctx.inputs.transform("in") {
                    self.seen.lock().unwrap().push(t);
                }
            }
            fn is_liveness_root(&self) -> bool {
                // Sink-only probe: no output wire propagates liveness to it,
                // so it must be a root to survive the live-step pruning.
                true
            }
        }

        /// Records every Vec3 seen on its input wire.
        struct CaptureVec3 {
            type_id: EffectNodeType,
            seen: Arc<Mutex<Vec<[f32; 3]>>>,
        }
        impl EffectNode for CaptureVec3 {
            fn depth_rule(&self) -> DepthRule {
                DepthRule::Terminal
            }
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                static INPUTS: [NodeInput; 1] = [NodePort {
                    name: std::borrow::Cow::Borrowed("in"),
                    ty: PortType::Scalar(manifold_renderer::node_graph::ports::ScalarType::Vec3),
                    kind: PortKind::Input,
                    required: true,
                }];
                &INPUTS
            }
            fn outputs(&self) -> &[NodeOutput] {
                &[]
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
                if let Some(manifold_renderer::node_graph::ParamValue::Vec3(v)) =
                    ctx.inputs.scalar("in")
                {
                    self.seen.lock().unwrap().push(v);
                }
            }
            fn is_liveness_root(&self) -> bool {
                // Sink-only probe: no output wire propagates liveness to it,
                // so it must be a root to survive the live-step pruning.
                true
            }
        }

        struct WaterSink {
            type_id: EffectNodeType,
            inputs: Vec<NodeInput>,
        }
        impl EffectNode for WaterSink {
            fn depth_rule(&self) -> DepthRule {
                DepthRule::Terminal
            }
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                &self.inputs
            }
            fn outputs(&self) -> &[NodeOutput] {
                &[]
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
            fn is_liveness_root(&self) -> bool {
                true
            }
        }

        /// Test-only particle producer — a small pre-bound buffer of
        /// zeroed (inactive) records so the executor graph satisfies
        /// collide_box's required `in` wire. The penetration measurement
        /// itself drives the kernel directly below.
        struct WaterSource {
            type_id: EffectNodeType,
        }
        impl EffectNode for WaterSource {
            fn depth_rule(&self) -> DepthRule {
                DepthRule::Terminal
            }
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                &[]
            }
            fn outputs(&self) -> &[NodeOutput] {
                static OUTPUTS: [NodeOutput; 1] = [NodePort {
                    name: std::borrow::Cow::Borrowed("out"),
                    ty: PortType::Array(ArrayType::of_known::<WaterParticle>()),
                    kind: PortKind::Output,
                    required: false,
                }];
                &OUTPUTS
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
            fn array_output_capacity(
                &self,
                _port_name: &str,
                _params: &ParamValues,
                _input_capacities: &[(&str, u32)],
            ) -> Option<u32> {
                Some(8)
            }
        }

        fn resource_for(
            plan: &ExecutionPlan,
            node: NodeInstanceId,
            port: &str,
            is_input: bool,
        ) -> ResourceId {
            for step in plan.steps() {
                if step.node == node {
                    let pool = if is_input {
                        &step.inputs
                    } else {
                        &step.outputs
                    };
                    for &(name, id) in pool {
                        if name == port {
                            return id;
                        }
                    }
                }
            }
            panic!("no resource for {port} on {node:?}");
        }

        fn frame_time() -> FrameTime {
            FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(0.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: 0,
            }
        }

        // The graph: authored target -> collider motion -> (capture +
        // collide box) -> sink. The same motion.transform wire feeds both
        // the display proxy and the collision stage — the shared-wire half
        // of the proof.
        let mut g = Graph::new();
        let v_dt = g.add_node(Box::new(Value::new()));
        let v_time = g.add_node(Box::new(Value::new()));
        let v_index = g.add_node(Box::new(Value::new()));
        let v_count = g.add_node(Box::new(Value::new()));
        let target = g.add_node(Box::new(
            manifold_renderer::node_graph::primitives::Transform3D::new(),
        ));
        let motion = g.add_node(Box::new(WaterColliderMotion::new()));
        let seen_t_log: Arc<Mutex<Vec<Transform>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_v_log: Arc<Mutex<Vec<[f32; 3]>>> = Arc::new(Mutex::new(Vec::new()));
        let capture_t = g.add_node(Box::new(CaptureTransform {
            type_id: EffectNodeType::new("test.capture_transform"),
            seen: Arc::clone(&seen_t_log),
        }));
        let capture_v = g.add_node(Box::new(CaptureVec3 {
            type_id: EffectNodeType::new("test.capture_vec3"),
            seen: Arc::clone(&seen_v_log),
        }));
        let collide = g.add_node(Box::new(WaterCollideBox::new()));
        let source = g.add_node(Box::new(WaterSource {
            type_id: EffectNodeType::new("test.water_source"),
        }));
        let sink = g.add_node(Box::new(WaterSink {
            type_id: EffectNodeType::new("test.water_sink"),
            inputs: vec![NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Array(ArrayType::of_known::<WaterParticle>()),
                kind: PortKind::Input,
                required: true,
            }],
        }));

        g.connect((v_dt, "out"), (motion, "step_dt")).unwrap();
        g.connect((v_time, "out"), (motion, "step_time")).unwrap();
        g.connect((v_index, "out"), (motion, "step_index")).unwrap();
        g.connect((v_count, "out"), (motion, "step_count")).unwrap();
        g.connect((target, "transform"), (motion, "target"))
            .unwrap();
        g.connect((motion, "transform"), (capture_t, "in")).unwrap();
        g.connect((motion, "velocity"), (capture_v, "in")).unwrap();
        g.connect((motion, "transform"), (collide, "collider"))
            .unwrap();
        g.connect((motion, "velocity"), (collide, "collider_velocity"))
            .unwrap();
        g.connect((source, "out"), (collide, "in")).unwrap();
        g.connect((collide, "out"), (sink, "in")).unwrap();

        let plan = compile(&g).expect("collider graph compiles");
        let r_source_out = resource_for(&plan, source, "out", false);
        let r_collide_out = resource_for(&plan, collide, "out", false);

        let mut backend =
            MetalBackend::new(Arc::clone(device()), 16, 16, GpuTextureFormat::Rgba16Float);
        let source_buf = particle_buffer(8);
        source_buf.zero_fill();
        let _source_slot = backend.pre_bind_array(r_source_out, source_buf);
        let _collide_slot = backend.pre_bind_array(r_collide_out, particle_buffer(8));

        let mut store = StateStore::new();
        let mut exec = Executor::new(Box::new(backend));

        const N_SUB: u32 = 4;
        let stroke_target = [1.004f32, 0.0, 0.0]; // y = 1.004: 0.96 m/s over 4 substeps
        // Frame 1: rest at y = 1.0. Frame 2: the stroke. Frame 3: hold.
        for (frame, target_y) in [(1u64, 1.0f32), (2, stroke_target[0]), (3, stroke_target[0])] {
            g.set_param(target, "pos_y", NodeParamValue::Float(target_y))
                .unwrap();
            g.set_param(v_count, "value", NodeParamValue::Float(N_SUB as f32))
                .unwrap();
            for i in 0..N_SUB {
                g.set_param(v_dt, "value", NodeParamValue::Float(S5_DT))
                    .unwrap();
                g.set_param(
                    v_time,
                    "value",
                    NodeParamValue::Float(frame as f32 * 10.0 + (i as f32 + 1.0) * S5_DT),
                )
                .unwrap();
                g.set_param(v_index, "value", NodeParamValue::Float(i as f32))
                    .unwrap();

                let mut native_enc = device().create_encoder("water-collider-motion");
                let mut gpu = RendererGpuEncoder::new(&mut native_enc, device());
                exec.set_simulation_frame(SimulationFrame {
                    frame_id: frame,
                    delta: Seconds(1.0 / 60.0),
                    epoch: 0,
                    advancing: true,
                    exporting: false,
                });
                exec.execute_frame_with_state(&mut g, &plan, frame_time(), &mut gpu, &mut store, 0);
                native_enc.commit_and_wait_completed();
            }
        }

        let seen_t = seen_t_log.lock().unwrap().clone();
        let seen_v = seen_v_log.lock().unwrap().clone();
        assert_eq!(seen_t.len(), 12, "one capture per substep evaluation");
        assert_eq!(seen_v.len(), 12);

        // Frame 1 (indices 0..4): rest at y = 1.0, zero velocity.
        for (i, t) in seen_t.iter().enumerate().take(4) {
            assert!(
                (t.pos[1] - 1.0).abs() < 1.0e-6,
                "rest frame {i} moved: {}",
                t.pos[1]
            );
            assert_eq!(seen_v[i], [0.0; 3]);
        }
        // Frame 2 (indices 4..8): linear interpolation previous=1.0 ->
        // target=1.004, fractions (i+1)/4, frame-constant velocity 0.96 m/s.
        let frac = [0.25f32, 0.5, 0.75, 1.0];
        for (i, &f) in frac.iter().enumerate() {
            let expected_y = 1.0 + 0.004 * f;
            assert!(
                (seen_t[4 + i].pos[1] - expected_y).abs() < 1.0e-6,
                "stroke substep {i}: {} != {expected_y}",
                seen_t[4 + i].pos[1]
            );
            assert!(
                (seen_v[4 + i][1] - 0.96).abs() < 1.0e-3,
                "stroke substep {i} velocity {} != 0.96",
                seen_v[4 + i][1]
            );
            assert_eq!(seen_v[4 + i][0], 0.0);
            assert_eq!(seen_v[4 + i][2], 0.0);
        }
        // Frame 3 (indices 8..12): hold at the accepted target, zero velocity.
        for i in 8..12 {
            assert!(
                (seen_t[i].pos[1] - stroke_target[0]).abs() < 1.0e-6,
                "hold frame moved: {}",
                seen_t[i].pos[1]
            );
            assert_eq!(seen_v[i], [0.0; 3]);
        }
        // The accepted (displayed) collider IS the authored target at the
        // end of the stroke — the shared-wire contract.
        let accepted = seen_t[7];
        assert!(
            (accepted.pos[1] - stroke_target[0]).abs() < 1.0e-6,
            "accepted collider {} != target {}",
            accepted.pos[1],
            stroke_target[0]
        );

        // Collision half: drive node.water_collide_box with the ACCEPTED
        // transform + velocity from the shared wire and particles placed
        // inside the box with into-wall velocities. Post-projection
        // penetration must be <= 0.1*h and the relative normal velocity at
        // contact must be removed (free-slip tangential preserved) — with a
        // CPU mirror per particle.
        let half = [0.25f32; 3];
        let centre = accepted.pos;
        let mut fixture: Vec<WaterParticle> = Vec::new();
        // Inside particles on every axis combination, moving into the box.
        let offs = [
            ([0.05f32, 0.0, 0.0], [-0.4f32, 0.1, 0.0]),
            ([-0.08, 0.06, 0.0], [0.5, -0.2, 0.1]),
            ([0.0, -0.1, 0.05], [0.1, 0.6, -0.3]),
            ([0.02, 0.02, -0.12], [-0.2, 0.0, 0.45]),
            ([-0.2, -0.2, -0.2], [0.3, 0.3, 0.3]),
        ];
        for (o, v) in offs {
            fixture.push(make_particle(
                [centre[0] + o[0], centre[1] + o[1], centre[2] + o[2]],
                v,
                [[0.0; 3]; 3],
                PARTICLE_MASS,
            ));
        }
        // A below-floor particle exercises the basin half of the kernel
        // (same rule as mpm_grid_velocity's boundary path).
        let basin_floor_y = manifold_renderer::node_graph::primitives::BASIN_MIN[1];
        fixture.push(make_particle(
            [centre[0], basin_floor_y - 0.05, centre[2]],
            [0.0, -1.0, 0.0],
            [[0.0; 3]; 3],
            PARTICLE_MASS,
        ));
        // Inactive slot passes through.
        fixture.push(WaterParticle::zeroed());
        let n = fixture.len();

        let buf_in = particle_buffer(n);
        let buf_out = particle_buffer(n);
        write_particles(&buf_in, &fixture);
        buf_out.zero_fill();

        // The collider velocity comes off the shared wire too: zero here
        // (the hold frame), so the relative velocity is the particle's own.
        let uniforms = CollideBoxUniforms {
            step_dt: S5_DT,
            cube_half_x: half[0],
            cube_half_y: half[1],
            cube_half_z: half[2],
            basin_min_x: manifold_renderer::node_graph::primitives::BASIN_MIN[0],
            basin_min_y: basin_floor_y,
            basin_min_z: manifold_renderer::node_graph::primitives::BASIN_MIN[2],
            basin_max_x: manifold_renderer::node_graph::primitives::BASIN_MAX[0],
            basin_max_y: manifold_renderer::node_graph::primitives::BASIN_MAX[1],
            basin_max_z: manifold_renderer::node_graph::primitives::BASIN_MAX[2],
            collider_x: centre[0],
            collider_y: centre[1],
            collider_z: centre[2],
            collider_velocity_x: 0.0,
            collider_velocity_y: 0.0,
            collider_velocity_z: 0.0,
            dispatch_count: n as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let mut enc = device().create_encoder("water-collide-box");
        enc.dispatch_compute(
            &collide_pipeline(),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &buf_in,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &buf_out,
                    offset: 0,
                },
            ],
            ceil256(n as u32),
            "node.water_collide_box",
        );
        enc.commit_and_wait_completed();
        let gpu = read_particles(&buf_out, n);

        let h = GRID_SPACING;
        let mut max_penetration = 0.0f32;
        for (i, g) in gpu.iter().enumerate().take(5) {
            let p = &fixture[i];
            let pos = [g.position_mass[0], g.position_mass[1], g.position_mass[2]];
            let v = [
                g.velocity_density[0],
                g.velocity_density[1],
                g.velocity_density[2],
            ];
            let d = [pos[0] - centre[0], pos[1] - centre[1], pos[2] - centre[2]];
            let ad = [d[0].abs(), d[1].abs(), d[2].abs()];
            // Post-projection penetration: how far inside the box the
            // particle remains on its worst axis (0 when on a face).
            let inside = ad[0] < half[0] && ad[1] < half[1] && ad[2] < half[2];
            let penetration = if inside {
                (half[0] - ad[0]).min(half[1] - ad[1]).min(half[2] - ad[2])
            } else {
                0.0
            };
            max_penetration = max_penetration.max(penetration);
            assert!(
                penetration <= 0.1 * h,
                "slot {i} remains inside the box by {penetration} m"
            );

            // CPU mirror of the projection: min-penetration-axis push +
            // relative normal velocity removal.
            let mut exp_pos = [p.position_mass[0], p.position_mass[1], p.position_mass[2]];
            let mut exp_v = [
                p.velocity_density[0],
                p.velocity_density[1],
                p.velocity_density[2],
            ];
            let pd = [
                exp_pos[0] - centre[0],
                exp_pos[1] - centre[1],
                exp_pos[2] - centre[2],
            ];
            let pad = [pd[0].abs(), pd[1].abs(), pd[2].abs()];
            if pad[0] < half[0] && pad[1] < half[1] && pad[2] < half[2] {
                let dx_lo = pd[0] + half[0];
                let dx_hi = half[0] - pd[0];
                let dy_lo = pd[1] + half[1];
                let dy_hi = half[1] - pd[1];
                let dz_lo = pd[2] + half[2];
                let dz_hi = half[2] - pd[2];
                let mut best = dx_lo;
                let mut axis = 0usize;
                let mut sgn = -1.0f32;
                if dx_hi < best {
                    best = dx_hi;
                    axis = 0;
                    sgn = 1.0;
                }
                if dy_lo < best {
                    best = dy_lo;
                    axis = 1;
                    sgn = -1.0;
                }
                if dy_hi < best {
                    best = dy_hi;
                    axis = 1;
                    sgn = 1.0;
                }
                if dz_lo < best {
                    best = dz_lo;
                    axis = 2;
                    sgn = -1.0;
                }
                if dz_hi < best {
                    axis = 2;
                    sgn = 1.0;
                }
                exp_pos[axis] = centre[axis] + sgn * half[axis];
                // Into-surface normal component removed; the collider
                // velocity is zero in this fixture.
                if exp_v[axis] * sgn < 0.0 {
                    exp_v[axis] = 0.0;
                }
                // Free-slip tangential: the other two components unchanged.
            }
            for a in 0..3 {
                assert!(
                    (pos[a] - exp_pos[a]).abs() <= 1.0e-6,
                    "slot {i} axis {a}: projected pos {} != CPU {}",
                    pos[a],
                    exp_pos[a]
                );
                assert!(
                    (v[a] - exp_v[a]).abs() <= 1.0e-6,
                    "slot {i} axis {a}: projected v {} != CPU {}",
                    v[a],
                    exp_v[a]
                );
            }
            // Relative normal velocity at contact is never into the surface.
            if inside {
                let n_axis = {
                    // Find the axis the particle was pushed along: the one
                    // sitting exactly on the face.
                    (0..3)
                        .find(|&a| (ad[a] - half[a]).abs() <= 1.0e-6)
                        .expect("a projected particle sits on a face")
                };
                let sgn = if d[n_axis] > 0.0 { 1.0 } else { -1.0 };
                let vn = v[n_axis] * sgn;
                assert!(
                    vn >= -1.0e-6,
                    "slot {i} still moving into the contact face: {vn}"
                );
            }
        }
        // Basin half: the below-floor particle may not hold a downward
        // velocity (identical rule to mpm_grid_velocity's boundary path).
        let floor = &gpu[5];
        assert!(
            (floor.position_mass[1] - basin_floor_y).abs() <= 1.0e-6,
            "basin penetration was not projected: y={} floor={basin_floor_y}",
            floor.position_mass[1]
        );
        assert!(
            floor.velocity_density[1] >= 0.0,
            "basin floor velocity must be clamped upward: {}",
            floor.velocity_density[1]
        );
        // Inactive pass-through.
        assert_eq!(gpu[6].position_mass, [0.0; 4]);

        println!(
            "water_cube_transform_and_collision_match: accepted collider y = {}, max post-projection penetration {:.3e} m (0.1*h = {:.4}), velocity-rule and basin-rule projections match the CPU mirror",
            accepted.pos[1],
            max_penetration,
            0.1 * h
        );
        assert!(max_penetration <= 0.1 * h);
    }
}
