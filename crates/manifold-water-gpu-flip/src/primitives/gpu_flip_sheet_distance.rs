//! The sheeter's own distance to the surface.
//!
//! `gpu_flip_sheeting` calls a particle thin water when, walking
//! `DEPTH_TEST_CELLS` inward from it, the distance ever rises. The engine's
//! whitewater curvature distance is not deep enough for that: six upwind
//! iterations from the particles' union-of-spheres level set, whose inside
//! values are only about -0.6 cells, stop deepening about 1.4 cells below the
//! surface in moving water and then wobble by +-0.2 cells with the particle
//! lumps, so every wobble read as "rises" and deep water seeded sheets.
//!
//! This builds a true signed distance from the same liquid level set, to
//! `SHEET_DISTANCE_CELLS` deep and clamped there: each cell beside a sign
//! change takes the distance to the interpolated zero crossing, then Jacobi
//! sweeps of the Godunov update of |grad u| = 1 carry it inward. It replaces
//! the curvature distance for the sheeter only; the whitewater keeps its own.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::gpu_flip_sheeting::SHEET_DISTANCE_CELLS;

const SHADER_BODY: &str = include_str!("shaders/gpu_flip_sheet_distance.wgsl");

/// The kernels with the storage helpers they index the lattice through.
fn shader() -> String {
    format!(
        "{}\n{SHADER_BODY}",
        manifold_water_liquid::tile_storage::TILE_STORAGE
    )
}
/// Bytes per fine cell of the two pooled distance fields.
pub(crate) const POOLED_BYTES_PER_CELL: u64 = 2 * 4;

const ENTRIES: [&str; 2] = ["seed", "sweep"];

/// Jacobi sweeps: a sweep carries the distance one cell along an axis, and a
/// diagonal path to depth `SHEET_DISTANCE_CELLS` crosses up to sqrt(3) times
/// that many cells. Even, so the result ends in the first buffer.
pub(crate) const SHEET_DISTANCE_SWEEPS: u32 = ceil_cells(2.0 * SHEET_DISTANCE_CELLS);
const _: () = assert!(SHEET_DISTANCE_SWEEPS.is_multiple_of(2));

const fn ceil_cells(x: f64) -> u32 {
    let t = x as u32;
    if (t as f64) < x { t + 1 } else { t }
}

/// The kernels' uniforms.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    nx: u32,
    ny: u32,
    nz: u32,
    count: u32,
    h: f32,
    depth: f32,
    /// The tile pool's capacity P (gpu_flip_sheet_distance.wgsl ts_pool_tiles).
    pool_tiles: u32,
    pad1: u32,
}

/// The depth the distance is true to, in metres.
pub(crate) fn depth_metres(h: f32) -> f32 {
    (SHEET_DISTANCE_CELLS * f64::from(h)) as f32
}

#[derive(Default)]
pub(crate) struct SheetDistance {
    pipelines: Vec<GpuComputePipeline>,
    /// The two ping-pong fields, pooled cell fields of `pool_len` elements.
    buffers: Option<[GpuBuffer; 2]>,
    cells: [u32; 3],
    pool_tiles: u32,
}

impl SheetDistance {
    /// Called even while sheeting is off, so switching it on mid-show does not
    /// compile on the content thread.
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.pipelines.is_empty() {
            let source = shader();
            for entry in ENTRIES {
                self.pipelines.push(device.create_compute_pipeline(
                    &source,
                    entry,
                    "gpu_flip.sheet_distance",
                ));
            }
        }
    }

    pub(crate) fn reserve(
        &mut self,
        device: &GpuDevice,
        cells: [u32; 3],
        pool_tiles: u32,
    ) -> Result<(), String> {
        if self.buffers.is_some() && self.cells == cells && self.pool_tiles == pool_tiles {
            return Ok(());
        }
        let bytes = manifold_water_liquid::tile_storage::pool_len(pool_tiles) * 4;
        manifold_node_engine::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            manifold_water_liquid::tile_storage::pool_len(pool_tiles) * POOLED_BYTES_PER_CELL,
        )
        .map_err(|e| e.to_string())?;
        self.buffers = Some([
            device.try_create_buffer(bytes)?,
            device.try_create_buffer(bytes)?,
        ]);
        self.cells = cells;
        self.pool_tiles = pool_tiles;
        Ok(())
    }

    /// Both fields, for the proofs.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn fields(&self) -> &[GpuBuffer; 2] {
        self.buffers.as_ref().expect("sheet distance reserved")
    }

    /// Builds the distance of `source` (the liquid level set, a pooled cell
    /// field) and returns the pooled buffer holding it. Gated by the FLIP
    /// clock plan: an inactive substep slot (live clock, zero step) changes
    /// nothing.
    pub(crate) fn encode_gated(
        &self,
        enc: &mut GpuEncoder,
        source: &GpuBuffer,
        h: f32,
        plan: &GpuBuffer,
        table: &GpuBuffer,
        pool_tiles: u32,
    ) -> &GpuBuffer {
        let [a, b] = self.buffers.as_ref().expect("sheet distance reserved");
        let [nx, ny, nz] = self.cells;
        let count = nx * ny * nz;
        let params = Params {
            nx,
            ny,
            nz,
            count,
            h,
            depth: depth_metres(h),
            pool_tiles,
            pad1: 0,
        };
        let mut pass = |entry: usize, from: &GpuBuffer, to: &GpuBuffer, label: &'static str| {
            enc.dispatch_compute(
                &self.pipelines[entry],
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&params),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: source,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: from,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: to,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: plan,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 5,
                        buffer: table,
                        offset: 0,
                    },
                ],
                [count.div_ceil(256).max(1), 1, 1],
                label,
            );
            enc.compute_memory_barrier_buffers();
        };
        pass(0, b, a, "gpu_flip.sheet_distance.seed");
        for sweep in 0..SHEET_DISTANCE_SWEEPS {
            if sweep % 2 == 0 {
                pass(1, a, b, "gpu_flip.sheet_distance.sweep");
            } else {
                pass(1, b, a, "gpu_flip.sheet_distance.sweep");
            }
        }
        a
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_node_engine::testkit::array_harness::read;
    use manifold_water_liquid::tile_storage::testkit::{from_pool, identity_table, to_pool};

    const CELLS: [u32; 3] = [24, 40, 20];
    const H: f32 = 0.0625;
    /// The water's top, in cells.
    const Y0: f32 = 30.3;

    fn index(x: u32, y: u32, z: u32) -> usize {
        (x + CELLS[0] * (y + CELLS[1] * z)) as usize
    }

    /// A fixed hash in [0, 1).
    fn noise(x: u32, y: u32, z: u32) -> f32 {
        let mut v =
            x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663) ^ z.wrapping_mul(83_492_791);
        v ^= v >> 13;
        v = v.wrapping_mul(0x5bd1_e995);
        v ^= v >> 15;
        (v & 0x00ff_ffff) as f32 / 16_777_216.0
    }

    /// Water below Y0 with the particles' shallow, noisy inside values;
    /// outside, the true distance above Y0.
    fn deep_water() -> Vec<f32> {
        let mut phi = vec![0.0; (CELLS[0] * CELLS[1] * CELLS[2]) as usize];
        for z in 0..CELLS[2] {
            for y in 0..CELLS[1] {
                for x in 0..CELLS[0] {
                    let yc = y as f32 + 0.5;
                    phi[index(x, y, z)] = if yc < Y0 {
                        -(0.5 + 0.3 * noise(x, y, z)) * H
                    } else {
                        (yc - Y0) * H
                    };
                }
            }
        }
        phi
    }

    fn shared(device: &GpuDevice, words: &[u32]) -> GpuBuffer {
        let buffer = device.create_buffer_shared((4 * words.len()).max(16) as u64);
        buffer.zero_fill();
        // SAFETY: a fresh shared buffer sized for `words`; no GPU work in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(words)) };
        buffer
    }

    fn copy_out(device: &GpuDevice, buffer: &GpuBuffer) -> Vec<u32> {
        let staged = device.create_buffer_shared(buffer.size);
        let mut enc = device.create_encoder("sheet distance readback");
        enc.copy_buffer_to_buffer(buffer, &staged, buffer.size);
        enc.commit_and_wait_completed();
        read(&staged, (buffer.size / 4) as usize)
    }

    /// A clock plan: `live_mode` and `step_dt` only (common.wgsl ClockPlan).
    fn plan(device: &GpuDevice, live_mode: u32, step_dt: f32) -> GpuBuffer {
        let mut words = [0u32; 12];
        words[0] = step_dt.to_bits();
        words[11] = live_mode;
        shared(device, &words)
    }

    /// Every box tile allocated: the identity pool table and its capacity.
    fn pool(device: &GpuDevice) -> (GpuBuffer, u32) {
        let (words, pool_tiles) = identity_table(CELLS);
        (shared(device, &words), pool_tiles)
    }

    fn stage(device: &GpuDevice) -> SheetDistance {
        let mut stage = SheetDistance::default();
        stage.prepare(device);
        stage.reserve(device, CELLS, pool(device).1).unwrap();
        stage
    }

    /// The distance of the dense `phi`, read back as a dense array.
    fn run(device: &GpuDevice, stage: &SheetDistance, phi: &[f32], plan: &GpuBuffer) -> Vec<f32> {
        let (table, pool_tiles) = pool(device);
        let pooled = to_pool(phi, CELLS, &[3.0 * H]);
        let source = shared(device, bytemuck::cast_slice(&pooled));
        let mut enc = device.create_encoder("sheet distance");
        let out = stage.encode_gated(&mut enc, &source, H, plan, &table, pool_tiles);
        enc.commit_and_wait_completed();
        let out: Vec<f32> = copy_out(device, out)
            .into_iter()
            .map(f32::from_bits)
            .collect();
        from_pool(&out, CELLS)
    }

    /// The same seed and sweeps on the CPU, in f32 and the kernel's order.
    fn cpu_reference(phi: &[f32]) -> Vec<f32> {
        let [nx, ny, nz] = CELLS.map(|n| n as i32);
        let depth = depth_metres(H);
        let sign = |v: f32| if v >= 0.0 { 1.0f32 } else { -1.0 };
        let at = |x: i32, y: i32, z: i32| (x + nx * (y + ny * z)) as usize;
        let neighbours = |x: i32, y: i32, z: i32| {
            let mut out: Vec<(usize, usize)> = Vec::new();
            for axis in 0..3 {
                for d in [-1, 1] {
                    let mut q = [x, y, z];
                    q[axis] += d;
                    if q[0] >= 0 && q[1] >= 0 && q[2] >= 0 && q[0] < nx && q[1] < ny && q[2] < nz {
                        out.push((axis, at(q[0], q[1], q[2])));
                    }
                }
            }
            out
        };
        let crossing = |x: i32, y: i32, z: i32| {
            let p = phi[at(x, y, z)];
            let mut best = -1.0f32;
            for (_, j) in neighbours(x, y, z) {
                if sign(phi[j]) == sign(p) {
                    continue;
                }
                let t = H * p.abs() / (p.abs() + phi[j].abs());
                if best < 0.0 || t < best {
                    best = t;
                }
            }
            best
        };
        let mut a = vec![0.0f32; phi.len()];
        let mut b = vec![0.0f32; phi.len()];
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let i = at(x, y, z);
                    let t = crossing(x, y, z);
                    a[i] = sign(phi[i]) * if t >= 0.0 { t } else { depth };
                }
            }
        }
        for _ in 0..SHEET_DISTANCE_SWEEPS {
            for z in 0..nz {
                for y in 0..ny {
                    for x in 0..nx {
                        let i = at(x, y, z);
                        if crossing(x, y, z) >= 0.0 {
                            b[i] = a[i];
                            continue;
                        }
                        let s = sign(phi[i]);
                        let mut m = [depth; 3];
                        for (axis, j) in neighbours(x, y, z) {
                            if sign(phi[j]) == s {
                                m[axis] = m[axis].min(a[j].abs());
                            }
                        }
                        let lo = m[0].min(m[1].min(m[2]));
                        let hi = m[0].max(m[1].max(m[2]));
                        let (a1, a2, a3) = (lo, m[0].min(m[1]).max(m[0].max(m[1]).min(m[2])), hi);
                        let mut v = a1 + H;
                        if v > a2 {
                            v = (a1 + a2 + (2.0 * H * H - (a1 - a2) * (a1 - a2)).max(0.0).sqrt())
                                / 2.0;
                            if v > a3 {
                                let sum = a1 + a2 + a3;
                                v = (sum
                                    + (sum * sum - 3.0 * (a1 * a1 + a2 * a2 + a3 * a3 - H * H))
                                        .max(0.0)
                                        .sqrt())
                                    / 3.0;
                            }
                        }
                        b[i] = s * a[i].abs().min(v.min(depth));
                    }
                }
            }
            std::mem::swap(&mut a, &mut b);
        }
        a
    }

    /// Down every column the distance is the true depth to the sheeter's
    /// reach, then holds at the clamp, and strictly deepens in between: what
    /// the walk's "does it ever rise" test needs.
    #[test]
    fn deep_water_gets_its_true_depth() {
        let device = manifold_gpu::testkit::test_device();
        let stage = stage(&device);
        let out = run(&device, &stage, &deep_water(), &plan(&device, 0, 0.0));
        let depth = depth_metres(H);
        let reach = SHEET_DISTANCE_CELLS as f32;
        let top = (Y0 - 0.5).floor() as u32; // the highest water cell
        for z in 0..CELLS[2] {
            for x in 0..CELLS[0] {
                for y in 0..=top {
                    let cells_deep = Y0 - (y as f32 + 0.5);
                    let got = out[index(x, y, z)];
                    if cells_deep <= reach - 0.5 {
                        assert!(
                            (got / H + cells_deep).abs() <= 0.1,
                            "column ({x},{z}) y {y}: {} cells vs {}",
                            got / H,
                            -cells_deep
                        );
                    } else if cells_deep >= reach + 0.2 {
                        assert_eq!(got, -depth, "column ({x},{z}) y {y} holds the clamp");
                    }
                }
                let mut above = out[index(x, top, z)];
                for y in (0..top).rev() {
                    let here = out[index(x, y, z)];
                    if above != -depth {
                        assert!(here < above, "column ({x},{z}) y {y}: {here} !< {above}");
                    }
                    above = here;
                }
            }
        }
    }

    /// A sheet 1.5 cells thick is nowhere deeper than half its thickness.
    #[test]
    fn thin_sheet_stays_thin() {
        let device = manifold_gpu::testkit::test_device();
        let stage = stage(&device);
        let (lo, hi) = (10.2f32, 11.7f32);
        let mut phi = vec![0.0; (CELLS[0] * CELLS[1] * CELLS[2]) as usize];
        for z in 0..CELLS[2] {
            for y in 0..CELLS[1] {
                for x in 0..CELLS[0] {
                    let yc = y as f32 + 0.5;
                    let inside = (yc - lo).min(hi - yc);
                    // Signed distance to the nearer face: negative inside.
                    phi[index(x, y, z)] = if yc > lo && yc < hi {
                        -inside * H
                    } else {
                        (lo - yc).max(yc - hi) * H
                    };
                }
            }
        }
        let out = run(&device, &stage, &phi, &plan(&device, 0, 0.0));
        let deepest = out.iter().map(|v| -v / H).fold(f32::MIN, f32::max);
        assert!(
            deepest <= 0.75 + 0.1,
            "deepest inside value {deepest} cells"
        );
        assert!(deepest > 0.0, "the sheet has an inside");
    }

    /// An inactive substep slot (live clock, zero step) leaves both fields
    /// bit for bit as they were.
    #[test]
    fn inactive_slot_changes_nothing() {
        let device = manifold_gpu::testkit::test_device();
        let stage = stage(&device);
        run(&device, &stage, &deep_water(), &plan(&device, 0, 0.0));
        let snapshot = |stage: &SheetDistance| -> Vec<Vec<u32>> {
            stage
                .fields()
                .iter()
                .map(|b| copy_out(&device, b))
                .collect()
        };
        let before = snapshot(&stage);
        let flipped: Vec<f32> = deep_water().iter().map(|v| -v).collect();
        run(&device, &stage, &flipped, &plan(&device, 1, 0.0));
        assert!(before == snapshot(&stage), "an inactive slot wrote");
        run(&device, &stage, &flipped, &plan(&device, 1, 0.01));
        assert!(before != snapshot(&stage), "an active live slot writes");
    }

    /// The kernels match the same seed and sweeps run on the CPU, to a few
    /// float steps: the pipelines compile with fast math, so the GPU's divide
    /// and square root differ from the CPU's in the last bits and twelve
    /// sweeps carry that along.
    #[test]
    fn matches_the_cpu_reference() {
        let device = manifold_gpu::testkit::test_device();
        let stage = stage(&device);
        let phi = deep_water();
        let got = run(&device, &stage, &phi, &plan(&device, 0, 0.0));
        let want = cpu_reference(&phi);
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(
                (g - w).abs() <= 1e-5 * w.abs().max(H),
                "cell {i}: gpu {g} vs cpu {w}"
            );
        }
    }
}
