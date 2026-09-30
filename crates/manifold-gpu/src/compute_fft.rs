//! Compute-kernel FFT prototype (BUG-l2h3.7, own FFT kernels): the real 3D and
//! batched 2D transforms SWASH runs, in plain compute kernels with the layout
//! and scaling of `GpuFft::new_nd`, so the two can be measured on the same
//! data. Not wired into any atom yet.
//!
//! Layout: the real lattice is row-major `[nz, ny, nx]` (x fastest), the half
//! spectrum `[nz, ny, nx/2 + 1]` interleaved complex. Axes 3 transforms x, y
//! and z; axes 2 transforms x and y of every z slice on its own. The forward
//! transform is unscaled; the inverse scales by one over the transformed
//! lengths' product, so the pair round-trips.
//!
//! Each axis is one dispatch. A threadgroup loads whole lines into threadgroup
//! memory, runs a mixed-radix Stockham FFT there (radices 8, 4, 2, 3, 5, 7)
//! and writes them back. Lines along x are real: a length-nx real line is
//! transformed as nx/2 complex pairs, then split into its half spectrum (and
//! the reverse for the inverse). Every transformed length must be even with no
//! prime factor above 7.

use crate::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

/// Direction of a [`ComputeFft`] plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputeFftKind {
    /// Real lattice → half spectrum, unscaled.
    RealToHermitean,
    /// Half spectrum → real lattice, scaled by 1 / (transformed volume).
    HermiteanToReal,
}

/// Threads per threadgroup the line passes aim for.
const TARGET_THREADS: u32 = 256;
/// Threadgroup memory a line pass may use, in bytes.
const SHARED_BYTES: u32 = 16 * 1024;

/// Radices for a line of `n` points, largest first, or `None` when `n` has a
/// prime factor above 7. Powers of two go in eights and fours, avoiding a
/// radix-2 stage where possible: it doubles the threads a line needs.
pub fn radix_plan(n: u32) -> Option<Vec<u32>> {
    if n < 2 {
        return None;
    }
    let mut rest = n;
    let mut twos = 0;
    while rest.is_multiple_of(2) {
        rest /= 2;
        twos += 1;
    }
    let mut radices = Vec::new();
    match (twos % 3, twos) {
        (_, 1) => radices.push(2),
        (1, _) => {
            radices.extend(std::iter::repeat_n(8, (twos - 4) / 3));
            radices.extend([4, 4]);
        }
        (2, _) => {
            radices.extend(std::iter::repeat_n(8, (twos - 2) / 3));
            radices.push(4);
        }
        _ => radices.extend(std::iter::repeat_n(8, twos / 3)),
    }
    for prime in [3, 5, 7] {
        while rest.is_multiple_of(prime) {
            rest /= prime;
            radices.push(prime);
        }
    }
    (rest == 1).then_some(radices)
}

/// Lines per threadgroup and threads per line for a line of `n` points.
fn line_shape(n: u32, radices: &[u32]) -> (u32, u32) {
    let threads = n / radices.iter().copied().min().expect("a line has at least one radix");
    let lines = (TARGET_THREADS / threads).clamp(1, (SHARED_BYTES / (n * 8)).max(1));
    (lines, threads)
}

/// Lines per threadgroup for a strided pass over `h` spectrum columns: the
/// count that pads `h` least (h = n/2 + 1 is odd, so the power-of-two default
/// would leave most of a last threadgroup idle), the larger on a tie.
fn strided_lines(n: u32, radices: &[u32], h: u32) -> u32 {
    let (most, _) = line_shape(n, radices);
    (1..=most).min_by_key(|&l| (h.div_ceil(l) * l, std::cmp::Reverse(l))).expect("at least one line")
}

/// `e^{-2πi k / n}` for `k` in `0..count`, computed in f64.
fn twiddles(n: u32, count: u32) -> Vec<[f32; 2]> {
    (0..count)
        .map(|k| {
            let angle = -2.0 * std::f64::consts::PI * f64::from(k) / f64::from(n);
            [angle.cos() as f32, angle.sin() as f32]
        })
        .collect()
}

fn f(value: f64) -> String {
    format!("{value:.9}")
}

/// `i·z` (inverse) or `-i·z` (forward).
fn rot(z: &str, inverse: bool) -> String {
    if inverse { format!("vec2<f32>(-({z}).y, ({z}).x)") } else { format!("vec2<f32>(({z}).y, -({z}).x)") }
}

/// Radix-`r` DFT over `u0..u{r-1}`, results back in the same variables.
fn butterfly(r: u32, inverse: bool) -> String {
    let sign = if inverse { 1.0 } else { -1.0 };
    match r {
        2 => "{ let a = u0 + u1; u1 = u0 - u1; u0 = a; }\n".into(),
        4 => format!(
            "{{ let a0 = u0 + u2; let a1 = u0 - u2; let a2 = u1 + u3; let a3 = {};\n\
             u0 = a0 + a2; u1 = a1 + a3; u2 = a0 - a2; u3 = a1 - a3; }}\n",
            rot("u1 - u3", inverse)
        ),
        8 => {
            let h = std::f64::consts::FRAC_1_SQRT_2;
            let w1 = format!("vec2<f32>({}, {})", f(h), f(sign * h));
            let w3 = format!("vec2<f32>({}, {})", f(-h), f(sign * h));
            format!(
                "{{ let ea0 = u0 + u4; let ea1 = u0 - u4; let ea2 = u2 + u6; let ea3 = {};\n\
                 let e0 = ea0 + ea2; let e1 = ea1 + ea3; let e2 = ea0 - ea2; let e3 = ea1 - ea3;\n\
                 let oa0 = u1 + u5; let oa1 = u1 - u5; let oa2 = u3 + u7; let oa3 = {};\n\
                 let o0 = oa0 + oa2; let o1 = cmul(oa1 + oa3, {w1}); let o2 = {}; let o3 = cmul(oa1 - oa3, {w3});\n\
                 u0 = e0 + o0; u4 = e0 - o0; u1 = e1 + o1; u5 = e1 - o1;\n\
                 u2 = e2 + o2; u6 = e2 - o2; u3 = e3 + o3; u7 = e3 - o3; }}\n",
                rot("u2 - u6", inverse),
                rot("u3 - u7", inverse),
                rot("oa0 - oa2", inverse),
            )
        }
        _ => {
            let mut code = String::from("{\n");
            for c in 0..r {
                let mut terms = vec!["u0".to_string()];
                for b in 1..r {
                    let m = (b * c) % r;
                    if m == 0 {
                        terms.push(format!("u{b}"));
                    } else {
                        let angle = sign * 2.0 * std::f64::consts::PI * f64::from(m) / f64::from(r);
                        terms.push(format!("cmul(u{b}, vec2<f32>({}, {}))", f(angle.cos()), f(angle.sin())));
                    }
                }
                code.push_str(&format!("let y{c} = {};\n", terms.join(" + ")));
            }
            for c in 0..r {
                code.push_str(&format!("u{c} = y{c};\n"));
            }
            code.push_str("}\n");
            code
        }
    }
}

/// The Stockham stages for one line of `n` points held at `sh[sb..sb + n]`,
/// run by the line's threads `t < threads`. Twiddles come from `tw[0..n]`.
fn stages(n: u32, radices: &[u32], inverse: bool) -> String {
    let mut code = String::new();
    let mut p = 1;
    for &r in radices {
        let count = n / r;
        let step = n / (p * r);
        code.push_str(&format!("{{\nlet run = t < {count}u;\nlet k = t % {p}u;\n"));
        for q in 0..r {
            code.push_str(&format!("var u{q}: vec2<f32>;\n"));
        }
        code.push_str("if (run) {\n");
        for q in 0..r {
            code.push_str(&format!("u{q} = sh[sb + t + {}u];\n", q * count));
        }
        if p > 1 {
            for q in 1..r {
                let w = if inverse {
                    format!("conj(tw[k * {}u])", q * step)
                } else {
                    format!("tw[k * {}u]", q * step)
                };
                code.push_str(&format!("u{q} = cmul(u{q}, {w});\n"));
            }
        }
        code.push_str(&butterfly(r, inverse));
        code.push_str("}\nworkgroupBarrier();\nif (run) {\n");
        code.push_str(&format!("let j = (t - k) * {r}u + k;\n"));
        for q in 0..r {
            code.push_str(&format!("sh[sb + j + {}u] = u{q};\n", q * p));
        }
        code.push_str("}\nworkgroupBarrier();\n}\n");
        p *= r;
    }
    code
}

const HELPERS: &str = "fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {\n\
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);\n}\n\
fn conj(a: vec2<f32>) -> vec2<f32> { return vec2<f32>(a.x, -a.y); }\n";

/// A complex pass along a strided axis. Lines are indexed by `a` (the
/// contiguous spectrum index, `0..h`, grouped `lines` to a threadgroup so
/// loads coalesce) and by `workgroup_id.y` (the other axis); element `e` of
/// line (a, b) sits at `a + b·b_stride + e·e_stride`.
fn strided_pass_wgsl(n: u32, radices: &[u32], inverse: bool, h: u32, b_stride: u32, e_stride: u32, in_place: bool) -> String {
    let (_, threads) = line_shape(n, radices);
    let lines = strided_lines(n, radices, h);
    let bindings = if in_place {
        "@group(0) @binding(0) var<storage, read_write> dst: array<vec2<f32>>;\n\
         @group(0) @binding(2) var<storage, read> tw: array<vec2<f32>>;\n"
    } else {
        "@group(0) @binding(0) var<storage, read_write> dst: array<vec2<f32>>;\n\
         @group(0) @binding(1) var<storage, read> src: array<vec2<f32>>;\n\
         @group(0) @binding(2) var<storage, read> tw: array<vec2<f32>>;\n"
    };
    let source = if in_place { "dst" } else { "src" };
    format!(
        "{bindings}var<workgroup> sh: array<vec2<f32>, {shared}>;\n{HELPERS}\
         @compute @workgroup_size({size})\n\
         fn main(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3<u32>) {{\n\
         let l = li % {lines}u;\nlet t = li / {lines}u;\n\
         let a = wg.x * {lines}u + l;\nlet live =a < {h}u;\n\
         let base = a + wg.y * {b_stride}u;\nlet sb = l * {n}u;\n\
         for (var q = 0u; q < {per}u; q = q + 1u) {{\n\
           let e = t + q * {threads}u;\n\
           if (live) {{ sh[sb + e] = {source}[base + e * {e_stride}u]; }}\n\
         }}\nworkgroupBarrier();\n{stages}\
         for (var q = 0u; q < {per}u; q = q + 1u) {{\n\
           let e = t + q * {threads}u;\n\
           if (live) {{ dst[base + e * {e_stride}u] = sh[sb + e]; }}\n\
         }}\n}}\n",
        shared = lines * n,
        size = lines * threads,
        per = n / threads,
        stages = stages(n, radices, inverse),
    )
}

/// Real lines along x → half spectrum. `n` is half the real length; the line
/// is transformed as `n` complex pairs, then split: X[k] = E[k] + W^k O[k]
/// with W = e^{-2πi/2n}, from `tw[n..2n + 1]`.
fn real_forward_x_wgsl(n: u32, radices: &[u32], rows: u32) -> String {
    let (lines, threads) = line_shape(n, radices);
    format!(
        "@group(0) @binding(0) var<storage, read_write> dst: array<vec2<f32>>;\n\
         @group(0) @binding(1) var<storage, read> src: array<vec2<f32>>;\n\
         @group(0) @binding(2) var<storage, read> tw: array<vec2<f32>>;\n\
         var<workgroup> sh: array<vec2<f32>, {shared}>;\n{HELPERS}\
         @compute @workgroup_size({size})\n\
         fn main(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3<u32>) {{\n\
         let t = li % {threads}u;\nlet l = li / {threads}u;\n\
         let row = wg.x * {lines}u + l;\nlet live =row < {rows}u;\nlet sb = l * {n}u;\n\
         for (var q = 0u; q < {per}u; q = q + 1u) {{\n\
           let e = t + q * {threads}u;\n\
           if (live) {{ sh[sb + e] = src[row * {n}u + e]; }}\n\
         }}\nworkgroupBarrier();\n{stages}\
         for (var kk = t; kk <= {n}u; kk = kk + {threads}u) {{\n\
           if (live) {{\n\
             let z = sh[sb + kk % {n}u];\n\
             let zc = conj(sh[sb + ({n}u - kk) % {n}u]);\n\
             let even = (z + zc) * 0.5;\n\
             let d = (z - zc) * 0.5;\n\
             let odd = vec2<f32>(d.y, -d.x);\n\
             dst[row * {h}u + kk] = even + cmul(tw[{n}u + kk], odd);\n\
           }}\n\
         }}\n}}\n",
        shared = lines * n,
        size = lines * threads,
        per = n / threads,
        h = n + 1,
        stages = stages(n, radices, false),
    )
}

/// Half spectrum along x → real lines, the inverse of [`real_forward_x_wgsl`],
/// with the plan's whole 1 / volume scale applied here.
fn real_inverse_x_wgsl(n: u32, radices: &[u32], rows: u32, scale: f64) -> String {
    let (lines, threads) = line_shape(n, radices);
    format!(
        "@group(0) @binding(0) var<storage, read_write> dst: array<vec2<f32>>;\n\
         @group(0) @binding(1) var<storage, read> src: array<vec2<f32>>;\n\
         @group(0) @binding(2) var<storage, read> tw: array<vec2<f32>>;\n\
         var<workgroup> sh: array<vec2<f32>, {shared}>;\n{HELPERS}\
         @compute @workgroup_size({size})\n\
         fn main(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3<u32>) {{\n\
         let t = li % {threads}u;\nlet l = li / {threads}u;\n\
         let row = wg.x * {lines}u + l;\nlet live =row < {rows}u;\nlet sb = l * {n}u;\n\
         for (var kk = t; kk < {n}u; kk = kk + {threads}u) {{\n\
           if (live) {{\n\
             let x = src[row * {h}u + kk];\n\
             let xc = conj(src[row * {h}u + {n}u - kk]);\n\
             let odd = cmul(x - xc, conj(tw[{n}u + kk]));\n\
             sh[sb + kk] = x + xc + vec2<f32>(-odd.y, odd.x);\n\
           }}\n\
         }}\nworkgroupBarrier();\n{stages}\
         for (var q = 0u; q < {per}u; q = q + 1u) {{\n\
           let e = t + q * {threads}u;\n\
           if (live) {{ dst[row * {n}u + e] = sh[sb + e] * {scale}; }}\n\
         }}\n}}\n",
        shared = lines * n,
        size = lines * threads,
        per = n / threads,
        h = n + 1,
        scale = f(scale),
        stages = stages(n, radices, true),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Input,
    Output,
    Scratch,
}

struct Pass {
    pipeline: GpuComputePipeline,
    groups: [u32; 3],
    table: usize,
    src: Slot,
    dst: Slot,
    /// Highest complex index each side touches, checked against the bound
    /// buffers before every encode.
    src_reach: u64,
    dst_reach: u64,
}

/// A compiled compute FFT plan. Build once, encode many times.
pub struct ComputeFft {
    kind: ComputeFftKind,
    shape: [u32; 3],
    passes: Vec<Pass>,
    tables: Vec<GpuBuffer>,
    scratch: Option<GpuBuffer>,
}

impl ComputeFft {
    /// A plan for the real `shape` `[nz, ny, nx]` over `axes` 3 (x, y, z) or 2
    /// (x and y of each z slice). `None` when a transformed length is odd or
    /// has a prime factor above 7.
    pub fn new(device: &GpuDevice, kind: ComputeFftKind, shape: [u32; 3], axes: u32) -> Option<Self> {
        let [nz, ny, nx] = shape;
        if !nx.is_multiple_of(2) || !(axes == 2 || axes == 3) {
            return None;
        }
        let half = nx / 2;
        let h = half + 1;
        let x_radices = radix_plan(half)?;
        let y_radices = radix_plan(ny)?;
        let z_radices = if axes == 3 { Some(radix_plan(nz)?) } else { None };
        let rows = ny * nz;
        let spectrum = u64::from(h) * u64::from(ny) * u64::from(nz);
        let real_pairs = u64::from(half) * u64::from(ny) * u64::from(nz);

        let mut tables = Vec::new();
        let mut table = |n: u32, extra_half: bool| {
            let mut values = twiddles(n, n);
            if extra_half {
                values.extend(twiddles(2 * n, n + 1));
            }
            let buffer = device.create_buffer_shared((values.len() * 8) as u64);
            let ptr = buffer.mapped_ptr().expect("shared buffer is mapped");
            // SAFETY: the buffer was just allocated with room for `values`.
            unsafe { std::ptr::copy_nonoverlapping(values.as_ptr().cast::<u8>(), ptr, values.len() * 8) };
            tables.push(buffer);
            tables.len() - 1
        };
        let x_table = table(half, true);
        let y_table = table(ny, false);
        let z_table = z_radices.as_ref().map(|_| table(nz, false));

        let inverse = kind == ComputeFftKind::HermiteanToReal;
        let strided = |n: u32, radices: &[u32], b_stride: u32, e_stride: u32, count_b: u32, in_place: bool, label: &str| {
            let lines = strided_lines(n, radices, h);
            let source = strided_pass_wgsl(n, radices, inverse, h, b_stride, e_stride, in_place);
            let reach = u64::from(h - 1) + u64::from(count_b - 1) * u64::from(b_stride) + u64::from(n - 1) * u64::from(e_stride) + 1;
            (device.create_compute_pipeline(&source, "main", label), [h.div_ceil(lines), count_b, 1], reach)
        };
        let mut passes = Vec::new();
        let x_lines = line_shape(half, &x_radices).0;
        if !inverse {
            let source = real_forward_x_wgsl(half, &x_radices, rows);
            passes.push(Pass {
                pipeline: device.create_compute_pipeline(&source, "main", "compute_fft x forward"),
                groups: [rows.div_ceil(x_lines), 1, 1],
                table: x_table,
                src: Slot::Input,
                dst: Slot::Output,
                src_reach: real_pairs,
                dst_reach: spectrum,
            });
            let (pipeline, groups, reach) = strided(ny, &y_radices, h * ny, h, nz, true, "compute_fft y forward");
            passes.push(Pass { pipeline, groups, table: y_table, src: Slot::Output, dst: Slot::Output, src_reach: reach, dst_reach: reach });
            if let (Some(radices), Some(table)) = (&z_radices, z_table) {
                let (pipeline, groups, reach) = strided(nz, radices, h, h * ny, ny, true, "compute_fft z forward");
                passes.push(Pass { pipeline, groups, table, src: Slot::Output, dst: Slot::Output, src_reach: reach, dst_reach: reach });
            }
        } else {
            // The input spectrum is read-only: the first pass writes scratch.
            let mut first = true;
            if let (Some(radices), Some(table)) = (&z_radices, z_table) {
                let (pipeline, groups, reach) = strided(nz, radices, h, h * ny, ny, false, "compute_fft z inverse");
                passes.push(Pass { pipeline, groups, table, src: Slot::Input, dst: Slot::Scratch, src_reach: reach, dst_reach: reach });
                first = false;
            }
            let (pipeline, groups, reach) = strided(ny, &y_radices, h * ny, h, nz, !first, "compute_fft y inverse");
            let src = if first { Slot::Input } else { Slot::Scratch };
            passes.push(Pass { pipeline, groups, table: y_table, src, dst: Slot::Scratch, src_reach: reach, dst_reach: reach });
            let volume = f64::from(nx) * f64::from(ny) * if axes == 3 { f64::from(nz) } else { 1.0 };
            let source = real_inverse_x_wgsl(half, &x_radices, rows, 1.0 / volume);
            passes.push(Pass {
                pipeline: device.create_compute_pipeline(&source, "main", "compute_fft x inverse"),
                groups: [rows.div_ceil(x_lines), 1, 1],
                table: x_table,
                src: Slot::Scratch,
                dst: Slot::Output,
                src_reach: spectrum,
                dst_reach: real_pairs,
            });
        }
        let scratch = inverse.then(|| device.create_buffer(spectrum * 8));
        Some(Self { kind, shape, passes, tables, scratch })
    }

    pub fn kind(&self) -> ComputeFftKind {
        self.kind
    }

    /// Half-spectrum size in bytes.
    pub fn spectrum_bytes(&self) -> u64 {
        let [nz, ny, nx] = self.shape;
        u64::from(nx / 2 + 1) * u64::from(ny) * u64::from(nz) * 8
    }

    /// Real lattice size in bytes.
    pub fn real_bytes(&self) -> u64 {
        let [nz, ny, nx] = self.shape;
        u64::from(nx) * u64::from(ny) * u64::from(nz) * 4
    }

    /// Dispatches one transform encodes.
    pub fn dispatch_count(&self) -> usize {
        self.passes.len()
    }

    /// Encode one transform. Panics when a buffer is smaller than the plan
    /// reaches, before anything reaches the GPU.
    pub fn encode(&self, enc: &mut GpuEncoder, input: &GpuBuffer, output: &GpuBuffer) {
        for index in 0..self.passes.len() {
            if index > 0 {
                enc.compute_memory_barrier_buffers();
            }
            self.encode_pass(enc, input, output, index);
        }
    }

    /// Encode pass `index` alone, with no barrier. [`Self::encode`] is the
    /// transform; this exists so a probe can time the passes one by one.
    pub fn encode_pass(&self, enc: &mut GpuEncoder, input: &GpuBuffer, output: &GpuBuffer, index: usize) {
        let buffer = |slot: Slot| match slot {
            Slot::Input => input,
            Slot::Output => output,
            Slot::Scratch => self.scratch.as_ref().expect("an inverse plan owns its scratch"),
        };
        let pass = &self.passes[index];
        let (src, dst) = (buffer(pass.src), buffer(pass.dst));
        assert!(src.size >= pass.src_reach * 8, "compute_fft: source of pass {index} is {} bytes, needs {}", src.size, pass.src_reach * 8);
        assert!(dst.size >= pass.dst_reach * 8, "compute_fft: destination of pass {index} is {} bytes, needs {}", dst.size, pass.dst_reach * 8);
        let table = &self.tables[pass.table];
        let mut bindings = vec![
            GpuBinding::Buffer { binding: 0, buffer: dst, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: table, offset: 0 },
        ];
        if pass.src != pass.dst {
            bindings.push(GpuBinding::Buffer { binding: 1, buffer: src, offset: 0 });
        }
        enc.dispatch_compute(&pass.pipeline, &bindings, pass.groups, &pass.pipeline.label);
    }

    /// Each pass's label and threadgroup grid, for probes.
    pub fn pass_shapes(&self) -> Vec<(String, [u32; 3])> {
        self.passes.iter().map(|p| (p.pipeline.label.clone(), p.groups)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radix_plans_cover_even_sides_and_refuse_large_primes() {
        assert_eq!(radix_plan(64), Some(vec![8, 8]));
        assert_eq!(radix_plan(128), Some(vec![8, 4, 4]));
        assert_eq!(radix_plan(96), Some(vec![8, 4, 3]));
        assert_eq!(radix_plan(48), Some(vec![4, 4, 3]));
        assert_eq!(radix_plan(2), Some(vec![2]));
        assert_eq!(radix_plan(80), Some(vec![4, 4, 5]));
        assert_eq!(radix_plan(112), Some(vec![4, 4, 7]));
        assert_eq!(radix_plan(22), None);
        for n in [2u32, 4, 6, 8, 12, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 256, 1024] {
            let radices = radix_plan(n).expect("plan");
            assert_eq!(radices.iter().product::<u32>(), n, "{n}: {radices:?}");
        }
    }

    /// Direct f64 DFT of a real `[nz, ny, nx]` lattice over the given axes,
    /// half spectrum along x.
    fn reference(values: &[f32], [nz, ny, nx]: [usize; 3], axes: u32) -> Vec<[f64; 2]> {
        let h = nx / 2 + 1;
        let mut out = vec![[0.0; 2]; h * ny * nz];
        let tau = 2.0 * std::f64::consts::PI;
        for kz in 0..nz {
            for ky in 0..ny {
                for kx in 0..h {
                    let mut sum = [0.0f64; 2];
                    for z in 0..nz {
                        if axes == 2 && z != kz {
                            continue;
                        }
                        for y in 0..ny {
                            for x in 0..nx {
                                let v = f64::from(values[x + nx * (y + ny * z)]);
                                let mut phase = kx as f64 * x as f64 / nx as f64 + ky as f64 * y as f64 / ny as f64;
                                if axes == 3 {
                                    phase += kz as f64 * z as f64 / nz as f64;
                                }
                                sum[0] += v * (tau * phase).cos();
                                sum[1] -= v * (tau * phase).sin();
                            }
                        }
                    }
                    out[kx + h * (ky + ny * kz)] = sum;
                }
            }
        }
        out
    }

    fn upload(device: &GpuDevice, values: &[f32]) -> GpuBuffer {
        let buf = device.create_buffer_shared((values.len() * 4) as u64);
        let ptr = buf.mapped_ptr().expect("shared buffer is mapped");
        // SAFETY: shared buffer sized for `values`; no GPU work in flight.
        unsafe { std::slice::from_raw_parts_mut(ptr.cast::<f32>(), values.len()).copy_from_slice(values) };
        buf
    }

    fn download(buf: &GpuBuffer, count: usize) -> Vec<f32> {
        let ptr = buf.mapped_ptr().expect("shared buffer is mapped");
        // SAFETY: shared buffer holding `count` floats; GPU work done.
        unsafe { std::slice::from_raw_parts(ptr.cast::<f32>(), count) }.to_vec()
    }

    fn lattice(len: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    /// Forward against a direct f64 DFT and the round trip, for mixed radices
    /// on every axis and both axes modes.
    #[test]
    #[cfg(target_os = "macos")]
    fn compute_fft_matches_direct_dft_and_round_trips() {
        let device = GpuDevice::new();
        for (shape, axes) in [([6u32, 12, 10], 3u32), ([4, 16, 24], 3), ([10, 14, 20], 3), ([3, 12, 16], 2), ([8, 24, 40], 3)] {
            let dims = [shape[0] as usize, shape[1] as usize, shape[2] as usize];
            let count = dims.iter().product::<usize>();
            let values = lattice(count, shape.iter().sum());
            let forward = ComputeFft::new(&device, ComputeFftKind::RealToHermitean, shape, axes).expect("forward plan");
            let inverse = ComputeFft::new(&device, ComputeFftKind::HermiteanToReal, shape, axes).expect("inverse plan");
            let input = upload(&device, &values);
            let spectrum = device.create_buffer_shared(forward.spectrum_bytes());
            let back = device.create_buffer_shared(forward.real_bytes());
            let mut enc = device.create_encoder("compute-fft-test");
            forward.encode(&mut enc, &input, &spectrum);
            enc.compute_memory_barrier_buffers();
            inverse.encode(&mut enc, &spectrum, &back);
            enc.commit_and_wait_completed();

            let got = download(&spectrum, (forward.spectrum_bytes() / 4) as usize);
            let want = reference(&values, dims, axes);
            let scale = want.iter().map(|c| c[0].hypot(c[1])).fold(0.0, f64::max);
            let worst = want
                .iter()
                .enumerate()
                .map(|(i, c)| (f64::from(got[2 * i]) - c[0]).hypot(f64::from(got[2 * i + 1]) - c[1]))
                .fold(0.0, f64::max);
            assert!(worst / scale < 1e-5, "{shape:?} axes {axes}: forward error {worst} against peak {scale}");

            let round = download(&back, count);
            let err = round.iter().zip(&values).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(err < 1e-5, "{shape:?} axes {axes}: round trip error {err}");
            // The inverse leaves its input alone.
            assert_eq!(download(&spectrum, got.len()), got, "{shape:?}: inverse wrote its input");
        }
    }
}
