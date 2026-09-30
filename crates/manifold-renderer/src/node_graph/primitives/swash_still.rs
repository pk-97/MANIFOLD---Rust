//! A CPU still of a water surface mesh in the 4 m Dam Break tank, the same
//! for every solver in the FFT water race (docs/FFT_WATER_SOLVER_DESIGN.md
//! P3): one fixed camera, one light, flat two-sided shading and a depth
//! buffer, so stills compare surface shape, not shading. Written only when
//! `SWASH_STILLS` names a directory.

use std::path::PathBuf;

const WIDTH: usize = 640;
const HEIGHT: usize = 400;
/// The part of the tank the camera frames: the full floor, up to 2.5 m.
const FRAME: [[f64; 2]; 3] = [[-2.0, 2.0], [0.0, 2.5], [-2.0, 2.0]];

type Vec3 = [f64; 3];

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn unit(a: Vec3) -> Vec3 {
    let l = dot(a, a).sqrt().max(1e-12);
    a.map(|c| c / l)
}

/// An orthographic camera 35° round from +z and 25° above the floor.
struct Camera {
    right: Vec3,
    up: Vec3,
    forward: Vec3,
    centre: Vec3,
    scale: f64,
}

impl Camera {
    fn new() -> Self {
        let (yaw, pitch) = (35_f64.to_radians(), 25_f64.to_radians());
        let forward = [-yaw.sin() * pitch.cos(), -pitch.sin(), -yaw.cos() * pitch.cos()];
        let right = unit(cross(forward, [0.0, 1.0, 0.0]));
        let up = cross(right, forward);
        let centre = FRAME.map(|[lo, hi]| 0.5 * (lo + hi));
        let mut camera = Self { right, up, forward, centre, scale: 1.0 };
        let (mut x, mut y) = ([f64::MAX, f64::MIN], [f64::MAX, f64::MIN]);
        for corner in 0..8 {
            let p = [0, 1, 2].map(|a| FRAME[a][(corner >> a) & 1]);
            let (sx, sy, _) = camera.view(p);
            x = [x[0].min(sx), x[1].max(sx)];
            y = [y[0].min(sy), y[1].max(sy)];
        }
        camera.scale = 0.95 * (WIDTH as f64 / (x[1] - x[0])).min(HEIGHT as f64 / (y[1] - y[0]));
        camera
    }

    /// Screen-space x and y in metres from the frame's centre, and depth.
    fn view(&self, p: Vec3) -> (f64, f64, f64) {
        let d = sub(p, self.centre);
        (dot(d, self.right), dot(d, self.up), dot(d, self.forward))
    }

    fn pixel(&self, p: Vec3) -> (f64, f64, f64) {
        let (x, y, depth) = self.view(p);
        (0.5 * WIDTH as f64 + x * self.scale, 0.5 * HEIGHT as f64 - y * self.scale, depth)
    }
}

struct Canvas {
    camera: Camera,
    depth: Vec<f64>,
    rgb: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        Self { camera: Camera::new(), depth: vec![f64::MAX; WIDTH * HEIGHT], rgb: vec![18; 3 * WIDTH * HEIGHT] }
    }

    fn triangle(&mut self, t: [Vec3; 3], colour: [f64; 3]) {
        let [a, b, c] = t.map(|p| self.camera.pixel(p));
        let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if area.abs() < 1e-12 {
            return;
        }
        let x0 = a.0.min(b.0).min(c.0).floor().max(0.0) as usize;
        let x1 = (a.0.max(b.0).max(c.0).ceil() as usize).min(WIDTH - 1);
        let y0 = a.1.min(b.1).min(c.1).floor().max(0.0) as usize;
        let y1 = (a.1.max(b.1).max(c.1).ceil() as usize).min(HEIGHT - 1);
        let rgb = colour.map(|c| (255.0 * c.clamp(0.0, 1.0)) as u8);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let wa = ((b.0 - px) * (c.1 - py) - (b.1 - py) * (c.0 - px)) / area;
                let wb = ((c.0 - px) * (a.1 - py) - (c.1 - py) * (a.0 - px)) / area;
                let wc = 1.0 - wa - wb;
                if wa < 0.0 || wb < 0.0 || wc < 0.0 {
                    continue;
                }
                let z = wa * a.2 + wb * b.2 + wc * c.2;
                let i = x + WIDTH * y;
                if z < self.depth[i] {
                    self.depth[i] = z;
                    self.rgb[3 * i..3 * i + 3].copy_from_slice(&rgb);
                }
            }
        }
    }
}

/// Writes `name`.png into `SWASH_STILLS`: the tank floor in grey and the
/// surface's triangles in water blue, lit from the camera's upper left.
/// Does nothing when `SWASH_STILLS` is unset.
pub(crate) fn write_still(name: &str, triangles: impl Iterator<Item = [[f32; 3]; 3]>) {
    let Some(dir) = std::env::var_os("SWASH_STILLS").map(PathBuf::from) else {
        return;
    };
    let mut canvas = Canvas::new();
    let [x, _, z] = FRAME;
    let floor = [[x[0], 0.0, z[0]], [x[1], 0.0, z[0]], [x[1], 0.0, z[1]], [x[0], 0.0, z[1]]];
    canvas.triangle([floor[0], floor[1], floor[2]], [0.32; 3]);
    canvas.triangle([floor[0], floor[2], floor[3]], [0.32; 3]);
    let light = unit([-0.4, 1.0, 0.6]);
    for t in triangles {
        let t = t.map(|p| p.map(f64::from));
        let shade = dot(unit(cross(sub(t[1], t[0]), sub(t[2], t[0]))), light).abs();
        canvas.triangle(t, [0.30, 0.55, 0.85].map(|c| c * (0.25 + 0.75 * shade)));
    }
    std::fs::create_dir_all(&dir).expect("stills directory");
    let path = dir.join(format!("{name}.png"));
    image::save_buffer(&path, &canvas.rgb, WIDTH as u32, HEIGHT as u32, image::ExtendedColorType::Rgb8)
        .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
}
