//! f64 test-only oracle for weights, P2G, stress and G2P (S1). Production math
//! stays specified by the design; this module never ships.

/// 3x3 matrix in row-major order.
pub type RefMat3 = [[f64; 3]; 3];

/// f64 counterpart of `WaterParticle`: world-space position (m), velocity (m/s),
/// mass (kg) and affine matrix C (1/s).
#[derive(Clone, Copy)]
pub struct RefParticle {
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub mass: f64,
    pub c: RefMat3,
}

impl RefParticle {
    pub fn new(position: [f64; 3], velocity: [f64; 3], mass: f64) -> Self {
        Self {
            position,
            velocity,
            mass,
            c: [[0.0; 3]; 3],
        }
    }
}

/// One-axis quadratic B-spline weights for fraction `f = q - base`,
/// `base = floor(q - 0.5)`, so `f` lies in [0.5, 1.5).
pub fn ref_weights(f: f64) -> [f64; 3] {
    [
        0.5 * (1.5 - f) * (1.5 - f),
        0.75 - (f - 1.0) * (f - 1.0),
        0.5 * (f - 0.5) * (f - 0.5),
    ]
}

/// Stencil base (per axis) and fraction for a particle at normalised coordinate q.
pub fn ref_stencil(q: [f64; 3]) -> ([i32; 3], [f64; 3]) {
    let mut base = [0i32; 3];
    let mut frac = [0.0f64; 3];
    for a in 0..3 {
        let b = (q[a] - 0.5).floor();
        base[a] = b as i32;
        frac[a] = q[a] - b;
    }
    (base, frac)
}

/// Flat grid index `g = x + nx * (y + ny * z)`.
pub fn ref_grid_index(x: i32, y: i32, z: i32, nx: i32, ny: i32) -> usize {
    (x + nx * (y + ny * z)) as usize
}

/// Weakly-compressible EOS `p = max(0, rho0*c0^2/7 * ((rho/rho0)^7 - 1))`.
/// Zero below rest density is the explicit free-surface approximation.
pub fn ref_eos_pressure(rho: f64, rho0: f64, c0: f64) -> f64 {
    (rho0 * c0 * c0 / 7.0 * ((rho / rho0).powi(7) - 1.0)).max(0.0)
}

/// Cauchy stress `sigma = -p*I + mu*(C + C^T)`.
pub fn ref_stress(p: f64, mu: f64, c: &RefMat3) -> RefMat3 {
    let mut s = [[0.0f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            s[i][j] = -p * if i == j { 1.0 } else { 0.0 } + mu * (c[i][j] + c[j][i]);
        }
    }
    s
}

pub fn ref_mat3_vec(m: &RefMat3, v: &[f64; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    for i in 0..3 {
        out[i] = m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2];
    }
    out
}

/// f64 MPM accumulation grid over the whole domain.
pub struct RefGrid {
    pub nx: usize,
    pub ny: usize,
    pub nz: usize,
    pub h: f64,
    pub origin: [f64; 3],
    pub mass: Vec<f64>,
    pub momentum: Vec<[f64; 3]>,
}

impl RefGrid {
    pub fn new(nx: usize, ny: usize, nz: usize, h: f64, origin: [f64; 3]) -> Self {
        let cells = nx * ny * nz;
        Self {
            nx,
            ny,
            nz,
            h,
            origin,
            mass: vec![0.0; cells],
            momentum: vec![[0.0; 3]; cells],
        }
    }

    /// All 27 (cell index, weight, offset d) stencil entries for a position.
    pub fn stencil_entries(&self, pos: [f64; 3]) -> Vec<(usize, f64, [f64; 3])> {
        let q = [
            (pos[0] - self.origin[0]) / self.h,
            (pos[1] - self.origin[1]) / self.h,
            (pos[2] - self.origin[2]) / self.h,
        ];
        let (base, frac) = ref_stencil(q);
        let w = [
            ref_weights(frac[0]),
            ref_weights(frac[1]),
            ref_weights(frac[2]),
        ];
        let mut out = Vec::with_capacity(27);
        for k in 0..3 {
            for j in 0..3 {
                for i in 0..3 {
                    let cell = [
                        base[0] + i as i32,
                        base[1] + j as i32,
                        base[2] + k as i32,
                    ];
                    if cell.iter().any(|&c| c < 0) {
                        continue;
                    }
                    if cell[0] as usize >= self.nx
                        || cell[1] as usize >= self.ny
                        || cell[2] as usize >= self.nz
                    {
                        continue;
                    }
                    let idx = ref_grid_index(
                        cell[0],
                        cell[1],
                        cell[2],
                        self.nx as i32,
                        self.ny as i32,
                    );
                    let weight = w[0][i] * w[1][j] * w[2][k];
                    let d = [
                        (cell[0] as f64 - q[0]) * self.h,
                        (cell[1] as f64 - q[1]) * self.h,
                        (cell[2] as f64 - q[2]) * self.h,
                    ];
                    out.push((idx, weight, d));
                }
            }
        }
        out
    }

    /// P2G: accumulate `w*m` and `w*m*(v + C*d)` for every live particle.
    pub fn p2g_mass_momentum(&mut self, particles: &[RefParticle]) {
        for p in particles {
            if p.mass == 0.0 {
                continue;
            }
            for (idx, w, d) in self.stencil_entries(p.position) {
                self.mass[idx] += w * p.mass;
                let cd = ref_mat3_vec(&p.c, &d);
                for (a, (mom, cd_a)) in self.momentum[idx].iter_mut().zip(cd).enumerate() {
                    *mom += w * p.mass * (p.velocity[a] + cd_a);
                }
            }
        }
    }

    /// Reconstructed density `rho_p = sum(w*m_i)/h^3` for one particle.
    pub fn particle_density(&self, p: &RefParticle) -> f64 {
        let mut rho = 0.0;
        for (idx, w, _) in self.stencil_entries(p.position) {
            rho += w * self.mass[idx];
        }
        rho / (self.h * self.h * self.h)
    }

    /// Stress momentum: `rho_p` from the completed grid, then per particle add
    /// `-4*dt*V_p/h^2 * w * sigma*d` with `V_p = m_p/rho_p`.
    pub fn p2g_stress(&mut self, particles: &[RefParticle], dt: f64, rho0: f64, c0: f64, mu: f64) {
        for p in particles {
            if p.mass == 0.0 {
                continue;
            }
            let rho_p = self.particle_density(p);
            let vol = p.mass / rho_p;
            let pressure = ref_eos_pressure(rho_p, rho0, c0);
            let sigma = ref_stress(pressure, mu, &p.c);
            for (idx, w, d) in self.stencil_entries(p.position) {
                let sd = ref_mat3_vec(&sigma, &d);
                for (mom, sd_a) in self.momentum[idx].iter_mut().zip(sd) {
                    *mom += -4.0 * dt * vol / (self.h * self.h) * w * sd_a;
                }
            }
        }
    }

    /// Resolved grid velocity `v_i = momentum_i / mass_i` (zero for empty cells).
    pub fn resolved_velocity(&self, idx: usize) -> [f64; 3] {
        if self.mass[idx] > 0.0 {
            let inv = 1.0 / self.mass[idx];
            [
                self.momentum[idx][0] * inv,
                self.momentum[idx][1] * inv,
                self.momentum[idx][2] * inv,
            ]
        } else {
            [0.0; 3]
        }
    }

    /// G2P: `v_p = sum(w*v_i)`, `C_p = 4/h^2 * sum(w*outer(v_i, d))`,
    /// `x_next = x + dt*v_p`. Returns advected copies with updated C.
    pub fn g2p_advect(&self, particles: &[RefParticle], dt: f64) -> Vec<RefParticle> {
        let mut out = particles.to_vec();
        for p in &mut out {
            if p.mass == 0.0 {
                continue;
            }
            let mut v = [0.0f64; 3];
            let mut c = [[0.0f64; 3]; 3];
            for (idx, w, d) in self.stencil_entries(p.position) {
                let vi = self.resolved_velocity(idx);
                for a in 0..3 {
                    v[a] += w * vi[a];
                    for (cb, db) in c[a].iter_mut().zip(d) {
                        *cb += w * vi[a] * db;
                    }
                }
            }
            let scale = 4.0 / (self.h * self.h);
            for a in 0..3 {
                for cb in c[a].iter_mut() {
                    *cb *= scale;
                }
                p.position[a] += dt * v[a];
            }
            p.velocity = v;
            p.c = c;
        }
        out
    }
}
