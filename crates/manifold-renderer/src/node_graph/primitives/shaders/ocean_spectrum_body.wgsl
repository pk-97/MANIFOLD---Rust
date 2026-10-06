// node.ocean_spectrum — fusable BUFFER body, SOURCE (no array inputs).
// One complex value of one of six ocean fields at one wavevector, at time t
// (docs/OCEAN_SURFACE_DESIGN.md section 3.1): JONSWAP × Donelan-Banner,
// h(k,t) = h0(k) e^{-iωt} + conj(h0(-k)) e^{+iωt}, pre-scaled by N² so the
// 1/N²-scaled inverse FFT returns Σ h(k) e^{ik·x}.
//
// ABI (buffer standalone codegen, source shape): (idx, count, params in
// PARAMS order) → Element { x: re, y: im }, written to buf_spectrum[idx].
// Layout: idx = (f·N + m)·(N/2+1) + n; column n is kx ≥ 0, row m is kz
// (signed, wrapped). Int params arrive as i32.

const OSP_G: f32 = 9.81;
const OSP_PI: f32 = 3.14159265358979;

fn osp_pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

// Two independent standard normals for the signed lattice index (ix, iz).
fn osp_gauss(seed: u32, ix: i32, iz: i32) -> vec2<f32> {
    let a = osp_pcg(seed ^ 0x9e3779b9u);
    let b = osp_pcg(a ^ bitcast<u32>(ix));
    let c = osp_pcg(b ^ (bitcast<u32>(iz) * 0x85ebca6bu));
    let d = osp_pcg(c);
    let u1 = (f32(c >> 8u) + 0.5) / 16777216.0;
    let u2 = (f32(d >> 8u) + 0.5) / 16777216.0;
    let r = sqrt(-2.0 * log(u1));
    return vec2<f32>(r * cos(2.0 * OSP_PI * u2), r * sin(2.0 * OSP_PI * u2));
}

// √(S_k)·Δk at wavevector k (rad/m); 0 outside [band_low, band_high).
fn osp_amplitude(k: vec2<f32>, dk: f32, band_low: f32, band_high: f32, wind_speed: f32, wind_dir: f32, fetch_m: f32) -> f32 {
    let kl = length(k);
    if kl < 1e-6 || kl < band_low || (band_high > 0.0 && kl >= band_high) {
        return 0.0;
    }
    let u = max(wind_speed, 0.1);
    let omega = sqrt(OSP_G * kl);
    let alpha = 0.076 * pow(u * u / (fetch_m * OSP_G), 0.22);
    let wp = 22.0 * pow(OSP_G * OSP_G / (u * fetch_m), 1.0 / 3.0);
    let sigma = select(0.09, 0.07, omega <= wp);
    let r = exp(-(omega - wp) * (omega - wp) / (2.0 * sigma * sigma * wp * wp));
    let s_omega = alpha * OSP_G * OSP_G / pow(omega, 5.0) * exp(-1.25 * pow(wp / omega, 4.0)) * pow(3.3, r);
    let rho = omega / wp;
    var beta: f32;
    if rho < 0.95 {
        beta = 2.61 * pow(rho, 1.3);
    } else if rho < 1.6 {
        beta = 2.28 * pow(rho, -1.3);
    } else {
        beta = pow(10.0, -0.4 + 0.8393 * exp(-0.567 * log(rho * rho)));
    }
    var theta = atan2(k.y, k.x) - wind_dir;
    theta = theta - 2.0 * OSP_PI * floor((theta + OSP_PI) / (2.0 * OSP_PI));
    var spread = 0.5 / OSP_PI;
    if beta >= 1e-4 {
        let ch = cosh(beta * theta);
        spread = beta / (2.0 * tanh(beta * OSP_PI)) / (ch * ch);
    }
    let s_k = s_omega * spread * (OSP_G / (2.0 * omega)) / kl;
    return sqrt(s_k) * dk;
}

fn osp_cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn body(
    idx: u32,
    count: u32,
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
) -> Element {
    let n = u32(size);
    let h = n / 2u + 1u;
    let plane = n * h;
    let f = idx / plane;
    let rem = idx % plane;
    let m = i32(rem / h);
    let half_n = i32(n / 2u);
    let ix = i32(rem % h);
    let iz = select(m, m - i32(n), m >= half_n);
    if f >= 6u || ix == half_n || iz == -half_n || (ix == 0 && iz == 0) {
        return Element(0.0, 0.0);
    }
    let dk = 2.0 * OSP_PI / max(tile_size, 1e-3);
    let k = vec2<f32>(f32(ix), f32(iz)) * dk;
    let wd = radians(wind_direction);
    let fetch_m = 1000.0 * max(fetch_km, 0.001);
    let s = bitcast<u32>(seed);
    let h0 = osp_gauss(s, ix, iz) * (0.5 * osp_amplitude(k, dk, band_low, band_high, wind_speed, wd, fetch_m));
    let h0n = osp_gauss(s, -ix, -iz) * (0.5 * osp_amplitude(-k, dk, band_low, band_high, wind_speed, wd, fetch_m));
    let kl = length(k);
    let ph = sqrt(OSP_G * kl) * time * swell_speed;
    let e = vec2<f32>(cos(ph), -sin(ph));
    let hk = (osp_cmul(h0, e) + osp_cmul(vec2<f32>(h0n.x, -h0n.y), vec2<f32>(e.x, -e.y)))
        * (wave_size * f32(n) * f32(n));
    let ih = vec2<f32>(-hk.y, hk.x);
    var out = hk;
    switch f {
        case 1u: { out = ih * (k.x / kl); }
        case 2u: { out = ih * (k.y / kl); }
        case 3u: { out = -hk * (k.x * k.x / kl); }
        case 4u: { out = -hk * (k.y * k.y / kl); }
        case 5u: { out = -hk * (k.x * k.y / kl); }
        default: {}
    }
    return Element(out.x, out.y);
}
