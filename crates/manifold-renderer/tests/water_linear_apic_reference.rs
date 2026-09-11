//! Independent f64 implementation of Jiang et al. 2015, section 6, Eq.12–14.
//! Trilinear MAC transfers; no PIC blend, filtering force, or EOS pressure.
//! RK3 advection follows FLIP Fluids 70a0e954's _RK3 (mathematical formula).
type FaceSample = (usize, f64, [f64; 3], [f64; 3]);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Particle {
    pub p: [f64; 3],
    pub v: [f64; 3],
    pub c: [[f64; 3]; 3],
    pub m: f64,
}
#[derive(Clone)]
pub(crate) struct Grid {
    pub n: [usize; 3],
    pub h: f64,
    pub origin: [f64; 3],
    pub velocity: [Vec<f64>; 3],
    pub weight: [Vec<f64>; 3],
}
impl Grid {
    pub fn new(n: [usize; 3], h: f64, origin: [f64; 3]) -> Self {
        assert!(
            n.iter().all(|&d| d >= 2)
                && h.is_finite()
                && h > 0.
                && origin.iter().all(|v| v.is_finite())
        );
        let sizes = std::array::from_fn::<_, 3, _>(|a| {
            let mut d = n;
            d[a] += 1;
            d.iter().product()
        });
        Self {
            n,
            h,
            origin,
            velocity: std::array::from_fn(|a| vec![0.; sizes[a]]),
            weight: std::array::from_fn(|a| vec![0.; sizes[a]]),
        }
    }
    pub fn dims(&self, a: usize) -> [usize; 3] {
        let mut n = self.n;
        n[a] += 1;
        n
    }
    pub fn index(&self, a: usize, c: [usize; 3]) -> usize {
        let d = self.dims(a);
        (c[2] * d[1] + c[1]) * d[0] + c[0]
    }
    pub fn position(&self, a: usize, c: [usize; 3]) -> [f64; 3] {
        std::array::from_fn(|d| {
            self.origin[d] + self.h * (c[d] as f64 + if a == d { 0. } else { 0.5 })
        })
    }
    fn stencil(&self, a: usize, p: [f64; 3]) -> Result<[FaceSample; 8], &'static str> {
        let mut b = [0; 3];
        let mut t = [0.; 3];
        let dims = self.dims(a);
        for d in 0..3 {
            let q = (p[d] - self.origin[d]) / self.h - if a == d { 0. } else { 0.5 };
            if !q.is_finite() || q < 0. || q.floor() + 1. >= dims[d] as f64 {
                return Err("incomplete APIC stencil");
            }
            b[d] = q.floor() as usize;
            t[d] = q - b[d] as f64;
        }
        Ok(std::array::from_fn(|i| {
            let bit = [i & 1, (i >> 1) & 1, (i >> 2) & 1];
            let c = std::array::from_fn(|d| b[d] + bit[d]);
            let w: [f64; 3] = std::array::from_fn(|d| if bit[d] == 0 { 1. - t[d] } else { t[d] });
            let grad = std::array::from_fn(|d| {
                let sign = if bit[d] == 0 { -1. } else { 1. };
                sign / self.h * w[(d + 1) % 3] * w[(d + 2) % 3]
            });
            (
                self.index(a, c),
                w.iter().product(),
                grad,
                self.position(a, c),
            )
        }))
    }
    pub fn transfer(&mut self, ps: &[Particle]) -> Result<(), &'static str> {
        // Prevalidate so a bad particle does not partially replace the grid.
        for p in ps {
            if !p.m.is_finite()
                || p.m <= 0.
                || p.v
                    .iter()
                    .chain(p.c.iter().flatten())
                    .any(|x| !x.is_finite())
            {
                return Err("invalid particle");
            }
            for a in 0..3 {
                self.stencil(a, p.p)?;
            }
        }
        for a in 0..3 {
            self.velocity[a].fill(0.);
            self.weight[a].fill(0.);
        }
        for p in ps {
            for a in 0..3 {
                for (i, w, _, f) in self.stencil(a, p.p)? {
                    let affine = (0..3).map(|d| p.c[a][d] * (f[d] - p.p[d])).sum::<f64>();
                    self.velocity[a][i] += p.m * w * (p.v[a] + affine);
                    self.weight[a][i] += p.m * w;
                }
            }
        }
        for a in 0..3 {
            for i in 0..self.velocity[a].len() {
                if self.weight[a][i] > 0. {
                    self.velocity[a][i] /= self.weight[a][i];
                }
            }
        }
        Ok(())
    }
    pub fn gather(&self, p: [f64; 3]) -> Result<([f64; 3], [[f64; 3]; 3]), &'static str> {
        let mut v = [0.; 3];
        let mut c = [[0.; 3]; 3];
        for a in 0..3 {
            for (i, w, grad, _) in self.stencil(a, p)? {
                let u = self.velocity[a][i];
                if !u.is_finite() {
                    return Err("invalid grid velocity");
                }
                v[a] += w * u;
                for (d, &g) in grad.iter().enumerate() {
                    c[a][d] += g * u;
                }
            }
        }
        Ok((v, c))
    }
    pub fn sample(&self, p: [f64; 3]) -> Result<[f64; 3], &'static str> {
        Ok(self.gather(p)?.0)
    }
    pub fn advect(&self, p: [f64; 3], dt: f64) -> Result<[f64; 3], &'static str> {
        if !dt.is_finite() || dt < 0. {
            return Err("invalid dt");
        }
        let k1 = self.sample(p)?;
        let k2 = self.sample(std::array::from_fn(|a| p[a] + 0.5 * dt * k1[a]))?;
        let k3 = self.sample(std::array::from_fn(|a| p[a] + 0.75 * dt * k2[a]))?;
        Ok(std::array::from_fn(|a| {
            p[a] + dt * (2. * k1[a] + 3. * k2[a] + 4. * k3[a]) / 9.
        }))
    }
    // Six-neighbour layer extrapolation, as in FLIP Fluids GridUtils. The
    // caller supplies validity after pressure; mass is not pressure validity.
    pub fn extrapolate(&mut self, valid: &mut [Vec<bool>; 3], layers: usize) {
        for _ in 0..layers {
            let mut changed = false;
            for (a, va) in valid.iter_mut().enumerate() {
                let old = va.clone();
                let old_u = self.velocity[a].clone();
                let d = self.dims(a);
                for z in 0..d[2] {
                    for y in 0..d[1] {
                        for x in 0..d[0] {
                            let c = [x, y, z];
                            let i = self.index(a, c);
                            if old[i] {
                                continue;
                            }
                            let mut sum = 0.;
                            let mut count = 0;
                            for axis in 0..3 {
                                for plus in [false, true] {
                                    let mut q = c;
                                    if plus {
                                        if q[axis] + 1 == d[axis] {
                                            continue;
                                        }
                                        q[axis] += 1;
                                    } else {
                                        if q[axis] == 0 {
                                            continue;
                                        }
                                        q[axis] -= 1;
                                    }
                                    let j = self.index(a, q);
                                    if old[j] {
                                        sum += old_u[j];
                                        count += 1;
                                    }
                                }
                            }
                            if count > 0 {
                                self.velocity[a][i] = sum / count as f64;
                                va[i] = true;
                                changed = true;
                            }
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_locations_and_weight_identities() {
        let g = Grid::new([8; 3], 0.25, [1., 2., 3.]);
        assert_eq!(g.position(0, [2, 2, 2]), [1.5, 2.625, 3.625]);
        assert_eq!(g.position(1, [2, 2, 2]), [1.625, 2.5, 3.625]);
        for a in 0..3 {
            let s = g.stencil(a, [1.71, 2.89, 3.73]).unwrap();
            assert!((s.iter().map(|x| x.1).sum::<f64>() - 1.).abs() < 1e-12);
            for d in 0..3 {
                assert!(s.iter().map(|x| x.2[d]).sum::<f64>().abs() < 1e-12);
            }
        }
    }
    #[test]
    fn affine_gather_and_scatter_momentum() {
        let mut g = Grid::new([8; 3], 0.25, [0.; 3]);
        let p = Particle {
            p: [0.71, 0.83, 0.92],
            v: [1.2, -0.7, 0.3],
            c: [[0.2, 0.3, -0.1], [-0.4, 0.7, 0.2], [0.1, -0.3, 0.4]],
            m: 2.,
        };
        g.transfer(&[p]).unwrap();
        for a in 0..3 {
            let momentum = g.velocity[a]
                .iter()
                .zip(&g.weight[a])
                .map(|(u, m)| u * m)
                .sum::<f64>();
            assert!((momentum - p.m * p.v[a]).abs() < 1e-12);
            assert!((g.weight[a].iter().sum::<f64>() - p.m).abs() < 1e-12);
        }
        let (v, c) = g.gather(p.p).unwrap();
        for a in 0..3 {
            assert!((v[a] - p.v[a]).abs() < 1e-12);
            for (actual, expected) in c[a].iter().zip(p.c[a]) {
                assert!((actual - expected).abs() < 1e-12);
            }
        }
    }
    #[test]
    fn rk3_constant_field_and_invalid_stencil() {
        let mut g = Grid::new([8; 3], 0.25, [0.; 3]);
        let v = [0.3, -0.2, 0.1];
        for (a, &u) in v.iter().enumerate() {
            g.velocity[a].fill(u);
        }
        let p = [0.6, 0.7, 0.8];
        let q = g.advect(p, 0.1).unwrap();
        for a in 0..3 {
            assert!((q[a] - p[a] - 0.1 * v[a]).abs() < 1e-12);
        }
        assert!(g.gather([0.; 3]).is_err());
    }
    #[test]
    fn extrapolation_preserves_known_faces() {
        let mut g = Grid::new([4; 3], 1., [0.; 3]);
        let mut valid = std::array::from_fn(|a| vec![false; g.velocity[a].len()]);
        for (a, va) in valid.iter_mut().enumerate() {
            let i = g.index(a, [1; 3]);
            va[i] = true;
            g.velocity[a][i] = 2.;
        }
        g.extrapolate(&mut valid, 1);
        for (a, va) in valid.iter().enumerate() {
            assert_eq!(va.iter().filter(|v| **v).count(), 7);
            for (i, &ok) in va.iter().enumerate() {
                if ok {
                    assert_eq!(g.velocity[a][i], 2.);
                }
            }
        }
    }
}
