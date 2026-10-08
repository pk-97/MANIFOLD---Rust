//! `node.ocean_spectrum` — a wind-driven ocean spectrum at time t, as the six
//! half spectra `node.inverse_fft_2d` turns into height, sideways displacement
//! and its derivatives (docs/OCEAN_SURFACE_DESIGN.md section 3.1).
//! Tessendorf 2001 with a JONSWAP spectrum and Donelan-Banner spreading
//! (Horvath 2015).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// The six fields, in spectrum order: Dy, Dx, Dz, Dxx, Dzz, Dxz.
pub const OCEAN_FIELDS: u32 = 6;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpectrumUniforms {
    size: i32,
    tile_size: f32,
    band_low: f32,
    band_high: f32,
    wind_speed: f32,
    wind_direction: f32,
    fetch_km: f32,
    wave_size: f32,
    swell_speed: f32,
    seed: i32,
    time: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: OceanSpectrum,
    type_id: "node.ocean_spectrum",
    purpose: "One ocean wave cascade's spectrum at time t: JONSWAP from wind speed and fetch, Donelan-Banner spreading around the wind direction, deep-water dispersion, random phases from a seed. Writes six half spectra (height Dy, sideways Dx and Dz, and their slopes Dxx, Dzz, Dxz) of N rows by N/2+1 columns for node.inverse_fft_2d, holding only wavenumbers in [band_low, band_high) so cascades never double-count.",
    inputs: {
        wind_speed: ScalarF32 optional,
        wind_direction: ScalarF32 optional,
        wave_size: ScalarF32 optional,
        swell_speed: ScalarF32 optional,
        time: ScalarF32 optional,
    },
    outputs: {
        spectrum: Array([f32; 2]),
    },
    params: [
        ParamDef { name: Cow::Borrowed("size"), label: "Size", ty: ParamType::Int, default: ParamValue::Float(256.0), range: Some((16.0, 1024.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tile_size"), label: "Tile Size", ty: ParamType::Float, default: ParamValue::Float(167.0), range: Some((1.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("band_low"), label: "Band Low", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("band_high"), label: "Band High", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wind_speed"), label: "Wind Speed", ty: ParamType::Float, default: ParamValue::Float(10.0), range: Some((0.1, 40.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wind_direction"), label: "Wind Direction", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-360.0, 360.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fetch_km"), label: "Fetch (km)", ty: ParamType::Float, default: ParamValue::Float(300.0), range: Some((0.1, 5000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wave_size"), label: "Wave Size", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("swell_speed"), label: "Swell Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("seed"), label: "Seed", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((0.0, 65535.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("time"), label: "Time", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 100000.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire `spectrum` into node.inverse_fft_2d with the same Size and Batch 6, and its `field` into node.ocean_displace. For cascades, use three spectra with tile sizes about 6 apart (1000, 167, 27 m) and band edges at 6·2π/L of the next finer tile, so each wavelength lives in one cascade. Wind Speed is m/s, Wind Direction degrees from +X toward +Z, Swell Speed scales time. Leave `time` unwired for the playback clock.",
    examples: ["Ocean"],
    picker: { label: "Ocean Spectrum", category: Atom },
    summary: "Makes the wave spectrum of a wind-driven sea for one band of wave sizes, the starting point of a spectral ocean.",
    category: Geometry3D,
    role: Source,
    aliases: ["ocean", "tessendorf", "jonswap", "wave spectrum", "sea"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/ocean_spectrum_body.wgsl"),
    frame_time_inputs: ["time"],
}

// A fused region's per-frame `time` field: the playback clock, as `run()`
// uses when `time` is unwired.
inventory::submit! {
    manifold_node_engine::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.ocean_spectrum",
        array_ports: &[],
        recompute: |ctx| Some(vec![ctx.frame.seconds.0 as f32]),
    }
}

/// The spectrum's size N when it is a power of two in 16..=1024.
fn valid_size(size: f32) -> Option<u32> {
    let n = size.round() as u32;
    ((16.0..=1024.0).contains(&size) && n.is_power_of_two()).then_some(n)
}

/// Complex values in one cascade's spectrum.
pub fn spectrum_len(n: u32) -> u32 {
    OCEAN_FIELDS * n * (n / 2 + 1)
}

fn param_f32(params: &manifold_node_engine::exec::effect_node::ParamValues, name: &str, default: f32) -> f32 {
    match params.get(name) {
        Some(ParamValue::Float(v)) => *v,
        _ => default,
    }
}

impl Primitive for OceanSpectrum {
    fn array_output_capacity(
        &self,
        port: &str,
        params: &manifold_node_engine::exec::effect_node::ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "spectrum").then(|| valid_size(param_f32(params, "size", 256.0))).flatten().map(spectrum_len)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let size = param_f32(ctx.params, "size", 256.0);
        let Some(n) = valid_size(size) else {
            ctx.error(format!("Ocean Spectrum: Size must be a power of two in 16..1024 (got {size})"));
            return;
        };
        let time = match ctx.inputs.scalar("time") {
            Some(ParamValue::Float(t)) => t,
            _ => ctx.time.seconds.0 as f32,
        };
        let uniforms = SpectrumUniforms {
            size: n as i32,
            tile_size: param_f32(ctx.params, "tile_size", 167.0),
            band_low: param_f32(ctx.params, "band_low", 0.0),
            band_high: param_f32(ctx.params, "band_high", 0.0),
            wind_speed: ctx.scalar_or_param("wind_speed", 10.0),
            wind_direction: ctx.scalar_or_param("wind_direction", 0.0),
            fetch_km: param_f32(ctx.params, "fetch_km", 300.0),
            wave_size: ctx.scalar_or_param("wave_size", 1.0),
            swell_speed: ctx.scalar_or_param("swell_speed", 1.0),
            seed: param_f32(ctx.params, "seed", 1.0).round() as i32,
            time,
            dispatch_count: spectrum_len(n),
        };
        let Some(out) = ctx.outputs.array("spectrum") else {
            return;
        };
        if out.size < u64::from(uniforms.dispatch_count) * 8 {
            ctx.error("Ocean Spectrum: output buffer smaller than the spectrum");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: out, offset: 0 },
            ],
            [uniforms.dispatch_count.div_ceil(256), 1, 1],
            "node.ocean_spectrum",
        );
    }
}

/// CPU reference of the WGSL body, in f64 except the hash, for the proofs.
#[cfg(test)]
pub(crate) mod reference {
    pub const G: f64 = 9.81;

    #[derive(Clone, Copy)]
    pub struct Spectrum {
        pub n: u32,
        pub tile_size: f64,
        pub band_low: f64,
        pub band_high: f64,
        pub wind_speed: f64,
        pub wind_direction_deg: f64,
        pub fetch_km: f64,
        pub wave_size: f64,
        pub swell_speed: f64,
        pub seed: u32,
    }

    fn pcg(v: u32) -> u32 {
        let s = v.wrapping_mul(747796405).wrapping_add(2891336453);
        let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277803737);
        (w >> 22) ^ w
    }

    pub fn gauss(seed: u32, ix: i32, iz: i32) -> (f64, f64) {
        let a = pcg(seed ^ 0x9e37_79b9);
        let b = pcg(a ^ ix as u32);
        let c = pcg(b ^ (iz as u32).wrapping_mul(0x85eb_ca6b));
        let d = pcg(c);
        let u1 = (f64::from(c >> 8) + 0.5) / 16_777_216.0;
        let u2 = (f64::from(d >> 8) + 0.5) / 16_777_216.0;
        let r = (-2.0 * u1.ln()).sqrt();
        (r * (std::f64::consts::TAU * u2).cos(), r * (std::f64::consts::TAU * u2).sin())
    }

    /// The directional spectrum density S_k at wavevector k, before the band cut.
    pub fn density(s: &Spectrum, kx: f64, kz: f64) -> f64 {
        let k = (kx * kx + kz * kz).sqrt();
        let u = s.wind_speed.max(0.1);
        let fetch = 1000.0 * s.fetch_km.max(0.001);
        let omega = (G * k).sqrt();
        let alpha = 0.076 * (u * u / (fetch * G)).powf(0.22);
        let wp = 22.0 * (G * G / (u * fetch)).powf(1.0 / 3.0);
        let sigma = if omega <= wp { 0.07 } else { 0.09 };
        let r = (-(omega - wp).powi(2) / (2.0 * sigma * sigma * wp * wp)).exp();
        let s_omega = alpha * G * G / omega.powi(5) * (-1.25 * (wp / omega).powi(4)).exp() * 3.3f64.powf(r);
        let rho = omega / wp;
        let beta = if rho < 0.95 {
            2.61 * rho.powf(1.3)
        } else if rho < 1.6 {
            2.28 * rho.powf(-1.3)
        } else {
            10f64.powf(-0.4 + 0.8393 * (-0.567 * (rho * rho).ln()).exp())
        };
        let pi = std::f64::consts::PI;
        let mut theta = kz.atan2(kx) - s.wind_direction_deg.to_radians();
        theta -= 2.0 * pi * ((theta + pi) / (2.0 * pi)).floor();
        let spread = if beta < 1e-4 { 0.5 / pi } else { beta / (2.0 * (beta * pi).tanh()) / (beta * theta).cosh().powi(2) };
        s_omega * spread * (G / (2.0 * omega)) / k
    }

    pub fn in_band(s: &Spectrum, k: f64) -> bool {
        k >= 1e-6 && k >= s.band_low && (s.band_high <= 0.0 || k < s.band_high)
    }

    /// h0 at signed index (ix, iz).
    pub fn h0(s: &Spectrum, ix: i32, iz: i32) -> (f64, f64) {
        let dk = std::f64::consts::TAU / s.tile_size;
        let (kx, kz) = (f64::from(ix) * dk, f64::from(iz) * dk);
        let k = (kx * kx + kz * kz).sqrt();
        if !in_band(s, k) {
            return (0.0, 0.0);
        }
        let a = 0.5 * density(s, kx, kz).sqrt() * dk;
        let (g1, g2) = gauss(s.seed, ix, iz);
        (g1 * a, g2 * a)
    }

    fn mul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
        (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
    }

    /// h(k,t) at signed index, without the N² pre-scale.
    pub fn h(s: &Spectrum, ix: i32, iz: i32, t: f64) -> (f64, f64) {
        let dk = std::f64::consts::TAU / s.tile_size;
        let k = (f64::from(ix).powi(2) + f64::from(iz).powi(2)).sqrt() * dk;
        let ph = (G * k).sqrt() * t * s.swell_speed;
        let e = (ph.cos(), -ph.sin());
        let a = mul(h0(s, ix, iz), e);
        let neg = h0(s, -ix, -iz);
        let b = mul((neg.0, -neg.1), (e.0, -e.1));
        ((a.0 + b.0) * s.wave_size, (a.1 + b.1) * s.wave_size)
    }

    /// Field `f`'s spectrum value at signed index, without the N² pre-scale;
    /// zero on the Nyquist row and column and at k = 0, as the body.
    pub fn field(s: &Spectrum, f: u32, ix: i32, iz: i32, t: f64) -> (f64, f64) {
        let half = (s.n / 2) as i32;
        if ix.abs() == half || iz.abs() == half || (ix == 0 && iz == 0) {
            return (0.0, 0.0);
        }
        let dk = std::f64::consts::TAU / s.tile_size;
        let (kx, kz) = (f64::from(ix) * dk, f64::from(iz) * dk);
        let k = (kx * kx + kz * kz).sqrt();
        let hk = h(s, ix, iz, t);
        let ih = (-hk.1, hk.0);
        let scale = |c: (f64, f64), v: f64| (c.0 * v, c.1 * v);
        match f {
            0 => hk,
            1 => scale(ih, kx / k),
            2 => scale(ih, kz / k),
            3 => scale(hk, -kx * kx / k),
            4 => scale(hk, -kz * kz / k),
            _ => scale(hk, -kx * kz / k),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::reference::*;

    fn sea(n: u32, seed: u32) -> Spectrum {
        Spectrum {
            n,
            tile_size: 167.0,
            band_low: 0.0,
            band_high: 0.0,
            wind_speed: 10.0,
            wind_direction_deg: 0.0,
            fetch_km: 300.0,
            wave_size: 1.0,
            swell_speed: 1.0,
            seed,
        }
    }

    /// Invariant 2 (docs/OCEAN_SURFACE_DESIGN.md section 4): by Parseval the
    /// field's variance is Σ|h(k)|², which must average to Σ S_k Δk². A
    /// 1 km tile spreads the peak over hundreds of modes, so 64 seeds pin the
    /// mean to about 1%.
    #[test]
    fn field_variance_matches_jonswap() {
        let n = 64;
        let half = (n / 2) as i32;
        let base = Spectrum { tile_size: 1000.0, ..sea(n, 0) };
        let dk = std::f64::consts::TAU / base.tile_size;
        let mut expected = 0.0;
        for iz in -half + 1..half {
            for ix in -half + 1..half {
                let (kx, kz) = (f64::from(ix) * dk, f64::from(iz) * dk);
                if in_band(&base, (kx * kx + kz * kz).sqrt()) {
                    expected += density(&base, kx, kz) * dk * dk;
                }
            }
        }
        let seeds = 64;
        let mut measured = 0.0;
        for seed in 0..seeds {
            let s = Spectrum { seed, ..base };
            for iz in -half + 1..half {
                for ix in -half + 1..half {
                    let (re, im) = field(&s, 0, ix, iz, 3.0);
                    measured += re * re + im * im;
                }
            }
        }
        measured /= f64::from(seeds);
        let ratio = measured / expected;
        assert!((ratio - 1.0).abs() < 0.05, "variance ratio {ratio} (measured {measured}, JONSWAP {expected})");
    }

    /// Invariant 3: one wave along +x sharpens its crest and runs downwind.
    #[test]
    fn single_wave_runs_downwind_and_sharpens() {
        let n = 16;
        let s = sea(n, 7);
        let (ix, iz) = (2, 0);
        // Height and Dx of just the ±k pair, by direct sum over the pair.
        let dk = std::f64::consts::TAU / s.tile_size;
        let k = f64::from(ix) * dk;
        let pair = |f: u32, x: f64, t: f64| {
            [(ix, iz), (-ix, -iz)].iter().map(|&(a, b)| {
                let (re, im) = field(&s, f, a, b, t);
                let ph = dk * (f64::from(a) * x);
                re * ph.cos() - im * ph.sin()
            }).sum::<f64>()
        };
        // Find the crest at t = 0, then check Dx vanishes there with negative slope.
        let samples = 4096;
        let wavelength = std::f64::consts::TAU / k;
        let crest = (0..samples)
            .map(|i| f64::from(i) / f64::from(samples) * wavelength)
            .max_by(|a, b| pair(0, *a, 0.0).total_cmp(&pair(0, *b, 0.0)))
            .unwrap();
        let amp = pair(0, crest, 0.0);
        assert!(amp > 0.0);
        assert!(pair(1, crest, 0.0).abs() < 1e-3 * amp, "Dx at the crest {}", pair(1, crest, 0.0));
        let dxx = pair(3, crest, 0.0);
        assert!((dxx + amp * k).abs() < 1e-3 * amp * k, "Dxx at crest {dxx}, expected {}", -amp * k);
        // A tenth of a period later the crest has moved toward +x.
        let omega = (G * k).sqrt();
        let t = 0.1 * std::f64::consts::TAU / omega;
        let moved = (0..samples)
            .map(|i| crest + (f64::from(i) / f64::from(samples) - 0.5) * wavelength)
            .max_by(|a, b| pair(0, *a, t).total_cmp(&pair(0, *b, t)))
            .unwrap();
        let expected = crest + omega * t / k;
        assert!((moved - expected).abs() < wavelength / 200.0, "crest moved to {moved}, expected {expected}");
    }

    /// Invariant 4: the preset's three bands partition every wavenumber.
    #[test]
    fn cascade_bands_partition_k() {
        let tiles = [1000.0f64, 167.0, 27.0];
        let edge = |l: f64| 6.0 * std::f64::consts::TAU / l;
        let bands = [(0.0, edge(tiles[1])), (edge(tiles[1]), edge(tiles[2])), (edge(tiles[2]), 0.0)];
        for i in 1..200_000 {
            let k = f64::from(i) * 1e-3;
            let hits = bands
                .iter()
                .filter(|&&(lo, hi)| {
                    let s = Spectrum { band_low: lo, band_high: hi, ..sea(16, 0) };
                    in_band(&s, k)
                })
                .count();
            assert_eq!(hits, 1, "k = {k} is in {hits} bands");
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::reference::*;
    use super::*;

    const N: u32 = 16;

    fn sea() -> Spectrum {
        Spectrum {
            n: N,
            tile_size: 40.0,
            band_low: 0.0,
            band_high: 0.0,
            wind_speed: 12.0,
            wind_direction_deg: 30.0,
            fetch_km: 200.0,
            wave_size: 1.3,
            swell_speed: 0.8,
            seed: 11,
        }
    }

    fn dispatch(device: &manifold_gpu::GpuDevice, s: &Spectrum, t: f32) -> manifold_gpu::GpuBuffer {
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<OceanSpectrum>().expect("ocean_spectrum codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, manifold_node_engine::freeze::codegen::ENTRY, "ocean-spectrum-test");
        let len = spectrum_len(N);
        let out = device.create_buffer_shared(u64::from(len) * 8);
        let uniforms = SpectrumUniforms {
            size: N as i32,
            tile_size: s.tile_size as f32,
            band_low: s.band_low as f32,
            band_high: s.band_high as f32,
            wind_speed: s.wind_speed as f32,
            wind_direction: s.wind_direction_deg as f32,
            fetch_km: s.fetch_km as f32,
            wave_size: s.wave_size as f32,
            swell_speed: s.swell_speed as f32,
            seed: s.seed as i32,
            time: t,
            dispatch_count: len,
        };
        let mut enc = device.create_encoder("ocean-spectrum-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &out, offset: 0 },
            ],
            [len.div_ceil(256), 1, 1],
            "ocean-spectrum-test",
        );
        enc.commit_and_wait_completed();
        out
    }

    /// The real field `f` at (x, z) by direct sum over the full lattice.
    fn direct_sum(s: &Spectrum, f: u32, x: f64, z: f64, t: f64) -> f64 {
        let half = (s.n / 2) as i32;
        let dk = std::f64::consts::TAU / s.tile_size;
        let mut sum = 0.0;
        for iz in -half..half {
            for ix in -half..half {
                let (re, im) = field(s, f, ix, iz, t);
                let ph = dk * (f64::from(ix) * x + f64::from(iz) * z);
                sum += re * ph.cos() - im * ph.sin();
            }
        }
        sum
    }

    fn read(buf: &manifold_gpu::GpuBuffer, count: usize) -> Vec<f32> {
        let ptr = buf.mapped_ptr().expect("shared buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const f32, count) }.to_vec()
    }

    /// The generated kernel matches the CPU reference value for value.
    #[test]
    fn ocean_spectrum_matches_cpu() {
        let device = manifold_gpu::testkit::test_device();
        let s = sea();
        let t = 2.5;
        let got = read(&dispatch(&device, &s, t as f32), spectrum_len(N) as usize * 2);
        let h = N / 2 + 1;
        let scale = f64::from(N * N);
        let mut peak = 0.0f64;
        let mut worst = 0.0f64;
        for f in 0..OCEAN_FIELDS {
            for m in 0..N {
                for col in 0..h {
                    let iz = if m >= N / 2 { m as i32 - N as i32 } else { m as i32 };
                    let (re, im) = field(&s, f, col as i32, iz, t);
                    let i = (((f * N + m) * h + col) * 2) as usize;
                    peak = peak.max((re * scale).abs()).max((im * scale).abs());
                    worst = worst.max((f64::from(got[i]) - re * scale).abs()).max((f64::from(got[i + 1]) - im * scale).abs());
                }
            }
        }
        assert!(peak > 0.0);
        assert!(worst <= 1e-4 * peak, "worst error {worst} against peak {peak}");
    }

    /// Invariant 1: spectrum → GPU inverse equals the direct sum, and the
    /// kept column 0's ±kz pairs are conjugate.
    #[test]
    fn ocean_field_matches_direct_sum() {
        let device = manifold_gpu::testkit::test_device();
        let s = sea();
        let t = 1.25;
        let spectrum = dispatch(&device, &s, t as f32);
        let h = (N / 2 + 1) as usize;
        let spec = read(&spectrum, spectrum_len(N) as usize * 2);
        for f in 0..OCEAN_FIELDS as usize {
            for m in 1..(N / 2) as usize {
                let a = ((f * N as usize + m) * h) * 2;
                let b = ((f * N as usize + (N as usize - m)) * h) * 2;
                let mag = spec[a].abs().max(spec[a + 1].abs()).max(1e-6);
                assert!((spec[a] - spec[b]).abs() <= 1e-5 * mag && (spec[a + 1] + spec[b + 1]).abs() <= 1e-5 * mag,
                    "field {f} row {m}: column 0 is not conjugate");
            }
        }
        let fft = manifold_gpu::GpuFft::new_nd(&device, manifold_gpu::FftKind::HermiteanToReal, &[6, N as usize, N as usize], &[1, 2]);
        let field = device.create_buffer_shared(fft.output_len_bytes());
        let mut enc = device.create_encoder("ocean-ifft-test");
        fft.encode(&mut enc, &spectrum, &field);
        enc.commit_and_wait_completed();
        let got = read(&field, (6 * N * N) as usize);
        for f in 0..OCEAN_FIELDS {
            let mut peak = 0.0f64;
            let mut worst = 0.0f64;
            for j in 0..N {
                for i in 0..N {
                    let (x, z) = (f64::from(i) * s.tile_size / f64::from(N), f64::from(j) * s.tile_size / f64::from(N));
                    let want = direct_sum(&s, f, x, z, t);
                    let have = f64::from(got[((f * N + j) * N + i) as usize]);
                    peak = peak.max(want.abs());
                    worst = worst.max((have - want).abs());
                }
            }
            assert!(peak > 0.0, "field {f} is empty");
            assert!(worst <= 1e-4 * peak, "field {f}: worst error {worst} against peak {peak}");
        }
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
