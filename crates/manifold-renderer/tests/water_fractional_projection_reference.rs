//! Independent f64 oracle: APIC paper section 6's MAC projection, with the
//! ghost-fluid / fractional-solid treatment used by FLIP Fluids engine
//! 70a0e954018fe39e1f9c3631264989569752bb7a (pressure solver, MIT).
//! Equations implemented independently. q=dt*p/rho; velocities in m/s.
//! Solid flux includes the cell open-volume correction from the reference,
//! so the solid velocity extension need not be divergence-free. Geometry
//! construction is a separate operation.

#[derive(Clone, Debug)]
pub(crate) struct Grid {
    pub n: [usize; 3],
    pub h: f64,
    pub phi: Vec<f64>,
    pub open: [Vec<f64>; 3],
    pub open_center: Vec<f64>,
    pub solid: [Vec<f64>; 3],
}
#[derive(Debug)]
pub(crate) struct Projection {
    pub q: Vec<f64>,
    pub residual: f64,
    pub iterations: usize,
}
#[derive(Debug)]
pub(crate) struct System {
    pub rows: Vec<Vec<(usize, f64)>>,
    pub rhs: Vec<f64>,
    pub cells: Vec<usize>,
}
impl Grid {
    pub fn new(n: [usize; 3], h: f64) -> Self {
        let lens = std::array::from_fn::<_, 3, _>(|a| {
            let mut d = n;
            d[a] += 1;
            d.iter().product()
        });
        Self {
            n,
            h,
            phi: vec![h; n.iter().product()],
            open: std::array::from_fn(|a| vec![1.; lens[a]]),
            open_center: vec![1.; n.iter().product()],
            solid: std::array::from_fn(|a| vec![0.; lens[a]]),
        }
    }
    pub fn dims(&self, a: usize) -> [usize; 3] {
        let mut d = self.n;
        d[a] += 1;
        d
    }
    pub fn cell(&self, c: [usize; 3]) -> usize {
        (c[2] * self.n[1] + c[1]) * self.n[0] + c[0]
    }
    pub fn face(&self, a: usize, c: [usize; 3]) -> usize {
        let d = self.dims(a);
        (c[2] * d[1] + c[1]) * d[0] + c[0]
    }
    pub fn coords(&self, i: usize) -> [usize; 3] {
        [
            i % self.n[0],
            i / self.n[0] % self.n[1],
            i / (self.n[0] * self.n[1]),
        ]
    }
    fn neighbor(&self, c: [usize; 3], a: usize, plus: bool) -> Option<usize> {
        let mut t = c;
        if plus {
            if t[a] + 1 == self.n[a] {
                return None;
            }
            t[a] += 1;
        } else {
            t[a] = t[a].checked_sub(1)?;
        }
        Some(self.cell(t))
    }
    // FLIP Fluids caps the AIR/LIQUID distance ratio at 25, so minimum
    // effective interface distance is h/26. The same coefficient must be
    // used in both matrix and gradient; this is conditioning, not damping.
    fn ghost(&self, liquid: usize, other: Option<usize>) -> f64 {
        let air = other.map_or(self.h, |j| self.phi[j]);
        1. + (air / (-self.phi[liquid])).min(25.)
    }
    pub fn divergence(&self, v: &[Vec<f64>; 3], c: [usize; 3]) -> f64 {
        let mut d = 0.;
        let center = self.open_center[self.cell(c)];
        for a in 0..3 {
            let mut hi = c;
            hi[a] += 1;
            let l = self.face(a, c);
            let r = self.face(a, hi);
            let flux =
                |f| self.open[a][f] * v[a][f] + (center - self.open[a][f]) * self.solid[a][f];
            d += (flux(r) - flux(l)) / self.h;
        }
        d
    }
    pub fn assemble(&self, v: &[Vec<f64>; 3]) -> Result<System, &'static str> {
        if !self.h.is_finite() || self.h <= 0. || self.n.contains(&0) {
            return Err("invalid grid");
        }
        if self.phi.len() != self.n.iter().product() || self.phi.iter().any(|x| !x.is_finite()) {
            return Err("invalid phi");
        }
        if self.open_center.len() != self.phi.len()
            || self
                .open_center
                .iter()
                .any(|x| !x.is_finite() || !(0. ..=1.).contains(x))
        {
            return Err("invalid cell fraction");
        }
        for (a, va) in v.iter().enumerate() {
            let len = self.dims(a).iter().product();
            if va.len() != len || self.open[a].len() != len || self.solid[a].len() != len {
                return Err("face dimensions");
            }
            if va.iter().chain(&self.solid[a]).any(|x| !x.is_finite())
                || self.open[a]
                    .iter()
                    .any(|x| !x.is_finite() || !(0. ..=1.).contains(x))
            {
                return Err("invalid face");
            }
        }
        // Solid cells with no open faces carry no pressure unknown. A moving
        // incompatible enclosed pocket is an error, not a fake diagonal.
        let mut cells = Vec::new();
        let mut map = vec![usize::MAX; self.phi.len()];
        for (i, &phi) in self.phi.iter().enumerate() {
            if phi >= 0. {
                continue;
            }
            let c = self.coords(i);
            let mut area = 0.;
            for a in 0..3 {
                let mut hi = c;
                hi[a] += 1;
                area += self.open[a][self.face(a, c)] + self.open[a][self.face(a, hi)];
            }
            if area == 0. {
                if self.divergence(v, c).abs() > 1e-12 {
                    return Err("incompatible closed solid cell");
                }
                continue;
            }
            map[i] = cells.len();
            cells.push(i);
        }
        let mut rows = vec![Vec::new(); cells.len()];
        let mut rhs = vec![0.; cells.len()];
        for (r, &i) in cells.iter().enumerate() {
            let c = self.coords(i);
            rhs[r] = -self.divergence(v, c);
            let mut diag = 0.;
            for a in 0..3 {
                for plus in [false, true] {
                    let mut f = c;
                    if plus {
                        f[a] += 1;
                    }
                    let k = self.open[a][self.face(a, f)] / (self.h * self.h);
                    if k == 0. {
                        continue;
                    }
                    let j = self.neighbor(c, a, plus);
                    if let Some(j) = j.filter(|&j| self.phi[j] < 0.) {
                        if map[j] == usize::MAX {
                            return Err("open face borders inactive solid");
                        }
                        diag += k;
                        rows[r].push((map[j], -k));
                    } else {
                        diag += k * self.ghost(i, j);
                    }
                }
            }
            rows[r].push((r, diag));
        }
        Ok(System { rows, rhs, cells })
    }
    pub fn project(
        &self,
        v: &mut [Vec<f64>; 3],
        tol: f64,
        max_iter: usize,
    ) -> Result<Projection, &'static str> {
        let s = self.assemble(v)?;
        let (x, residual, iterations) = s.solve(tol, max_iter)?;
        let mut q = vec![0.; self.phi.len()];
        for (r, &i) in s.cells.iter().enumerate() {
            q[i] = x[r];
        }
        for (a, va) in v.iter_mut().enumerate() {
            let dims = self.dims(a);
            for z in 0..dims[2] {
                for y in 0..dims[1] {
                    for x in 0..dims[0] {
                        let c = [x, y, z];
                        let f = self.face(a, c);
                        if self.open[a][f] == 0. {
                            va[f] = self.solid[a][f];
                            continue;
                        }
                        let hi = if c[a] < self.n[a] {
                            Some(self.cell(c))
                        } else {
                            None
                        };
                        let mut l = c;
                        let lo = if c[a] > 0 {
                            l[a] -= 1;
                            Some(self.cell(l))
                        } else {
                            None
                        };
                        let lf = lo.is_some_and(|i| self.phi[i] < 0.);
                        let hf = hi.is_some_and(|i| self.phi[i] < 0.);
                        let grad = match (lf, hf) {
                            (true, true) => q[hi.unwrap()] - q[lo.unwrap()],
                            (true, false) => -q[lo.unwrap()] * self.ghost(lo.unwrap(), hi),
                            (false, true) => q[hi.unwrap()] * self.ghost(hi.unwrap(), lo),
                            (false, false) => continue,
                        };
                        // q already contains dt/rho. Open fractions belong in flux
                        // divergence, never as a second multiplier in this gradient.
                        va[f] -= grad / self.h;
                    }
                }
            }
        }
        Ok(Projection {
            q,
            residual,
            iterations,
        })
    }
}
impl System {
    pub fn multiply(&self, x: &[f64]) -> Vec<f64> {
        self.rows
            .iter()
            .map(|r| r.iter().map(|&(j, a)| a * x[j]).sum())
            .collect()
    }
    fn solve(&self, tol: f64, max_iter: usize) -> Result<(Vec<f64>, f64, usize), &'static str> {
        if !tol.is_finite() || tol <= 0. {
            return Err("invalid tolerance");
        }
        let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
        let n = self.rows.len();
        let mut x = vec![0.; n];
        let mut r = self.rhs.clone();
        let bnorm = dot(&r, &r).sqrt();
        if bnorm == 0. {
            return Ok((x, 0., 0));
        }
        let diag: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                row.iter()
                    .filter(|(j, _)| *j == i)
                    .map(|(_, a)| a)
                    .sum::<f64>()
            })
            .collect();
        if diag.iter().any(|d| !d.is_finite() || *d <= 0.) {
            return Err("nonpositive diagonal");
        }
        let mut z: Vec<_> = r.iter().zip(&diag).map(|(r, d)| r / d).collect();
        let mut p = z.clone();
        let mut rz = dot(&r, &z);
        for it in 1..=max_iter {
            let ap = self.multiply(&p);
            let pap = dot(&p, &ap);
            if !pap.is_finite() || pap <= 0. {
                return Err("nonpositive CG curvature");
            }
            let alpha = rz / pap;
            for i in 0..n {
                x[i] += alpha * p[i];
                r[i] -= alpha * ap[i];
            }
            let error = dot(&r, &r).sqrt() / bnorm;
            if error <= tol {
                let ax = self.multiply(&x);
                let actual = ax
                    .iter()
                    .zip(&self.rhs)
                    .map(|(a, b)| (a - b).powi(2))
                    .sum::<f64>()
                    .sqrt()
                    / bnorm;
                if actual <= tol * 2. {
                    return Ok((x, actual, it));
                }
                return Err("recursive residual drift");
            }
            for i in 0..n {
                z[i] = r[i] / diag[i];
            }
            let next = dot(&r, &z);
            let beta = next / rz;
            for i in 0..n {
                p[i] = z[i] + beta * p[i];
            }
            rz = next;
        }
        Err("pressure iteration budget exhausted")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tank() -> (Grid, [Vec<f64>; 3]) {
        let mut g = Grid::new([6, 8, 4], 0.125);
        for i in 0..g.phi.len() {
            g.phi[i] = (g.coords(i)[1] as f64 + 0.5) * g.h - 0.57;
        }
        for a in 0..3 {
            let d = g.dims(a);
            for z in 0..d[2] {
                for y in 0..d[1] {
                    for x in 0..d[0] {
                        let c = [x, y, z];
                        let f = g.face(a, c);
                        if c[a] == 0 || c[a] == g.n[a] {
                            g.open[a][f] = 0.;
                        }
                    }
                }
            }
        }
        let v = std::array::from_fn(|a| vec![0.; g.open[a].len()]);
        (g, v)
    }
    #[test]
    fn subcell_hydrostatic_equilibrium() {
        let (g, mut v) = tank();
        v[1].fill(-9.81 / 120.);
        let s = g.assemble(&v).unwrap();
        let out = g.project(&mut v, 1e-12, 512).unwrap();
        assert!(out.residual < 2e-12);
        assert!(out.iterations > 0);
        for &i in &s.cells {
            let c = g.coords(i);
            assert!(g.divergence(&v, c).abs() < 1e-10);
            for a in 0..3 {
                let mut hi = c;
                hi[a] += 1;
                assert!(v[a][g.face(a, c)].abs() < 1e-8);
                assert!(v[a][g.face(a, hi)].abs() < 1e-8);
            }
            let expected = (9.81 / 120.) * (0.57 - (c[1] as f64 + 0.5) * g.h);
            assert!((out.q[i] - expected).abs() < 1e-10);
        }
    }
    #[test]
    fn tangential_free_slip_flow_unchanged() {
        let (mut g, mut v) = tank();
        g.open[0].fill(1.);
        v[0].fill(2.);
        let out = g.project(&mut v, 1e-12, 100).unwrap();
        assert_eq!(out.iterations, 0);
        assert!(v[0].iter().all(|u| (*u - 2.).abs() < 1e-12));
    }
    #[test]
    fn moving_fractional_solid_flux_and_matrix_identity() {
        let (mut g, mut v) = tank();
        g.open[0].fill(1.);
        g.solid[0].fill(0.4);
        for z in 0..g.n[2] {
            for y in 0..g.n[1] {
                let f = g.face(0, [3, y, z]);
                g.open[0][f] = 0.37;
            }
        }
        v[0].fill(0.4);
        let same = g.project(&mut v, 1e-12, 100).unwrap();
        assert_eq!(same.iterations, 0);
        for (i, u) in v[0].iter_mut().enumerate() {
            *u += 0.03 * (i as f64).sin();
        }
        let s = g.assemble(&v).unwrap();
        for (i, row) in s.rows.iter().enumerate() {
            for &(j, a) in row {
                let reverse = s.rows[j].iter().find(|(k, _)| *k == i).unwrap().1;
                assert_eq!(a, reverse);
            }
        }
        let out = g.project(&mut v, 1e-12, 512).unwrap();
        let x: Vec<_> = s.cells.iter().map(|&i| out.q[i]).collect();
        let ax = s.multiply(&x);
        for (r, &i) in s.cells.iter().enumerate() {
            let div = g.divergence(&v, g.coords(i));
            assert!((div - (ax[r] - s.rhs[r])).abs() < 1e-12);
            assert!(div.abs() < 1e-9);
        }
    }
    #[test]
    fn non_divergence_free_solid_extension_uses_cell_fraction() {
        let (mut g, mut v) = tank();
        g.open_center.fill(0.4);
        g.open[0].fill(0.7);
        let d = g.dims(0);
        for z in 0..d[2] {
            for y in 0..d[1] {
                for x in 0..d[0] {
                    let f = g.face(0, [x, y, z]);
                    g.solid[0][f] = x as f64 * g.h;
                }
            }
        }
        // Unit solid-extension divergence: (center-open)*div(solid)=-0.3.
        // The simplified (1-open) form would instead produce +0.3.
        assert!((g.divergence(&v, [2, 2, 2]) + 0.3).abs() < 1e-12);
        let s = g.assemble(&v).unwrap();
        let out = g.project(&mut v, 1e-12, 512).unwrap();
        assert!(out.residual < 1e-12);
        for i in s.cells {
            assert!(g.divergence(&v, g.coords(i)).abs() < 1e-10);
        }
    }
    #[test]
    fn iteration_exhaustion_does_not_mutate_velocity() {
        let (g, mut v) = tank();
        v[1].fill(-1.);
        let before = v.clone();
        assert!(g.project(&mut v, 1e-12, 0).is_err());
        assert_eq!(v, before);
    }
}
