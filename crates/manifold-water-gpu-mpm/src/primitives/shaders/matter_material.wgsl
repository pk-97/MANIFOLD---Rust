// Matter's material maths, declared through `wgsl_includes`: one particle's
// elastic deformation F, its plastic state and the Kirchhoff stress they
// give. Binding-free: it reads no grid, record or binding, so it moves
// unchanged onto another solver's grid (MATTER_SAND_DESIGN.md section 2).
// Matrices are mathematical: m[c][r] is row r of column c.

// The rules a material's points follow (`kind`): granular (sand and soil,
// Drucker-Prager), snow (Stomakhin et al. 2013), viscoplastic (clay, mud and
// goo, von Mises with a viscous overstress) and elastic (jelly).
const MATTER_GRANULAR: u32 = 0u;
const MATTER_SNOW: u32 = 1u;
const MATTER_VISCOPLASTIC: u32 = 2u;
const MATTER_ELASTIC: u32 = 3u;

// Snow's critical compression and stretch, θc and θs: beyond them a stretch
// is plastic (Stomakhin et al. 2013, Table 2).
const MM_SNOW_COMPRESS: f32 = 2.5e-2;
const MM_SNOW_STRETCH: f32 = 7.5e-3;
// The hardening factor's bounds, as implementations of the paper hold it, so
// a fully packed or fully loosened point stays stable at the substeps the
// domain budgets (five times the stiffness at most).
const MM_SNOW_HARDENING_MIN: f32 = 0.1;
const MM_SNOW_HARDENING_MAX: f32 = 5.0;

// One material's constants. Sand is cohesion 0; soil is sand with cohesion.
struct MatterMaterial {
    // Lamé shear modulus μ, Pa.
    mu: f32,
    // Lamé λ, Pa.
    lambda: f32,
    // Drucker-Prager friction: √(2/3)·2 sin φ / (3 − sin φ), φ the Friction Angle.
    alpha: f32,
    // β₀: the tensile log strain intact soil carries, from the Cohesion dial.
    cohesion: f32,
    // q_s: the plastic strain over which cohesion falls to 1/e.
    softening: f32,
    // Which rule: MATTER_GRANULAR, MATTER_SNOW, MATTER_VISCOPLASTIC or MATTER_ELASTIC.
    kind: u32,
    // Viscoplastic: the deviatoric log strain it yields at, τ_y / (√2 μ) for
    // a shear yield stress τ_y.
    yield_strain: f32,
    // Viscoplastic: η / μ, seconds: past yield, the strain relaxes toward
    // the yield surface at this time constant, so the stress past yield in
    // steady shear is η times the shear rate (a Bingham fluid). Elastic: η / μ
    // of its Kelvin-Voigt damping, so the stress adds η times the strain rate
    // and a wobble dies away.
    relax_time: f32,
    // Snow: ξ, how fast compaction stiffens it.
    hardening: f32,
}

// What a particle remembers of its plastic history.
struct MatterPlastic {
    // Granular: log volume gained while the grains were pulled apart, paid
    // back before pressure builds again (Tampubolon et al. 2017).
    volume_correction: f32,
    // Granular and viscoplastic: accumulated plastic strain; cohesion fades
    // as it grows. Snow: log Jp, the log of the volume its plastic flow
    // packed or loosened it to, 0 as it fell.
    strain: f32,
}

// F = U·diag(sigma)·Vᵀ with U and V proper rotations, sigma sorted by
// magnitude, sigma.x and sigma.y never negative and sigma.z negative only
// when F is inverted.
struct MatterSvd {
    u: mat3x3<f32>,
    sigma: vec3<f32>,
    v: mat3x3<f32>,
}

struct MatterUpdate {
    f: mat3x3<f32>,
    // Kirchhoff stress τ, symmetric.
    tau: mat3x3<f32>,
    plastic: MatterPlastic,
    // det F.
    j: f32,
}

// McAdams et al. 2011, "Computing the Singular Value Decomposition of 3×3
// matrices with minimal branching and elementary floating point operations":
// Jacobi on FᵀF with approximate Givens rotations gives V, a sort of FV's
// columns by length, and a Givens QR of FV gives U and sigma.
const MM_GAMMA: f32 = 5.828427124; // 3 + 2√2
const MM_COS_PI_8: f32 = 0.9238795325;
const MM_SIN_PI_8: f32 = 0.3826834324;
const MM_EPSILON: f32 = 1.0e-6;
// Four sweeps (the paper's count) leave up to 0.7% error in U·Σ·Vᵀ on
// random F near identity; six reach f32 rounding.
const MM_JACOBI_SWEEPS: u32 = 6u;

// M·G, G the rotation by (c, s) in plane (p, q): column p becomes
// c·col p + s·col q, column q becomes −s·col p + c·col q.
fn mm_rotate_columns(m: mat3x3<f32>, p: u32, q: u32, c: f32, s: f32) -> mat3x3<f32> {
    var out = m;
    out[p] = c * m[p] + s * m[q];
    out[q] = c * m[q] - s * m[p];
    return out;
}

// Gᵀ·M: row p becomes c·row p + s·row q, row q becomes −s·row p + c·row q.
fn mm_rotate_rows(m: mat3x3<f32>, p: u32, q: u32, c: f32, s: f32) -> mat3x3<f32> {
    var out = m;
    for (var j = 0u; j < 3u; j = j + 1u) {
        out[j][p] = c * m[j][p] + s * m[j][q];
        out[j][q] = c * m[j][q] - s * m[j][p];
    }
    return out;
}

// One Jacobi step on symmetric `s` in plane (p, q): the approximate Givens
// half angle, or π/8 where the approximation is poor, as (cos, sin) of the
// full angle.
fn mm_jacobi_angle(app: f32, apq: f32, aqq: f32) -> vec2<f32> {
    let ch = 2.0 * (app - aqq);
    let sh = apq;
    let w = inverseSqrt(ch * ch + sh * sh);
    let exact = MM_GAMMA * sh * sh < ch * ch;
    let h = select(vec2<f32>(MM_COS_PI_8, MM_SIN_PI_8), vec2<f32>(w * ch, w * sh), exact);
    return vec2<f32>(h.x * h.x - h.y * h.y, 2.0 * h.x * h.y);
}

// The Givens rotation (cos, sin) that zeroes a2 under the pivot a1 and leaves
// the pivot at +√(a1² + a2²).
fn mm_qr_angle(a1: f32, a2: f32) -> vec2<f32> {
    let rho = sqrt(a1 * a1 + a2 * a2);
    var sh = select(0.0, a2, rho > MM_EPSILON);
    var ch = abs(a1) + max(rho, MM_EPSILON);
    if a1 < 0.0 {
        let t = sh;
        sh = ch;
        ch = t;
    }
    let w = inverseSqrt(ch * ch + sh * sh);
    ch = ch * w;
    sh = sh * w;
    return vec2<f32>(1.0 - 2.0 * sh * sh, 2.0 * ch * sh);
}

// Swaps columns p and q of b and v when column p of b is shorter, negating
// one so v stays a rotation.
fn mm_sort_pair(b: ptr<function, mat3x3<f32>>, v: ptr<function, mat3x3<f32>>, p: u32, q: u32) {
    if dot((*b)[p], (*b)[p]) < dot((*b)[q], (*b)[q]) {
        let bp = (*b)[p];
        (*b)[p] = (*b)[q];
        (*b)[q] = -bp;
        let vp = (*v)[p];
        (*v)[p] = (*v)[q];
        (*v)[q] = -vp;
    }
}

fn matter_svd(f: mat3x3<f32>) -> MatterSvd {
    var s = transpose(f) * f;
    var v = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    for (var sweep = 0u; sweep < MM_JACOBI_SWEEPS; sweep = sweep + 1u) {
        for (var k = 0u; k < 3u; k = k + 1u) {
            // Planes (0, 1), (1, 2), (0, 2).
            let p = select(k, 0u, k == 2u);
            let q = select(k + 1u, 2u, k == 2u);
            let r = mm_jacobi_angle(s[p][p], s[q][p], s[q][q]);
            s = mm_rotate_rows(mm_rotate_columns(s, p, q, r.x, r.y), p, q, r.x, r.y);
            v = mm_rotate_columns(v, p, q, r.x, r.y);
        }
    }
    var b = f * v;
    mm_sort_pair(&b, &v, 0u, 1u);
    mm_sort_pair(&b, &v, 0u, 2u);
    mm_sort_pair(&b, &v, 1u, 2u);

    var u = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    let r1 = mm_qr_angle(b[0][0], b[0][1]);
    b = mm_rotate_rows(b, 0u, 1u, r1.x, r1.y);
    u = mm_rotate_columns(u, 0u, 1u, r1.x, r1.y);
    let r2 = mm_qr_angle(b[0][0], b[0][2]);
    b = mm_rotate_rows(b, 0u, 2u, r2.x, r2.y);
    u = mm_rotate_columns(u, 0u, 2u, r2.x, r2.y);
    let r3 = mm_qr_angle(b[1][1], b[1][2]);
    b = mm_rotate_rows(b, 1u, 2u, r3.x, r3.y);
    u = mm_rotate_columns(u, 1u, 2u, r3.x, r3.y);
    return MatterSvd(u, vec3<f32>(b[0][0], b[1][1], b[2][2]), v);
}

// U·diag(d)·Uᵀ.
fn mm_symmetric(u: mat3x3<f32>, d: vec3<f32>) -> mat3x3<f32> {
    let ud = mat3x3<f32>(u[0] * d.x, u[1] * d.y, u[2] * d.z);
    return ud * transpose(u);
}

// The cohesion's tensile log strain after `strain` of plastic flow.
fn matter_cohesion(strain: f32, m: MatterMaterial) -> f32 {
    return m.cohesion * exp(-strain / max(m.softening, MM_EPSILON));
}

// Drucker-Prager return map on Hencky strain (Klár et al. 2016), its cone
// tip shifted into tension by the cohesion β. `eps` is the log of the trial
// stretches; the result's xyz is the elastic log strain kept and w the
// plastic strain this step adds. The volume correction counts as stretch
// (eps_c), so whatever volume the map does not keep elastically is carried
// in it; returned through `plastic`.
fn matter_return_map(eps: vec3<f32>, plastic: ptr<function, MatterPlastic>, m: MatterMaterial) -> vec4<f32> {
    let beta = matter_cohesion((*plastic).strain, m);
    let eps_c = eps + vec3<f32>((*plastic).volume_correction / 3.0);
    let tr = eps_c.x + eps_c.y + eps_c.z;
    if tr >= beta {
        // Pulled apart: the grains separate, keeping only what cohesion holds.
        let tip = vec3<f32>(beta / 3.0);
        (*plastic).volume_correction = tr - beta;
        return vec4<f32>(tip, length(eps_c - tip));
    }
    (*plastic).volume_correction = 0.0;
    let dev = eps_c - vec3<f32>(tr / 3.0);
    let n = length(dev);
    let dgamma = n + m.alpha * (3.0 * m.lambda + 2.0 * m.mu) / (2.0 * m.mu) * (tr - beta);
    if dgamma <= 0.0 || n <= 0.0 {
        // Holding: inside the cone.
        return vec4<f32>(eps_c, 0.0);
    }
    // Sliding: the shear past the cone is removed, the volume kept.
    return vec4<f32>(eps_c - (dgamma / n) * dev, dgamma);
}

// Kirchhoff stress of elastic log strain `eps` along U (St. Venant–Kirchhoff
// on Hencky strain): τ = U·diag(2μ·ε + λ·tr ε)·Uᵀ.
fn matter_stress(u: mat3x3<f32>, eps: vec3<f32>, m: MatterMaterial) -> mat3x3<f32> {
    let tr = eps.x + eps.y + eps.z;
    return mm_symmetric(u, 2.0 * m.mu * eps + vec3<f32>(m.lambda * tr));
}

// Elastic energy density of log strain `eps`, J/m³ of rest volume.
fn matter_energy(eps: vec3<f32>, m: MatterMaterial) -> f32 {
    let tr = eps.x + eps.y + eps.z;
    return m.mu * dot(eps, eps) + 0.5 * m.lambda * tr * tr;
}

// The log stretches of F, each held at least 10⁻⁴.
fn matter_log_stretch(sigma: vec3<f32>) -> vec3<f32> {
    return log(max(abs(sigma), vec3<f32>(1.0e-4)));
}

// Von Mises with a viscous overstress (Perzyna) on Hencky strain: past the
// yield radius the deviatoric strain returns toward it by
// Δγ = (‖dev ε‖ − yield) / (1 + relax / dt), all the way when relax is 0
// (rate-independent clay), a dt / relax share of the excess when it is long
// (a thick goo). Volume stays elastic. xyz is the kept log strain, w the
// plastic strain this step adds.
fn matter_viscoplastic_map(eps: vec3<f32>, dt: f32, m: MatterMaterial) -> vec4<f32> {
    let tr = eps.x + eps.y + eps.z;
    let dev = eps - vec3<f32>(tr / 3.0);
    let n = length(dev);
    if n <= m.yield_strain {
        return vec4<f32>(eps, 0.0);
    }
    let dgamma = (n - m.yield_strain) / (1.0 + m.relax_time / max(dt, MM_EPSILON));
    return vec4<f32>(eps - (dgamma / n) * dev, dgamma);
}

// F = U·diag(stretch)·Vᵀ.
fn mm_compose(svd: MatterSvd, stretch: vec3<f32>) -> mat3x3<f32> {
    let us = mat3x3<f32>(svd.u[0] * stretch.x, svd.u[1] * stretch.y, svd.u[2] * stretch.z);
    return us * transpose(svd.v);
}

// Snow (Stomakhin et al. 2013): stretches past θc and θs are plastic and pack
// or loosen the point (log Jp in `strain`); packing stiffens it by
// exp(ξ (1 − Jp)). Fixed-corotated stress: τ = 2μ(F − R)Fᵀ + λ(J − 1)J·I,
// diagonal in U as 2μ(σ − 1)σ + λ(J − 1)J.
fn matter_snow_update(svd: MatterSvd, plastic: MatterPlastic, m: MatterMaterial) -> MatterUpdate {
    let sigma = clamp(svd.sigma, vec3<f32>(1.0 - MM_SNOW_COMPRESS), vec3<f32>(1.0 + MM_SNOW_STRETCH));
    var state = plastic;
    let trial_log = matter_log_stretch(svd.sigma);
    let kept_log = log(sigma);
    state.strain = plastic.strain + (trial_log.x + trial_log.y + trial_log.z) - (kept_log.x + kept_log.y + kept_log.z);
    let h = clamp(exp(m.hardening * (1.0 - exp(state.strain))), MM_SNOW_HARDENING_MIN, MM_SNOW_HARDENING_MAX);
    let j = sigma.x * sigma.y * sigma.z;
    let tau = 2.0 * m.mu * h * (sigma - vec3<f32>(1.0)) * sigma + vec3<f32>(m.lambda * h * (j - 1.0) * j);
    return MatterUpdate(mm_compose(svd, sigma), mm_symmetric(svd.u, tau), state, j);
}

// One substep of one particle: F advected by its velocity gradient over dt,
// then the material's rule for what stays elastic, then the stress the kept
// strain gives.
fn matter_update(f: mat3x3<f32>, grad_v: mat3x3<f32>, dt: f32, plastic: MatterPlastic, m: MatterMaterial) -> MatterUpdate {
    let identity = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    let trial = (identity + dt * grad_v) * f;
    let svd = matter_svd(trial);
    if m.kind == MATTER_SNOW {
        return matter_snow_update(svd, plastic, m);
    }
    var state = plastic;
    let eps = matter_log_stretch(svd.sigma);
    var kept = vec4<f32>(eps, 0.0);
    if m.kind == MATTER_GRANULAR {
        kept = matter_return_map(eps, &state, m);
    } else if m.kind == MATTER_VISCOPLASTIC {
        kept = matter_viscoplastic_map(eps, dt, m);
    }
    state.strain = state.strain + kept.w;
    let stretch = exp(kept.xyz);
    let j = stretch.x * stretch.y * stretch.z;
    var tau = matter_stress(svd.u, kept.xyz, m);
    if m.kind == MATTER_ELASTIC {
        // Kelvin-Voigt: Cauchy stress η (∇v + ∇vᵀ), Kirchhoff J times it. A
        // turn or a move has no symmetric velocity gradient, so only the
        // wobble loses energy.
        tau = tau + (j * m.relax_time * m.mu) * (grad_v + transpose(grad_v));
    }
    return MatterUpdate(mm_compose(svd, stretch), tau, state, j);
}
