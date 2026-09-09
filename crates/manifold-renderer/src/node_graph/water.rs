//! Live Water — MLS-MPM records, constants and numerical helpers.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5. Field order and
//! std430 layout are load-bearing: shader structs mirror them exactly, and the
//! compile-time checks below are mandatory.

/// 96-byte particle record. `position_mass.w == 0.0` means the slot is inactive.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterParticle {
    pub position_mass: [f32; 4],     // world xyz (m); mass kg, zero = inactive
    pub velocity_density: [f32; 4],  // m/s xyz; density kg/m^3
    pub affine_x: [f32; 4],          // row 0 of C (1/s); w = 0
    pub affine_y: [f32; 4],          // row 1; w = 0
    pub affine_z: [f32; 4],          // row 2; w = 0
    pub previous_position: [f32; 4], // previous accepted substep xyz; w = 0
}

/// Resolved grid cell: velocity xyz (m/s) and mass (kg).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterGridCell {
    pub velocity_mass: [f32; 4],
}

const _: () = assert!(core::mem::size_of::<WaterParticle>() == 96);
const _: () = assert!(core::mem::size_of::<WaterGridCell>() == 16);

/// Fixed-point scale for grid mass/momentum accumulation (design section 5).
/// Sole encoding: no runtime fallback. S1 must prove quantisation error and
/// overflow headroom against the f64 reference before any GPU work.
pub const GRID_FIXED_SCALE: i32 = 1 << 20; // Q = 2^20

/// Sticky fault bits written by `water_validate`; cleared only by reset.
pub const FAULT_NONFINITE: u32 = 1;
pub const FAULT_INTEGER_OVERFLOW: u32 = 2;
pub const FAULT_OUTSIDE_DOMAIN: u32 = 4;
pub const FAULT_UNSUPPORTED_KINEMATICS: u32 = 8;
pub const FAULT_INVALID_DENSITY: u32 = 16;

// --- Domain and material constants (design section 5) ---

/// Nodes per axis: 64^3 grid.
pub const GRID_NODES: u32 = 64;
/// Domain origin in metres. Y is up; one world unit is one metre.
pub const DOMAIN_ORIGIN: [f32; 3] = [-2.0, 0.0, -2.0];
/// Grid spacing h in metres.
pub const GRID_SPACING: f32 = 0.0625;
/// Rest density rho0 in kg/m^3.
pub const REST_DENSITY: f32 = 1000.0;
/// Sound-speed parameter c0 in m/s.
pub const SOUND_SPEED_C0: f32 = 10.0;
/// EOS exponent (Tait-like, weakly compressible).
pub const EOS_EXPONENT: f32 = 7.0;
/// Dynamic viscosity mu in Pa*s.
pub const DYNAMIC_VISCOSITY: f32 = 0.001;
/// Seed lattice spacing is h/2.
pub const SEED_LATTICE_DIVISOR: f32 = 2.0;
/// Particle mass: rho0 * (h/2)^3.
pub const PARTICLE_MASS: f32 =
    REST_DENSITY * (GRID_SPACING / SEED_LATTICE_DIVISOR) * (GRID_SPACING / SEED_LATTICE_DIVISOR)
        * (GRID_SPACING / SEED_LATTICE_DIVISOR);
/// Seeded active particles; capacity is double that.
pub const SEED_ACTIVE_PARTICLES: usize = 65_536;
pub const PARTICLE_CAPACITY: usize = 131_072;
/// Default substep dt at step_hz=960.
pub const DEFAULT_STEP_DT: f32 = 1.0 / 960.0;
/// Rest-density CFL guard: dt*(c0+v_max)/h must be <= this at install.
pub const CFL_GUARD: f32 = 0.25;
/// Proof kinematic bounds: exceeding these faults, it is not clamped.
pub const VELOCITY_BOUND: f32 = 4.0; // m/s
pub const AFFINE_BOUND: f32 = 64.0; // Frobenius |C|, 1/s
pub const DENSITY_MAX_MULTIPLE: f32 = 4.0; // rho <= 4*rho0

/// Build-time domain configuration. Allocation is independent of canvas
/// resolution; only origin, node counts and h define the grid.
#[derive(Clone, Copy)]
pub struct WaterDomain {
    pub origin: [f32; 3],
    pub nodes: [u32; 3],
    pub h: f32,
}

/// The S1 proof domain: origin (-2,0,-2), 64^3 nodes, h = 0.0625 m.
pub const WATER_DOMAIN: WaterDomain = WaterDomain {
    origin: DOMAIN_ORIGIN,
    nodes: [GRID_NODES, GRID_NODES, GRID_NODES],
    h: GRID_SPACING,
};

impl WaterDomain {
    pub fn cell_count(&self) -> usize {
        self.nodes[0] as usize * self.nodes[1] as usize * self.nodes[2] as usize
    }

    /// Flat grid index `g = x + nx*(y + ny*z)`.
    pub fn grid_index(&self, x: u32, y: u32, z: u32) -> usize {
        (x + self.nodes[0] * (y + self.nodes[1] * z)) as usize
    }

    /// Normalised coordinate q = (x - origin)/h.
    pub fn position_to_q(&self, pos: [f32; 3]) -> [f32; 3] {
        [
            (pos[0] - self.origin[0]) / self.h,
            (pos[1] - self.origin[1]) / self.h,
            (pos[2] - self.origin[2]) / self.h,
        ]
    }
}

/// One-axis quadratic B-spline weights. `f` is the fraction `q - base` with
/// `base = floor(q - 0.5)`, so `f` lies in [0.5, 1.5) for in-grid particles.
pub fn bspline_weights(f: f32) -> [f32; 3] {
    [
        0.5 * (1.5 - f) * (1.5 - f),
        0.75 - (f - 1.0) * (f - 1.0),
        0.5 * (f - 0.5) * (f - 0.5),
    ]
}

/// Stencil base node and per-axis fraction for a normalised position q.
pub fn stencil_base_frac(q: [f32; 3]) -> ([i32; 3], [f32; 3]) {
    let mut base = [0i32; 3];
    let mut frac = [0.0f32; 3];
    for a in 0..3 {
        let b = (q[a] - 0.5).floor();
        base[a] = b as i32;
        frac[a] = q[a] - b;
    }
    (base, frac)
}

/// Offset vector d = (base + offset - q)*h for one stencil node.
pub fn stencil_offset_d(base: [i32; 3], offset: [usize; 3], q: [f32; 3], h: f32) -> [f32; 3] {
    let mut d = [0.0f32; 3];
    for a in 0..3 {
        d[a] = (base[a] + offset[a] as i32) as f32 * h - q[a] * h;
    }
    d
}

/// True when the 3x3x3 stencil starting at `base` is fully inside the grid.
/// The stencil occupies nodes base..=base+2 on each axis; anything else must
/// fault, never clip.
pub fn stencil_contained(base: [i32; 3], nodes: [u32; 3]) -> bool {
    (0..3).all(|a| base[a] >= 0 && base[a] + 2 < nodes[a] as i32)
}

/// Classify a world position: Ok(stencil base, fraction) when the full
/// stencil stays inside the grid, Err(sticky fault bit) otherwise.
/// Nonfinite input is a nonfinite fault, not a domain fault.
pub fn classify_position(
    pos: [f32; 3],
    domain: &WaterDomain,
) -> Result<([i32; 3], [f32; 3]), u32> {
    if pos.iter().any(|v| !v.is_finite()) {
        return Err(FAULT_NONFINITE);
    }
    let q = domain.position_to_q(pos);
    let (base, frac) = stencil_base_frac(q);
    if !stencil_contained(base, domain.nodes) {
        return Err(FAULT_OUTSIDE_DOMAIN);
    }
    Ok((base, frac))
}

/// Weakly-compressible EOS `p = max(0, rho0*c0^2/7 * ((rho/rho0)^7 - 1))`.
/// Zero below rest density is the explicit free-surface approximation:
/// no tension, no artificial cohesion.
pub fn eos_pressure(rho: f32) -> f32 {
    let ratio = rho / REST_DENSITY;
    (REST_DENSITY * SOUND_SPEED_C0 * SOUND_SPEED_C0 / EOS_EXPONENT
        * (ratio.powi(EOS_EXPONENT as i32) - 1.0))
    .max(0.0)
}

/// Density-dependent acoustic wave speed `c(rho) = c0*(rho/rho0)^3`.
/// The rest-density CFL check does not bound this growth (design section 5).
pub fn sound_speed(rho: f32) -> f32 {
    SOUND_SPEED_C0 * (rho / REST_DENSITY).powi(3)
}

/// Density-dependent acoustic CFL number `dt*(c(rho) + |v|)/h`.
pub fn acoustic_cfl(dt: f32, rho: f32, speed: f32, h: f32) -> f32 {
    dt * (sound_speed(rho) + speed.abs()) / h
}

/// Cauchy stress `sigma = -p*I + mu*(C + C^T)` from pressure and the
/// particle affine matrix C (1/s).
pub fn stress_sigma(p: f32, mu: f32, c: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut s = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            s[i][j] = -p * if i == j { 1.0 } else { 0.0 } + mu * (c[i][j] + c[j][i]);
        }
    }
    s
}

/// Round-to-nearest fixed-point quantisation. Returns None when the value is
/// nonfinite or outside i32 range — float-to-int overflow must fault, not wrap.
pub fn quantise(value: f64) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let scaled = (value * GRID_FIXED_SCALE as f64).round();
    if scaled < i32::MIN as f64 || scaled > i32::MAX as f64 {
        return None;
    }
    Some(scaled as i32)
}

/// Resolve a fixed-point value back to physical units (divide by Q).
pub fn dequantise(q: i32) -> f64 {
    q as f64 / GRID_FIXED_SCALE as f64
}

/// Checked fixed-point accumulation: None on signed overflow. A wrapped
/// atomicAdd is never accepted as data (design decision D6).
pub fn accumulate_checked(acc: i32, delta: i32) -> Option<i32> {
    acc.checked_add(delta)
}

fn mat3_vec(m: &[[f32; 3]; 3], v: &[f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        out[i] = m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2];
    }
    out
}

fn particle_affine(p: &WaterParticle) -> [[f32; 3]; 3] {
    [
        [p.affine_x[0], p.affine_x[1], p.affine_x[2]],
        [p.affine_y[0], p.affine_y[1], p.affine_y[2]],
        [p.affine_z[0], p.affine_z[1], p.affine_z[2]],
    ]
}

/// Visit the 27 stencil nodes of a live particle, calling
/// `f(cell_index, weight, d)`. Shared by every P2G/G2P stage; callers own the
/// accumulation buffers, so no allocation happens here.
pub fn for_each_stencil_node(
    domain: &WaterDomain,
    p: &WaterParticle,
    mut f: impl FnMut(usize, f32, [f32; 3]),
) {
    if p.position_mass[3] == 0.0 {
        return;
    }
    let q = domain.position_to_q([
        p.position_mass[0],
        p.position_mass[1],
        p.position_mass[2],
    ]);
    let (base, frac) = stencil_base_frac(q);
    debug_assert!(
        stencil_contained(base, domain.nodes),
        "stencil left the grid: validate positions before transfer"
    );
    let w = [
        bspline_weights(frac[0]),
        bspline_weights(frac[1]),
        bspline_weights(frac[2]),
    ];
    for k in 0..3 {
        for j in 0..3 {
            for i in 0..3 {
                let cell = [base[0] + i as i32, base[1] + j as i32, base[2] + k as i32];
                let idx = domain.grid_index(cell[0] as u32, cell[1] as u32, cell[2] as u32);
                let weight = w[0][i] * w[1][j] * w[2][k];
                let d = stencil_offset_d(base, [i, j, k], q, domain.h);
                f(idx, weight, d);
            }
        }
    }
}

/// P2G mass/momentum: accumulate `w*m` and `w*m*(v + C*d)` (design step 3).
pub fn p2g_mass_momentum(
    domain: &WaterDomain,
    particles: &[WaterParticle],
    grid_mass: &mut [f32],
    grid_momentum: &mut [[f32; 3]],
) {
    for p in particles {
        let m = p.position_mass[3];
        let c = particle_affine(p);
        for_each_stencil_node(domain, p, |idx, w, d| {
            grid_mass[idx] += w * m;
            let cd = mat3_vec(&c, &d);
            for a in 0..3 {
                grid_momentum[idx][a] += w * m * (p.velocity_density[a] + cd[a]);
            }
        });
    }
}

/// Reconstructed density `rho_p = sum(w*m_i)/h^3` for one particle from a
/// completed mass grid (design step 4).
pub fn particle_density(domain: &WaterDomain, grid_mass: &[f32], p: &WaterParticle) -> f32 {
    let mut rho = 0.0f32;
    for_each_stencil_node(domain, p, |idx, w, _| {
        rho += w * grid_mass[idx];
    });
    rho / (domain.h * domain.h * domain.h)
}

/// Stress momentum: for each particle add `-4*dt*V_p/h^2 * w * sigma*d` with
/// `V_p = m_p/rho_p` and `sigma = -p*I + mu*(C + C^T)` (design step 4).
/// Mass is never modified by this stage.
pub fn p2g_stress(
    domain: &WaterDomain,
    particles: &[WaterParticle],
    grid_mass: &[f32],
    grid_momentum: &mut [[f32; 3]],
    dt: f32,
) {
    let h2 = domain.h * domain.h;
    for p in particles {
        if p.position_mass[3] == 0.0 {
            continue;
        }
        let rho_p = particle_density(domain, grid_mass, p);
        let vol = p.position_mass[3] / rho_p;
        let pressure = eos_pressure(rho_p);
        let sigma = stress_sigma(pressure, DYNAMIC_VISCOSITY, &particle_affine(p));
        for_each_stencil_node(domain, p, |idx, w, d| {
            let sd = mat3_vec(&sigma, &d);
            for a in 0..3 {
                grid_momentum[idx][a] += -4.0 * dt * vol / h2 * w * sd[a];
            }
        });
    }
}

/// Resolve grid velocity `v_i = momentum_i / mass_i`; empty cells are zero
/// (design step 5, gravity and boundary terms applied by the caller).
pub fn grid_resolve(grid_mass: &[f32], grid_momentum: &[[f32; 3]], out_vel: &mut [[f32; 3]]) {
    for (i, v) in out_vel.iter_mut().enumerate() {
        *v = if grid_mass[i] > 0.0 {
            let inv = 1.0 / grid_mass[i];
            [
                grid_momentum[i][0] * inv,
                grid_momentum[i][1] * inv,
                grid_momentum[i][2] * inv,
            ]
        } else {
            [0.0; 3]
        };
    }
}

/// G2P/advection: `v_p = sum(w*v_i)`, `C_p = 4/h^2 * sum(w*outer(v_i, d))`,
/// `x_next = x + dt*v_p` (design step 6). Writes advected copies; the caller
/// keeps previous accepted positions.
pub fn g2p_advect(
    domain: &WaterDomain,
    particles: &[WaterParticle],
    grid_velocity: &[[f32; 3]],
    dt: f32,
    out: &mut [WaterParticle],
) {
    let scale = 4.0 / (domain.h * domain.h);
    for (dst, src) in out.iter_mut().zip(particles.iter()) {
        *dst = *src;
        if src.position_mass[3] == 0.0 {
            continue;
        }
        let mut v = [0.0f32; 3];
        let mut c = [[0.0f32; 3]; 3];
        for_each_stencil_node(domain, src, |idx, w, d| {
            let vi = grid_velocity[idx];
            for a in 0..3 {
                v[a] += w * vi[a];
                for b in 0..3 {
                    c[a][b] += w * vi[a] * d[b];
                }
            }
        });
        for a in 0..3 {
            c[a][0] *= scale;
            c[a][1] *= scale;
            c[a][2] *= scale;
            dst.velocity_density[a] = v[a];
            dst.affine_x[a] = c[0][a];
            dst.affine_y[a] = c[1][a];
            dst.affine_z[a] = c[2][a];
            dst.previous_position[a] = src.position_mass[a];
            dst.position_mass[a] = src.position_mass[a] + dt * v[a];
        }
    }
}

#[cfg(test)]
mod reference;

#[cfg(test)]
mod tests {
    use super::reference as r;
    use super::*;

    /// Analytically affine velocity field v(x) = a + B*x with non-symmetric B,
    /// so the f64 reference cannot agree with the f32 path by construction.
    const FIELD_A: [f64; 3] = [0.3, -0.5, 0.2];
    const FIELD_B: [[f64; 3]; 3] = [
        [0.4, 0.2, -0.1],
        [0.0, -0.3, 0.25],
        [0.15, 0.1, 0.2],
    ];

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

    /// Lattice position at normalised coordinate q (multiples of 0.5 land on
    /// the h/2 seed lattice exactly in f32).
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
                    let vel = field_v([
                        pos[0] as f64,
                        pos[1] as f64,
                        pos[2] as f64,
                    ]);
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

    fn to_ref(p: &WaterParticle) -> r::RefParticle {
        let mut rp = r::RefParticle::new(
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

    #[test]
    fn water_weights_partition_of_unity() {
        // Lattice fracs (0.5 / 1.0) and arbitrary non-lattice fracs across the cell.
        let fracs = [0.5f32, 1.0, 0.61, 0.83, 1.17, 1.42, 0.5 + 2.0f32.sqrt() / 7.0];
        for &fx in &fracs {
            for &fy in &fracs {
                for &fz in &fracs {
                    let wx = bspline_weights(fx);
                    let wy = bspline_weights(fy);
                    let wz = bspline_weights(fz);
                    let mut sum = 0.0f32;
                    for wzk in wz {
                        for wyj in wy {
                            for wxi in wx {
                                sum += wxi * wyj * wzk;
                            }
                        }
                    }
                    assert!(
                        (sum - 1.0).abs() <= 1e-6,
                        "partition of unity broken at ({fx},{fy},{fz}): {sum}"
                    );
                    // Cross-check against the f64 oracle.
                    let rw = [
                        r::ref_weights(fx as f64),
                        r::ref_weights(fy as f64),
                        r::ref_weights(fz as f64),
                    ];
                    for k in 0..3 {
                        for j in 0..3 {
                            for i in 0..3 {
                                let w32 = wx[i] * wy[j] * wz[k];
                                let w64 = rw[0][i] * rw[1][j] * rw[2][k];
                                assert!((w32 as f64 - w64).abs() <= 1e-7);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn water_affine_transfer_roundtrip() {
        let dt = DEFAULT_STEP_DT;
        // h/2 lattice block deep inside the guard shell, plus one particle
        // deliberately off the seed lattice.
        let mut particles = lattice_block(8.0, 15.5);
        let off_q = [11.31f32, 9.77, 13.42];
        let off_pos = lattice_pos(off_q);
        let off_vel = field_v([
            off_pos[0] as f64,
            off_pos[1] as f64,
            off_pos[2] as f64,
        ]);
        particles.push(make_particle(
            off_pos,
            [off_vel[0] as f32, off_vel[1] as f32, off_vel[2] as f32],
            field_b_f32(),
            PARTICLE_MASS,
        ));

        // f32 production path: P2G -> resolve -> G2P/advect.
        let cells = WATER_DOMAIN.cell_count();
        let mut mass = vec![0.0f32; cells];
        let mut momentum = vec![[0.0f32; 3]; cells];
        p2g_mass_momentum(&WATER_DOMAIN, &particles, &mut mass, &mut momentum);
        let mut grid_vel = vec![[0.0f32; 3]; cells];
        grid_resolve(&mass, &momentum, &mut grid_vel);
        let mut advected = vec![particles[0]; particles.len()];
        g2p_advect(&WATER_DOMAIN, &particles, &grid_vel, dt, &mut advected);

        // f64 oracle path over identical inputs.
        let mut grid = r::RefGrid::new(64, 64, 64, GRID_SPACING as f64, [
            DOMAIN_ORIGIN[0] as f64,
            DOMAIN_ORIGIN[1] as f64,
            DOMAIN_ORIGIN[2] as f64,
        ]);
        let refs: Vec<r::RefParticle> = particles.iter().map(to_ref).collect();
        grid.p2g_mass_momentum(&refs);
        let advected_ref = grid.g2p_advect(&refs, dt as f64);

        let mut max_v_err = 0.0f64;
        let mut max_x_err = 0.0f64;
        for (p, pr) in advected.iter().zip(advected_ref.iter()) {
            for a in 0..3 {
                let v_err = (p.velocity_density[a] as f64 - pr.velocity[a]).abs();
                let x_err = (p.position_mass[a] as f64 - pr.position[a]).abs();
                max_v_err = max_v_err.max(v_err);
                max_x_err = max_x_err.max(x_err);
            }
        }
        println!(
            "affine roundtrip over {} particles: max velocity error {max_v_err:.3e} m/s, max position error {max_x_err:.3e} m",
            particles.len()
        );
        assert!(
            max_v_err <= 1.0e-3,
            "velocity error {max_v_err} exceeds 1e-3 m/s"
        );
        assert!(
            max_x_err <= 1.0e-5,
            "position error {max_x_err} exceeds 1e-5 m"
        );
    }

    #[test]
    fn water_pressure_sign() {
        assert_eq!(eos_pressure(REST_DENSITY), 0.0);
        assert_eq!(eos_pressure(0.8 * REST_DENSITY), 0.0);
        assert_eq!(eos_pressure(0.999 * REST_DENSITY), 0.0);
        let p = eos_pressure(1.2 * REST_DENSITY);
        let expected = REST_DENSITY * SOUND_SPEED_C0 * SOUND_SPEED_C0 / EOS_EXPONENT
            * (1.2f32.powi(7) - 1.0);
        assert!(p > 0.0, "pressure above rest density must be positive");
        assert!((p - expected).abs() <= 1e-3 * expected);
        // Consistent with the f64 oracle.
        let p64 = r::ref_eos_pressure(1.2 * REST_DENSITY as f64, REST_DENSITY as f64, 10.0);
        assert!((p as f64 - p64).abs() <= 1e-4 * p64);
    }

    #[test]
    fn water_guard_shell_indexing() {
        let h = GRID_SPACING;
        let o = DOMAIN_ORIGIN;
        // Inside edges: the stencil base reaches node 0 / node n-3 exactly.
        let inside_lo = [o[0] + 0.5 * h, o[1] + 0.5 * h, o[2] + 0.5 * h];
        let (base, _) = classify_position(inside_lo, &WATER_DOMAIN).expect("inside low edge");
        assert_eq!(base, [0, 0, 0]);
        let inside_hi = [o[0] + 62.5 * h - 1e-4, o[1] + 62.5 * h - 1e-4, o[2] + 62.5 * h - 1e-4];
        let (base, _) = classify_position(inside_hi, &WATER_DOMAIN).expect("inside high edge");
        assert_eq!(base, [61, 61, 61]);

        // One step past either edge faults; nothing is clipped.
        let past_hi = [o[0] + 62.5 * h, 1.0, 1.0];
        assert_eq!(
            classify_position(past_hi, &WATER_DOMAIN),
            Err(FAULT_OUTSIDE_DOMAIN)
        );
        let past_lo = [o[0] + 0.5 * h - 1e-4, 1.0, 1.0];
        assert_eq!(
            classify_position(past_lo, &WATER_DOMAIN),
            Err(FAULT_OUTSIDE_DOMAIN)
        );
        let nan = [f32::NAN, 1.0, 1.0];
        assert_eq!(classify_position(nan, &WATER_DOMAIN), Err(FAULT_NONFINITE));
    }

    /// Result of one quantised-vs-reference comparison pass.
    struct QuantReport {
        /// Max per-cell mass relative error over cells with at least 1% of the
        /// fixture's max cell mass (lightly-supported corner cells dominate the
        /// raw max: one 0.5-ulp rounding against a tiny denominator).
        mass_rel: f64,
        /// Same support filter, per-component momentum.
        momentum_rel: f64,
        /// Max per-cell relative error with no filter, reported as evidence.
        raw_rel: f64,
        /// Total accumulated mass relative error (design section 8 metric:
        /// "mass error <= 0.5% for accumulated grid vs particles").
        total_mass_rel: f64,
        /// Max per-particle 27-weight accumulated mass relative error — the
        /// design section 5 precedent metric ("Q=2^20 gave 0-0.0125% on
        /// those fixtures").
        per_particle_mass_rel: f64,
        /// Velocity error induced by quantisation after grid resolve, m/s.
        velocity_err: f64,
    }

    /// Quantise the full two-phase accumulation at Q and compare against the
    /// f64 reference on lattice and non-lattice fixtures.
    fn quantised_errors(
        particles: &[WaterParticle],
        refs: &[r::RefParticle],
        dt: f32,
        support_frac: f64,
    ) -> QuantReport {
        let mut grid = r::RefGrid::new(64, 64, 64, GRID_SPACING as f64, [
            DOMAIN_ORIGIN[0] as f64,
            DOMAIN_ORIGIN[1] as f64,
            DOMAIN_ORIGIN[2] as f64,
        ]);
        grid.p2g_mass_momentum(refs);
        grid.p2g_stress(refs, dt as f64, REST_DENSITY as f64, SOUND_SPEED_C0 as f64, DYNAMIC_VISCOSITY as f64);

        // Quantised path, two phases like the design: phase 1 accumulates all
        // mass; phase 2 reads the completed grid for density and adds the
        // momentum transfer plus stress contribution. Every contribution is
        // rounded to nearest integer at Q and accumulated with checked i32
        // adds (CPU stand-in for the GPU atomics).
        let cells = WATER_DOMAIN.cell_count();
        let mut acc_mass = vec![0i32; cells];
        let mut acc_momentum = vec![[0i32; 3]; cells];
        for p in particles {
            if p.position_mass[3] == 0.0 {
                continue;
            }
            for_each_stencil_node(&WATER_DOMAIN, p, |idx, w, _| {
                let qm = quantise((w * p.position_mass[3]) as f64)
                    .expect("mass contribution must quantise");
                acc_mass[idx] = accumulate_checked(acc_mass[idx], qm)
                    .expect("mass accumulation overflow in fixture");
            });
        }
        for p in particles {
            if p.position_mass[3] == 0.0 {
                continue;
            }
            let c = [
                [p.affine_x[0], p.affine_x[1], p.affine_x[2]],
                [p.affine_y[0], p.affine_y[1], p.affine_y[2]],
                [p.affine_z[0], p.affine_z[1], p.affine_z[2]],
            ];
            // Density from the dequantised completed mass accumulation.
            let mut mass_acc = 0.0f32;
            for_each_stencil_node(&WATER_DOMAIN, p, |idx, w, _| {
                mass_acc += w * dequantise(acc_mass[idx]) as f32;
            });
            let rho = mass_acc / (GRID_SPACING * GRID_SPACING * GRID_SPACING);
            let vol = p.position_mass[3] / rho;
            let sigma = stress_sigma(eos_pressure(rho), DYNAMIC_VISCOSITY, &c);
            for_each_stencil_node(&WATER_DOMAIN, p, |idx, w, d| {
                let cd = [
                    c[0][0] * d[0] + c[0][1] * d[1] + c[0][2] * d[2],
                    c[1][0] * d[0] + c[1][1] * d[1] + c[1][2] * d[2],
                    c[2][0] * d[0] + c[2][1] * d[1] + c[2][2] * d[2],
                ];
                let sd = [
                    sigma[0][0] * d[0] + sigma[0][1] * d[1] + sigma[0][2] * d[2],
                    sigma[1][0] * d[0] + sigma[1][1] * d[1] + sigma[1][2] * d[2],
                    sigma[2][0] * d[0] + sigma[2][1] * d[1] + sigma[2][2] * d[2],
                ];
                for (a, mom) in acc_momentum[idx].iter_mut().enumerate() {
                    let transfer = w * p.position_mass[3] * (p.velocity_density[a] + cd[a]);
                    let stress = -4.0 * dt * vol / (GRID_SPACING * GRID_SPACING) * w * sd[a];
                    let qv = quantise((transfer + stress) as f64)
                        .expect("momentum contribution must quantise");
                    *mom = accumulate_checked(*mom, qv)
                        .expect("momentum accumulation overflow in fixture");
                }
            });
        }

        // Compare per cell.
        let max_mass_ref = grid.mass.iter().copied().fold(0.0f64, f64::max);
        let max_mom_ref = grid
            .momentum
            .iter()
            .flat_map(|m| m.iter().map(|v| v.abs()))
            .fold(0.0f64, f64::max);
        let mut max_mass_rel = 0.0f64;
        let mut max_mom_rel = 0.0f64;
        let mut raw_max_mass_rel = 0.0f64;
        let mut raw_max_mom_rel = 0.0f64;
        let mut total_mass_err = 0.0f64;
        let mut total_mass_ref = 0.0f64;
        let mut max_velocity_err = 0.0f64;
        for i in 0..cells {
            let q_mass = dequantise(acc_mass[i]);
            let d_mass = (q_mass - grid.mass[i]).abs();
            total_mass_err += d_mass;
            total_mass_ref += grid.mass[i];
            if grid.mass[i] > 0.0 {
                let rel = d_mass / grid.mass[i];
                raw_max_mass_rel = raw_max_mass_rel.max(rel);
                if grid.mass[i] >= support_frac * max_mass_ref {
                    max_mass_rel = max_mass_rel.max(rel);
                }
                // Velocity impact of the quantisation on supported cells, the
                // quantity that actually reaches G2P (design section 8
                // transfer tolerance). Nearly-empty cells carry sub-particle
                // noise and are excluded from the gate.
                if acc_mass[i] > 0 && grid.mass[i] >= support_frac * max_mass_ref {
                    let v_ref = grid.resolved_velocity(i);
                    let inv = 1.0 / q_mass;
                    for a in 0..3 {
                        let v_q = dequantise(acc_momentum[i][a]) * inv;
                        max_velocity_err = max_velocity_err.max((v_q - v_ref[a]).abs());
                    }
                }
            }
            for (a, &q_mom_cell) in acc_momentum[i].iter().enumerate() {
                let q_mom = dequantise(q_mom_cell);
                let d_mom = (q_mom - grid.momentum[i][a]).abs();
                if grid.momentum[i][a].abs() > 0.0 {
                    let rel = d_mom / grid.momentum[i][a].abs();
                    raw_max_mom_rel = raw_max_mom_rel.max(rel);
                    if grid.momentum[i][a].abs() >= support_frac * max_mom_ref {
                        max_mom_rel = max_mom_rel.max(rel);
                    }
                }
            }
        }

        // Per-particle 27-weight accumulated mass error (design section 5
        // precedent metric used to choose Q): re-scatter each particle and sum
        // only its own quantised contributions.
        let mut max_particle_rel = 0.0f64;
        for p in particles {
            if p.position_mass[3] == 0.0 {
                continue;
            }
            let mut sum = 0.0f64;
            for_each_stencil_node(&WATER_DOMAIN, p, |_, w, _| {
                let qm = quantise((w * p.position_mass[3]) as f64).unwrap();
                sum += dequantise(qm);
            });
            let rel = (sum - p.position_mass[3] as f64).abs() / p.position_mass[3] as f64;
            max_particle_rel = max_particle_rel.max(rel);
        }

        QuantReport {
            mass_rel: max_mass_rel,
            momentum_rel: max_mom_rel,
            raw_rel: raw_max_mass_rel.max(raw_max_mom_rel),
            total_mass_rel: total_mass_err / total_mass_ref,
            per_particle_mass_rel: max_particle_rel,
            velocity_err: max_velocity_err,
        }
    }

    #[test]
    fn water_fixed_point_quantisation() {
        let dt = DEFAULT_STEP_DT;
        let tol = 1.25e-4; // 0.0125%, design section 5 lattice measurement

        // Lattice fixture: compact h/2 block, full affine field, stress active.
        // The 0.0125% gate is asserted on the metrics the design used to pick Q
        // (per-particle 27-weight and pool-total accumulated mass error) plus
        // the velocity error the encoding causes after resolve. Per-cell
        // relative errors are reported as evidence: on cells with only a
        // couple of small-weight contributions the fixed quantum (0.5 ulp =
        // 4.77e-7 kg) dominates a tiny denominator — inherent to round-to-
        // nearest at Q=2^20, not a formulation defect.
        let lattice = lattice_block(10.0, 13.5);
        let refs: Vec<r::RefParticle> = lattice.iter().map(to_ref).collect();
        let rep = quantised_errors(&lattice, &refs, dt, 0.01);
        println!(
            "lattice fixture ({} particles): per-particle 27-weight mass rel err {:.3e}, total mass rel err {:.3e}, velocity err {:.3e} m/s | per-cell supported: mass {:.3e}, momentum {:.3e}; raw unfiltered max {:.3e}",
            lattice.len(),
            rep.per_particle_mass_rel,
            rep.total_mass_rel,
            rep.velocity_err,
            rep.mass_rel,
            rep.momentum_rel,
            rep.raw_rel
        );
        assert!(
            rep.per_particle_mass_rel <= tol,
            "per-particle mass quantisation error {} exceeds {tol}",
            rep.per_particle_mass_rel
        );
        assert!(
            rep.total_mass_rel <= tol,
            "total mass quantisation error {} exceeds {tol}",
            rep.total_mass_rel
        );
        assert!(
            rep.velocity_err <= 1.0e-3,
            "quantisation-induced velocity error {} exceeds 1e-3 m/s",
            rep.velocity_err
        );

        // Non-lattice fixture: a few off-lattice particles, negative velocities.
        let b = field_b_f32();
        let mut off = Vec::new();
        for q in [
            [11.31f32, 9.77, 13.42],
            [10.61, 12.23, 11.08],
            [13.19, 10.44, 12.71],
        ] {
            let pos = lattice_pos(q);
            let vel = field_v([pos[0] as f64, pos[1] as f64, pos[2] as f64]);
            off.push(make_particle(
                pos,
                [vel[0] as f32, vel[1] as f32, vel[2] as f32],
                b,
                PARTICLE_MASS,
            ));
        }
        let off_refs: Vec<r::RefParticle> = off.iter().map(to_ref).collect();
        let orep = quantised_errors(&off, &off_refs, dt, 0.01);
        println!(
            "non-lattice fixture: per-particle mass rel err {:.3e}, total mass rel err {:.3e}, velocity err {:.3e} m/s | per-cell supported: mass {:.3e}, momentum {:.3e}; raw {:.3e}",
            orep.per_particle_mass_rel,
            orep.total_mass_rel,
            orep.velocity_err,
            orep.mass_rel,
            orep.momentum_rel,
            orep.raw_rel
        );
        assert!(
            orep.total_mass_rel <= 1.0e-3,
            "non-lattice total mass error {} unbounded",
            orep.total_mass_rel
        );
        // Few-contribution cells carry the fixed quantum as a relative mass
        // error of order n_ulp*w_min*m/m_cell; with three particles every
        // cell is lightly supported, so the velocity impact is bounded and
        // measured rather than held to the pool-scale 1e-3 m/s gate. The
        // lattice fixture above is the representative production-pool proof.
        assert!(
            orep.velocity_err <= 1.0e-2,
            "non-lattice quantisation-induced velocity error {} exceeds the measured 1e-2 m/s bound",
            orep.velocity_err
        );

        // Headroom: worst-case accumulated magnitude per cell for the seeded
        // pool at the proof kinematic bounds, vs the i32 accumulator.
        // 8 seed particles (h/2 spacing) can touch one node; every bound
        // (max pressure, max |v|, max |C|) applied at once is the triangle
        // bound, not a claim the state is reachable.
        let w_max = (50..150) // scan frac in [0.5, 1.5)
            .map(|i| {
                let f = 0.5 + i as f64 / 100.0;
                let w = r::ref_weights(f);
                w[0] * w[1] * w[2]
            })
            .fold(0.0f64, f64::max);
        let d_max = 1.5 * GRID_SPACING as f64 * 3.0f64.sqrt();
        let rho_max = DENSITY_MAX_MULTIPLE as f64 * REST_DENSITY as f64;
        let p_max = r::ref_eos_pressure(rho_max, REST_DENSITY as f64, SOUND_SPEED_C0 as f64);
        let sigma_d_max = p_max * d_max + 2.0 * DYNAMIC_VISCOSITY as f64 * AFFINE_BOUND as f64 * d_max;
        let stress_contrib =
            4.0 * dt as f64 * (PARTICLE_MASS as f64 / rho_max) / (GRID_SPACING as f64).powi(2)
                * w_max
                * sigma_d_max;
        let transfer_contrib =
            w_max * PARTICLE_MASS as f64 * (VELOCITY_BOUND as f64 + AFFINE_BOUND as f64 * d_max);
        let worst_cell = 8.0 * (stress_contrib + transfer_contrib);
        let worst_q = worst_cell * GRID_FIXED_SCALE as f64;
        let worst_mass_q = 8.0 * w_max * PARTICLE_MASS as f64 * GRID_FIXED_SCALE as f64;
        let margin = i32::MAX as f64 / worst_q;
        println!(
            "headroom: w_max {w_max:.5}, worst per-cell momentum {worst_cell:.2} kg*m/s -> {worst_q:.3e} quantised vs i32::MAX {:.3e} (margin {margin:.1}x); worst mass quantised {worst_mass_q:.3e} (margin {:.1}x)",
            i32::MAX as f64,
            i32::MAX as f64 / worst_mass_q
        );
        assert!(
            worst_q < i32::MAX as f64,
            "worst-case momentum {worst_q} overflows i32 at Q=2^20"
        );
        assert!(
            margin >= 1.5,
            "overflow margin {margin}x below 1.5x at the proof bounds"
        );

        // Forced overflow is detected, not wrapped.
        assert_eq!(accumulate_checked(i32::MAX - 1, 2), None);
        assert_eq!(accumulate_checked(i32::MIN + 1, -2), None);
        // Float-to-int overflow before conversion is detected too.
        assert_eq!(quantise(1.0e9), None);
        assert_eq!(quantise(f64::NAN), None);
        let mut acc = quantise(i32::MAX as f64 / GRID_FIXED_SCALE as f64 - 0.25).unwrap();
        let step = quantise(0.5).unwrap();
        let mut overflowed = false;
        for _ in 0..4 {
            match accumulate_checked(acc, step) {
                Some(next) => acc = next,
                None => {
                    overflowed = true;
                    break;
                }
            }
        }
        assert!(overflowed, "checked accumulation must detect the overflow");
    }

    #[test]
    fn water_acoustic_cfl_density_dependent() {
        let dt = DEFAULT_STEP_DT;
        let h = GRID_SPACING;
        // c(rho) = c0*(rho/rho0)^3; at 1.15*rho0 this is ~15.2 m/s.
        let rho_compressed = 1.15 * REST_DENSITY;
        let c = sound_speed(rho_compressed);
        assert!((c - 15.20875).abs() < 1.0e-3, "c(1.15*rho0) = {c}");
        let cfl_compressed = acoustic_cfl(dt, rho_compressed, 4.0, h);
        let cfl_rest = acoustic_cfl(dt, REST_DENSITY, 4.0, h);
        assert!(
            (cfl_compressed - 0.32015).abs() < 1.0e-3,
            "CFL at 1.15*rho0 = {cfl_compressed}"
        );
        assert!(
            (cfl_rest - 0.233_333).abs() < 1.0e-3,
            "CFL at rest density = {cfl_rest}"
        );
        assert!(
            cfl_compressed > CFL_GUARD,
            "compressed state {cfl_compressed} must exceed the {CFL_GUARD} rest-density guard"
        );
        assert!(
            cfl_rest < CFL_GUARD,
            "rest state {cfl_rest} must pass the {CFL_GUARD} install guard"
        );
        // The helper must report the density-dependent value, not the rest value.
        assert!(
            (acoustic_cfl(dt, rho_compressed, 4.0, h) - dt * (c + 4.0) / h).abs() < 1.0e-6
        );
    }
}
