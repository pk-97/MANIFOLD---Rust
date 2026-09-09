// Live Water — shared solver constants and helpers (S4 GPU stages).
//
// Single source of the solver math, mirrored from
// crates/manifold-renderer/src/node_graph/water.rs (S1). The codegen bodies
// prepend this via `wgsl_includes`; the hand-authored atomic/validate kernels
// concatenate it ahead of their own source. `water_domain_constants_match`
// (seed_water lib test) fails on drift between this file and the S1 constants.
//
// Invariants carried here (design docs/WATER_SIMULATION_DESIGN.md section 5):
// - Quadratic B-spline weights partition unity; fractions f in [0.5, 1.5).
// - Grid accumulation is signed fixed-point Q = 2^20, round-to-nearest,
//   checked compare/exchange adds only — a wrapped atomicAdd is never data.
// - Quantisation faults (NaN, ±inf, float-to-int overflow) and checked-add
//   overflow/retries-exhausted all stick FAULT_INTEGER_OVERFLOW and retain the
//   last representable cell value.

const WATER_GRID_N: u32 = 64u;            // nodes per axis, 64^3 grid
const WATER_H: f32 = 0.0625;              // grid spacing h, metres
const WATER_INV_H: f32 = 16.0;            // 1 / h
const WATER_ORIGIN: vec3<f32> = vec3<f32>(-2.0, 0.0, -2.0);
const WATER_FIXED_SCALE: f32 = 1048576.0; // Q = 2^20
const WATER_RHO0: f32 = 1000.0;           // rest density kg/m^3
const WATER_C0_SQ_OVER_7: f32 = 100000.0 / 7.0; // rho0 * c0^2 / 7
const WATER_MU: f32 = 0.001;              // dynamic viscosity Pa*s
// Bounded CAS attempts per contribution. Sized against worst-case per-cell
// contention: a cell in a dense pool is written by ~k = 16-32 particles per
// substep, each CAS attempt wins the cell with probability >= 1/k, so
// P(2048 consecutive losses) <= e^(-2048/32) = e^-64 — never observed across
// a show's ~1e11 contributions, while staying a hard bound with sticky-fault
// evidence rather than an unbounded spin.
const WATER_CAS_RETRIES: u32 = 2048u;

const WATER_FAULT_NONFINITE: u32 = 1u;
const WATER_FAULT_INTEGER_OVERFLOW: u32 = 2u;
const WATER_FAULT_OUTSIDE_DOMAIN: u32 = 4u;
const WATER_FAULT_UNSUPPORTED_KINEMATICS: u32 = 8u;
const WATER_FAULT_INVALID_DENSITY: u32 = 16u;

// One-axis quadratic B-spline weights for fraction f = q - base (S1
// bspline_weights). f lies in [0.5, 1.5) for in-grid particles.
fn water_weights(f: f32) -> vec3<f32> {
    let a = 1.5 - f;
    let b = f - 1.0;
    let c = f - 0.5;
    return vec3<f32>(0.5 * a * a, 0.75 - b * b, 0.5 * c * c);
}

// Bit-level finite test: exponent field 0xFF means NaN or ±inf. This is the
// fast-math-safe form — a NaN-satisfies-neither-comparison check can be
// compiled away (Metal fast math assumes no NaNs), the exponent mask cannot.
fn water_finite1(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

fn water_finite3(v: vec3<f32>) -> bool {
    return water_finite1(v.x) && water_finite1(v.y) && water_finite1(v.z);
}

// Round-to-nearest fixed-point quantisation (S1 quantise: ties away from
// zero, matching Rust f64::round). Finiteness is tested at the bit level
// first; the range check then runs on a known-finite value, so NaN can never
// reach the f32 -> i32 conversion (WGSL conversion of NaN/inf is
// indeterminate).
//
// The rounding is explicit (floor + fraction compare), NOT WGSL `round()`:
// under Metal fast math `round` may compile to rint() and resolve exact ties
// half-to-even, which drifts the Q=2^20 encoding from the S1 contract — a
// lattice contribution of exactly 62.5 quanta must round to 63, not 62.
fn water_quantise(v: f32, ok: ptr<function, bool>) -> i32 {
    var scaled = v * WATER_FIXED_SCALE;
    if (!water_finite1(scaled) || scaled <= -2147483648.0 || scaled >= 2147483648.0) {
        *ok = false;
        return 0;
    }
    var sign = 1.0;
    if (scaled < 0.0) {
        sign = -1.0;
        scaled = -scaled;
    }
    let r = floor(scaled);
    // scaled - r is exact (fraction of the floor), so the 0.5 tie compares
    // true and rounds away from zero, on both polarities.
    let up = select(0.0, 1.0, scaled - r >= 0.5);
    return i32(sign * (r + up));
}

// Signed i32 wrap detection for a + b without computing a + b first.
fn water_add_overflows(a: i32, b: i32) -> bool {
    return (b > 0 && a > 2147483647 - b) || (b < 0 && a < (-2147483647 - 1) - b);
}

// Stencil base node per axis for normalised coordinate q (S1 stencil_base_frac).
fn water_stencil_base(q: vec3<f32>) -> vec3<i32> {
    return vec3<i32>(floor(q - vec3<f32>(0.5)));
}

// True when the 27-node stencil starting at base is fully inside the grid
// (S1 stencil_contained). Anything else faults — stencil mass is never
// silently clipped at an edge.
fn water_stencil_contained(base: vec3<i32>) -> bool {
    let n = i32(WATER_GRID_N);
    return base.x >= 0 && base.y >= 0 && base.z >= 0
        && base.x + 2 < n && base.y + 2 < n && base.z + 2 < n;
}

// Conversion guard: keeps the f32 -> i32 base conversion in range for any
// finite (but absurd) position before the containment check runs. Runs on a
// bit-level finite test for the fast-math reason above.
fn water_q_plausible(q: vec3<f32>) -> bool {
    return water_finite3(q)
        && abs(q.x) <= 1000.0 && abs(q.y) <= 1000.0 && abs(q.z) <= 1000.0;
}

// Flat grid index g = x + nx * (y + ny * z) (S1 grid_index).
fn water_grid_index(x: u32, y: u32, z: u32) -> u32 {
    return x + WATER_GRID_N * (y + WATER_GRID_N * z);
}

// Weakly-compressible EOS p = max(0, rho0*c0^2/7 * ((rho/rho0)^7 - 1))
// (S1 eos_pressure). The exponent is an explicit multiply chain, not pow():
// deterministic rounding, and it matches the f32 host reference.
fn water_eos_pressure(rho: f32) -> f32 {
    let r = rho / WATER_RHO0;
    let r2 = r * r;
    let r3 = r2 * r;
    let r7 = r3 * r3 * r;
    return max(0.0, WATER_C0_SQ_OVER_7 * (r7 - 1.0));
}
