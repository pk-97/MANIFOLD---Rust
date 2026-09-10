//! Independent f64 quadratic APIC reference on a staggered MAC grid.
#[derive(Clone, Copy)]
pub(crate) struct Particle {
    pub(crate) p: [f64; 3],
    pub(crate) v: [f64; 3],
    pub(crate) c: [[f64; 3]; 3],
    pub(crate) m: f64,
}
pub(crate) struct Grid {
    n: [usize; 3],
    h: f64,
    o: [f64; 3],
    mass: [Vec<f64>; 3],
    mom: [Vec<f64>; 3],
}
impl Grid {
    pub(crate) fn new(n: [usize; 3], h: f64, o: [f64; 3]) -> Self {
        let s = [
            (n[0] + 1) * n[1] * n[2],
            n[0] * (n[1] + 1) * n[2],
            n[0] * n[1] * (n[2] + 1),
        ];
        Self {
            n,
            h,
            o,
            mass: [vec![0.; s[0]], vec![0.; s[1]], vec![0.; s[2]]],
            mom: [vec![0.; s[0]], vec![0.; s[1]], vec![0.; s[2]]],
        }
    }
    pub(crate) fn weights(x: f64) -> [f64; 3] {
        [
            0.5 * (1.5 - x).powi(2),
            0.75 - (x - 1.).powi(2),
            0.5 * (x - 0.5).powi(2),
        ]
    }
    fn dims(&self, a: usize) -> [usize; 3] {
        [
            self.n[0] + (a == 0) as usize,
            self.n[1] + (a == 1) as usize,
            self.n[2] + (a == 2) as usize,
        ]
    }
    fn ix(&self, a: usize, q: [usize; 3]) -> usize {
        let d = self.dims(a);
        assert!(q[0] < d[0] && q[1] < d[1] && q[2] < d[2]);
        (q[2] * d[1] + q[1]) * d[0] + q[0]
    }
    fn face(&self, a: usize, q: [usize; 3]) -> [f64; 3] {
        std::array::from_fn(|d| self.o[d] + self.h * (q[d] as f64 + if d == a { 0.0 } else { 0.5 }))
    }
    fn st(&self, p: [f64; 3], a: usize) -> ([isize; 3], [[f64; 3]; 3]) {
        let mut b = [0; 3];
        let mut w = [[0.; 3]; 3];
        for d in 0..3 {
            let q = (p[d] - self.o[d]) / self.h - if d == a { 0.0 } else { 0.5 };
            b[d] = (q - 0.5).floor() as isize;
            w[d] = Self::weights(q - b[d] as f64)
        }
        (b, w)
    }
    pub(crate) fn transfer(&mut self, ps: &[Particle]) {
        for a in 0..3 {
            self.mass[a].fill(0.);
            self.mom[a].fill(0.)
        }
        for p in ps {
            for a in 0..3 {
                let (b, w) = self.st(p.p, a);
                for i in 0..3 {
                    for j in 0..3 {
                        for k in 0..3 {
                            let z = [b[0] + i as isize, b[1] + j as isize, b[2] + k as isize];
                            assert!(!z.iter().any(|x| *x < 0), "negative stencil index: {z:?}");
                            let q = [z[0] as usize, z[1] as usize, z[2] as usize];
                            let wt = w[0][i] * w[1][j] * w[2][k];
                            let f = self.face(a, q);
                            let r = [f[0] - p.p[0], f[1] - p.p[1], f[2] - p.p[2]];
                            let x = self.ix(a, q);
                            self.mass[a][x] += p.m * wt;
                            self.mom[a][x] += p.m * wt * (p.v[a] + dot(p.c[a], r))
                        }
                    }
                }
            }
        }
        for a in 0..3 {
            for i in 0..self.mass[a].len() {
                if self.mass[a][i] > 0. {
                    self.mom[a][i] /= self.mass[a][i]
                }
            }
        }
    }
    pub(crate) fn face_mass(&self, a: usize, q: [usize; 3]) -> f64 {
        self.mass[a][self.ix(a, q)]
    }
    pub(crate) fn face_velocity(&self, a: usize, q: [usize; 3]) -> f64 {
        self.mom[a][self.ix(a, q)]
    }
    pub(crate) fn gather(&self, p: &Particle) -> ([f64; 3], [[f64; 3]; 3]) {
        let mut v = [0.; 3];
        let mut c = [[0.; 3]; 3];
        for a in 0..3 {
            let (b, w) = self.st(p.p, a);
            for i in 0..3 {
                for j in 0..3 {
                    for k in 0..3 {
                        let z = [b[0] + i as isize, b[1] + j as isize, b[2] + k as isize];
                        assert!(!z.iter().any(|x| *x < 0), "negative stencil index: {z:?}");
                        let q = [z[0] as usize, z[1] as usize, z[2] as usize];
                        let wt = w[0][i] * w[1][j] * w[2][k];
                        let f = self.face(a, q);
                        let r = [f[0] - p.p[0], f[1] - p.p[1], f[2] - p.p[2]];
                        let x = self.mom[a][self.ix(a, q)];
                        v[a] += wt * x;
                        for d in 0..3 {
                            c[a][d] += 4. / (self.h * self.h) * wt * x * r[d]
                        }
                    }
                }
            }
        }
        (v, c)
    }
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-10, "{a} != {b}")
}
fn p(x: [f64; 3], v: [f64; 3], c: [[f64; 3]; 3], m: f64) -> Particle {
    Particle { p: x, v, c, m }
}
#[test]
fn weights() {
    for x in [0.5, 0.73, 1., 1.499] {
        let w = Grid::weights(x);
        assert!(w.iter().all(|x| *x >= 0.));
        close(w.iter().sum(), 1.);
        close(w[0] * (0. - x) + w[1] * (1. - x) + w[2] * (2. - x), 0.);
        close(
            w[0] * (0. - x).powi(2) + w[1] * (1. - x).powi(2) + w[2] * (2. - x).powi(2),
            0.25,
        )
    }
}
#[test]
fn affine_multi_conservation() {
    let a = [[0.2, 0.31, -0.4], [-0.17, 0.5, 0.23], [0.61, -0.29, 0.11]];
    let b = [1.2, -0.7, 2.4];
    let ps = [
        p(
            [0.337, 0.291, 0.423],
            [
                b[0] + dot(a[0], [0.337, 0.291, 0.423]),
                b[1] + dot(a[1], [0.337, 0.291, 0.423]),
                b[2] + dot(a[2], [0.337, 0.291, 0.423]),
            ],
            a,
            2.7,
        ),
        p(
            [0.411, 0.367, 0.509],
            [
                b[0] + dot(a[0], [0.411, 0.367, 0.509]),
                b[1] + dot(a[1], [0.411, 0.367, 0.509]),
                b[2] + dot(a[2], [0.411, 0.367, 0.509]),
            ],
            a,
            1.3,
        ),
    ];
    let mut g = Grid::new([16; 3], 1. / 16., [0.; 3]);
    g.transfer(&ps);
    for x in &ps {
        let (v, c) = g.gather(x);
        for d in 0..3 {
            close(v[d], x.v[d]);
            for e in 0..3 {
                close(c[d][e], a[d][e])
            }
        }
    }
    for d in 0..3 {
        close(g.mass[d].iter().sum(), 4.);
        close(
            g.mass[d].iter().zip(&g.mom[d]).map(|(m, v)| m * v).sum(),
            ps.iter().map(|x| x.m * x.v[d]).sum(),
        )
    }
}
#[test]
fn translated_zero() {
    let mut g = Grid::new([16; 3], 1. / 16., [2.; 3]);
    let z = p([2.337, 2.291, 2.423], [0.; 3], [[0.; 3]; 3], 1.);
    g.transfer(&[z]);
    let (v, c) = g.gather(&z);
    assert!(v.iter().chain(c.iter().flatten()).all(|x| x.is_finite()));
    for x in v {
        close(x, 0.)
    }
    for x in c.into_iter().flatten() {
        close(x, 0.)
    }
}

#[test]
fn constant_velocity_nonzero_origin() {
    let origin = [2.0, -1.0, 0.5];
    let position = [origin[0] + 0.337, origin[1] + 0.291, origin[2] + 0.423];
    let velocity = [1.2, -0.7, 2.4];
    let particle = p(position, velocity, [[0.0; 3]; 3], 2.7);
    let mut grid = Grid::new([16; 3], 1. / 16., origin);
    grid.transfer(&[particle]);
    let (gathered_velocity, gathered_affine) = grid.gather(&particle);
    for d in 0..3 {
        // A supported face must retain the constant field before gathering.
        assert!(grid.face_mass(d, [5, 4, 6]) > 0.0);
        close(grid.face_velocity(d, [5, 4, 6]), velocity[d]);
        close(gathered_velocity[d], velocity[d]);
        for component in gathered_affine[d] {
            close(component, 0.0);
        }
    }
}
