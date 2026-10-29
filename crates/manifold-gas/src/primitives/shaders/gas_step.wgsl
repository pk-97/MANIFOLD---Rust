// The gas solver's step: one box of cells holding smoke or fire, advanced one
// frame by the kernels below in command order (the host composes the
// BLACKBODY_*, LIGHT_FIXED_SCALE and LIGHT_WORDS constants ahead of this
// source).
//
// Cell (x, y, z) is element i = x + nx (y + ny z) of every field; its centre is
// origin + (c + 1/2) h. The floor (y = 0) is a wall, the sides and the top are
// open. Velocity is metres per second, state is x density (soot), y
// temperature (0 ambient, 1 a hot flame), z fuel and w flame.
//
// Roles rotate: the host names three velocity and three state buffers and
// binds them as vel_in/state_in (last frame's result), vel_a/state_a and
// vel_b/state_b. Advection reads the first and writes the others; after it,
// the step's working fields R are bound at vel_a/state_a for every later
// kernel.

struct Params {
    dims: vec3<u32>,
    cells: u32,
    origin: vec3<f32>,
    h: f32,
    up: vec3<f32>,
    g: f32,
    dt: f32,
    time: f32,
    kind: u32,
    pad: u32,
    source_size: f32,
    rate: f32,
    inflow: f32,
    source_heat: f32,
    buoyancy: f32,
    weight: f32,
    vorticity: f32,
    turbulence: f32,
    dissipation: f32,
    cooling: f32,
    burn_rate: f32,
    heat_release: f32,
    soot: f32,
    expansion: f32,
    ignition: f32,
    seed: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> vel_in: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> state_in: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> vel_a: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> state_a: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> vel_b: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read_write> state_b: array<vec4<f32>>;
@group(0) @binding(7) var<storage, read_write> curl: array<vec4<f32>>;
@group(0) @binding(8) var<storage, read_write> pressure: array<f32>;
@group(0) @binding(9) var<storage, read_write> divergence: array<f32>;
// Two words per cell: density and temperature, then fuel and flame, each pair
// packed as two halves. The velocity's pairs are (u, v) then (w, 0).
@group(0) @binding(10) var<storage, read_write> state_out: array<u32>;
@group(0) @binding(11) var<storage, read_write> velocity_out: array<u32>;
@group(0) @binding(12) var<storage, read_write> light_sums: array<atomic<u32>>;
@group(0) @binding(13) var<storage, read> blackbody: array<vec4<f32>>;

// How fast a flame fades once its fuel is gone.
const FLAME_SECONDS: f32 = 0.06;
// Fuel that does not burn fades at this rate, per second.
const FUEL_DECAY: f32 = 0.5;
const MAX_TEMPERATURE: f32 = 1.5;
// Over-relaxation of the pressure sweeps.
const SOR_OMEGA: f32 = 1.6;
// The open faces fade the gas over this many cells.
const OPEN_FADE_CELLS: f32 = 2.0;
// Gravity the buoyancy and weight rates are quoted at.
const REFERENCE_GRAVITY: f32 = 9.81;
// Temperature range of a flame in kelvin, at normalised temperature 0 and 1.
const FLAME_KELVIN_COOL: f32 = 1000.0;
const FLAME_KELVIN_HOT: f32 = 2000.0;

fn inside(c: vec3<u32>) -> bool {
    return c.x < params.dims.x && c.y < params.dims.y && c.z < params.dims.z;
}

fn cell_index(c: vec3<u32>) -> u32 {
    return c.x + params.dims.x * (c.y + params.dims.y * c.z);
}

// Index of the cell nearest `c`, inside the box.
fn clamped_index(c: vec3<i32>) -> u32 {
    let q = clamp(c, vec3<i32>(0), vec3<i32>(params.dims) - vec3<i32>(1));
    return cell_index(vec3<u32>(q));
}

fn cell_centre(c: vec3<u32>) -> vec3<f32> {
    return params.origin + (vec3<f32>(c) + vec3<f32>(0.5)) * params.h;
}

// ---- Trilinear taps -------------------------------------------------------

struct Taps {
    index: array<u32, 8>,
    weight: array<f32, 8>,
}

// The eight cells around grid coordinate `g` (cell units, centres at integers,
// clamped to the box) and their weights.
fn taps(g: vec3<f32>) -> Taps {
    let top = vec3<f32>(params.dims) - vec3<f32>(1.0);
    let q = clamp(g, vec3<f32>(0.0), top);
    let base = floor(q);
    let f = q - base;
    let lo = vec3<u32>(base);
    let hi = min(lo + vec3<u32>(1u), params.dims - vec3<u32>(1u));
    var t: Taps;
    for (var k = 0u; k < 8u; k++) {
        let hx = (k & 1u) != 0u;
        let hy = (k & 2u) != 0u;
        let hz = (k & 4u) != 0u;
        let c = vec3<u32>(select(lo.x, hi.x, hx), select(lo.y, hi.y, hy), select(lo.z, hi.z, hz));
        t.index[k] = cell_index(c);
        t.weight[k] = select(1.0 - f.x, f.x, hx) * select(1.0 - f.y, f.y, hy) * select(1.0 - f.z, f.z, hz);
    }
    return t;
}

struct Range {
    lo: vec4<f32>,
    hi: vec4<f32>,
}

fn blend_vel_in(t: Taps) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var k = 0u; k < 8u; k++) {
        sum += t.weight[k] * vel_in[t.index[k]];
    }
    return sum;
}

fn blend_state_in(t: Taps) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var k = 0u; k < 8u; k++) {
        sum += t.weight[k] * state_in[t.index[k]];
    }
    return sum;
}

fn blend_vel_a(t: Taps) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var k = 0u; k < 8u; k++) {
        sum += t.weight[k] * vel_a[t.index[k]];
    }
    return sum;
}

fn blend_state_a(t: Taps) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var k = 0u; k < 8u; k++) {
        sum += t.weight[k] * state_a[t.index[k]];
    }
    return sum;
}

fn range_vel_in(t: Taps) -> Range {
    var r = Range(vel_in[t.index[0]], vel_in[t.index[0]]);
    for (var k = 1u; k < 8u; k++) {
        let v = vel_in[t.index[k]];
        r.lo = min(r.lo, v);
        r.hi = max(r.hi, v);
    }
    return r;
}

fn range_state_in(t: Taps) -> Range {
    var r = Range(state_in[t.index[0]], state_in[t.index[0]]);
    for (var k = 1u; k < 8u; k++) {
        let s = state_in[t.index[k]];
        r.lo = min(r.lo, s);
        r.hi = max(r.hi, s);
    }
    return r;
}

// ---- Noise ----------------------------------------------------------------

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn lattice_hash(c: vec3<i32>) -> u32 {
    return pcg(pcg(pcg(params.seed ^ bitcast<u32>(c.x)) ^ bitcast<u32>(c.y)) ^ bitcast<u32>(c.z));
}

// A unit vector per lattice point, uniform on the sphere.
fn lattice_gradient(c: vec3<i32>) -> vec3<f32> {
    let h = lattice_hash(c);
    let z = 1.0 - 2.0 * f32(h & 0xffffu) / 65535.0;
    let angle = 6.2831853 * f32(h >> 16u) / 65536.0;
    let r = sqrt(max(1.0 - z * z, 0.0));
    return vec3<f32>(r * cos(angle), r * sin(angle), z);
}

// Gradient (Perlin) noise, within about +-0.87.
fn noise3(q: vec3<f32>) -> f32 {
    let base = floor(q);
    let c = vec3<i32>(base);
    let f = q - base;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let n000 = dot(lattice_gradient(c), f);
    let n100 = dot(lattice_gradient(c + vec3<i32>(1, 0, 0)), f - vec3<f32>(1.0, 0.0, 0.0));
    let n010 = dot(lattice_gradient(c + vec3<i32>(0, 1, 0)), f - vec3<f32>(0.0, 1.0, 0.0));
    let n110 = dot(lattice_gradient(c + vec3<i32>(1, 1, 0)), f - vec3<f32>(1.0, 1.0, 0.0));
    let n001 = dot(lattice_gradient(c + vec3<i32>(0, 0, 1)), f - vec3<f32>(0.0, 0.0, 1.0));
    let n101 = dot(lattice_gradient(c + vec3<i32>(1, 0, 1)), f - vec3<f32>(1.0, 0.0, 1.0));
    let n011 = dot(lattice_gradient(c + vec3<i32>(0, 1, 1)), f - vec3<f32>(0.0, 1.0, 1.0));
    let n111 = dot(lattice_gradient(c + vec3<i32>(1, 1, 1)), f - vec3<f32>(1.0, 1.0, 1.0));
    return mix(
        mix(mix(n000, n100, u.x), mix(n010, n110, u.x), u.y),
        mix(mix(n001, n101, u.x), mix(n011, n111, u.x), u.y),
        u.z,
    );
}

// Three independent noise potentials, offset along x and y.
fn noise_potential(q: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        noise3(q),
        noise3(q + vec3<f32>(31.4, 0.0, 0.0)),
        noise3(q + vec3<f32>(0.0, 47.2, 0.0)),
    );
}

// The curl of the potentials by central differences: a swirl with no
// divergence, so it stirs the gas without compressing it.
fn curl_noise(q: vec3<f32>) -> vec3<f32> {
    let e = 0.05;
    let dx = noise_potential(q + vec3<f32>(e, 0.0, 0.0)) - noise_potential(q - vec3<f32>(e, 0.0, 0.0));
    let dy = noise_potential(q + vec3<f32>(0.0, e, 0.0)) - noise_potential(q - vec3<f32>(0.0, e, 0.0));
    let dz = noise_potential(q + vec3<f32>(0.0, 0.0, e)) - noise_potential(q - vec3<f32>(0.0, 0.0, e));
    return vec3<f32>(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x) / (2.0 * e);
}

// ---- Advection ------------------------------------------------------------

// Semi-Lagrangian: each cell takes the fields from where its air came from.
@compute @workgroup_size(4, 4, 4)
fn cs_advect(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !inside(gid) {
        return;
    }
    let i = cell_index(gid);
    let back = vec3<f32>(gid) - params.dt * vel_in[i].xyz / params.h;
    let t = taps(back);
    vel_a[i] = vec4<f32>(blend_vel_in(t).xyz, 0.0);
    state_a[i] = blend_state_in(t);
}

// MacCormack's second look: the advected fields carried forward again.
@compute @workgroup_size(4, 4, 4)
fn cs_advect_back(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !inside(gid) {
        return;
    }
    let i = cell_index(gid);
    let ahead = vec3<f32>(gid) + params.dt * vel_in[i].xyz / params.h;
    let t = taps(ahead);
    vel_b[i] = vec4<f32>(blend_vel_a(t).xyz, 0.0);
    state_b[i] = blend_state_a(t);
}

// The error-corrected result, held between the extremes of the eight cells
// the air came from. A thread reads only its own vel_b/state_b, so it
// writes the result in place.
@compute @workgroup_size(4, 4, 4)
fn cs_maccormack(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !inside(gid) {
        return;
    }
    let i = cell_index(gid);
    let back = vec3<f32>(gid) - params.dt * vel_in[i].xyz / params.h;
    let t = taps(back);
    let rv = range_vel_in(t);
    let rs = range_state_in(t);
    let v = vel_a[i] + 0.5 * (vel_in[i] - vel_b[i]);
    let s = state_a[i] + 0.5 * (state_in[i] - state_b[i]);
    vel_b[i] = vec4<f32>(clamp(v.xyz, rv.lo.xyz, rv.hi.xyz), 0.0);
    state_b[i] = clamp(s, rs.lo, rs.hi);
}

// ---- Sources, burning and forces (on R) -------------------------------------

@compute @workgroup_size(4, 4, 4)
fn cs_sources(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !inside(gid) {
        return;
    }
    let i = cell_index(gid);
    let pos = cell_centre(gid);
    var v = vel_a[i].xyz;
    var s = state_a[i];

    // The source: a squashed ellipsoid at the floor's centre, broken up by
    // noise that drifts along with time.
    let box_centre_xz = params.origin.xz + 0.5 * vec2<f32>(params.dims.xz) * params.h;
    let source_centre = vec3<f32>(
        box_centre_xz.x,
        params.origin.y + max(2.0 * params.h, 0.15 * params.source_size),
        box_centre_xz.y,
    );
    let radii = vec3<f32>(
        0.5 * params.source_size,
        max(1.5 * params.h, 0.3 * params.source_size),
        0.5 * params.source_size,
    );
    let falloff = 1.0 - smoothstep(0.6, 1.0, length((pos - source_centre) / radii));
    var w = 0.0;
    if falloff > 0.0 {
        let breakup = clamp(
            0.6 + 0.8 * noise3(pos * 2.0 / params.source_size + vec3<f32>(0.0, -2.0 * params.time, 0.0)),
            0.0,
            1.4,
        );
        w = falloff * breakup;
    }
    if params.kind == 0u {
        s.x += params.rate * w * params.dt;
        s.y = max(s.y, params.source_heat * w);
    } else {
        s.z += params.rate * w * params.dt;
        s.y = max(s.y, (params.ignition + 0.3) * step(0.05, w));
    }
    // The source blows along `up` at least as fast as its inflow speed.
    if w > 0.0 {
        v += params.up * max(0.0, params.inflow * w - dot(v, params.up));
    }

    // Fire burns its fuel where it is hot enough, heating and smoking.
    var flame = s.w * exp(-params.dt / FLAME_SECONDS);
    if params.kind == 1u && s.y > params.ignition {
        let burn = s.z * (1.0 - exp(-params.burn_rate * params.dt));
        s.z -= burn;
        s.y = min(s.y + burn * params.heat_release, MAX_TEMPERATURE);
        s.x += burn * params.soot;
        flame = max(flame, clamp(burn / max(params.dt * params.burn_rate, 1e-6), 0.0, 1.0));
    }
    s.w = flame;

    // Hot air rises and smoke sinks, along -gravity; noise stirs only where
    // there is gas.
    v += params.dt * params.up * (params.g / REFERENCE_GRAVITY)
        * (params.buoyancy * s.y - params.weight * s.x);
    if params.turbulence > 0.0 {
        let gas = saturate(s.x + s.y + s.z);
        if gas > 0.0 {
            let q = pos / (4.0 * params.h) + vec3<f32>(0.0, -0.5 * params.time, 0.0);
            v += params.dt * params.turbulence * gas * curl_noise(q);
        }
    }

    s.x *= exp(-params.dissipation * params.dt);
    s.y *= exp(-params.cooling * params.dt);
    s.z *= exp(-FUEL_DECAY * params.dt);

    // What reaches the sides or the top leaves: it fades over the last cells.
    let to_face = vec3<u32>(
        min(gid.x, params.dims.x - 1u - gid.x),
        params.dims.y - 1u - gid.y,
        min(gid.z, params.dims.z - 1u - gid.z),
    );
    let nearest = min(to_face.x, min(to_face.y, to_face.z));
    s *= min((f32(nearest) + 0.5) / OPEN_FADE_CELLS, 1.0);

    // At most four cells a step, so the next advection stays in reach.
    let limit = 4.0 * params.h / max(params.dt, 1e-4);
    let speed = length(v);
    if speed > limit {
        v *= limit / speed;
    }
    s = max(s, vec4<f32>(0.0));
    s.y = min(s.y, MAX_TEMPERATURE);

    vel_a[i] = vec4<f32>(v, 0.0);
    state_a[i] = s;
}

// ---- Vorticity (on R) -------------------------------------------------------

// d(velocity)/d(axis) by central differences; one-sided at the box faces.
fn velocity_derivative(c: vec3<i32>, axis: u32) -> vec3<f32> {
    let lo = max(c[axis] - 1, 0);
    let hi = min(c[axis] + 1, i32(params.dims[axis]) - 1);
    var a = c;
    a[axis] = lo;
    var b = c;
    b[axis] = hi;
    return (vel_a[clamped_index(b)].xyz - vel_a[clamped_index(a)].xyz) / (f32(hi - lo) * params.h);
}

// d(|vorticity|)/d(axis), the same way.
fn curl_length_derivative(c: vec3<i32>, axis: u32) -> f32 {
    let lo = max(c[axis] - 1, 0);
    let hi = min(c[axis] + 1, i32(params.dims[axis]) - 1);
    var a = c;
    a[axis] = lo;
    var b = c;
    b[axis] = hi;
    return (curl[clamped_index(b)].w - curl[clamped_index(a)].w) / (f32(hi - lo) * params.h);
}

@compute @workgroup_size(4, 4, 4)
fn cs_curl(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !inside(gid) {
        return;
    }
    let c = vec3<i32>(gid);
    let dx = velocity_derivative(c, 0u);
    let dy = velocity_derivative(c, 1u);
    let dz = velocity_derivative(c, 2u);
    let omega = vec3<f32>(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x);
    curl[cell_index(gid)] = vec4<f32>(omega, length(omega));
}

// Vorticity confinement (Fedkiw): a push around the vortex cores that puts
// back the swirl advection smooths away. `vorticity` is the dimensionless
// strength; the cell size makes the push shrink with the cells.
@compute @workgroup_size(4, 4, 4)
fn cs_confine(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !inside(gid) {
        return;
    }
    let i = cell_index(gid);
    let c = vec3<i32>(gid);
    let grad = vec3<f32>(
        curl_length_derivative(c, 0u),
        curl_length_derivative(c, 1u),
        curl_length_derivative(c, 2u),
    );
    let len = length(grad);
    if len < 1e-6 {
        return;
    }
    let n = grad / len;
    let v = vel_a[i].xyz + params.dt * params.vorticity * params.h * cross(n, curl[i].xyz);
    vel_a[i] = vec4<f32>(v, 0.0);
}

// ---- Pressure projection (on R) ---------------------------------------------

// Velocity of the cell at `c`, which may lie one past the box: the floor
// mirrors (its normal component flips), an open face repeats the cell `own`.
fn neighbour_velocity(c: vec3<i32>, own: vec3<f32>) -> vec3<f32> {
    if c.y < 0 {
        return vec3<f32>(own.x, -own.y, own.z);
    }
    let dims = vec3<i32>(params.dims);
    if c.x < 0 || c.z < 0 || c.x >= dims.x || c.y >= dims.y || c.z >= dims.z {
        return own;
    }
    return vel_a[cell_index(vec3<u32>(c))].xyz;
}

// Pressure of the cell at `c`, which may lie one past the box: the floor has
// no pressure gradient through it, an open face is atmosphere (0).
fn neighbour_pressure(c: vec3<i32>, own: f32) -> f32 {
    if c.y < 0 {
        return own;
    }
    let dims = vec3<i32>(params.dims);
    if c.x < 0 || c.z < 0 || c.x >= dims.x || c.y >= dims.y || c.z >= dims.z {
        return 0.0;
    }
    return pressure[cell_index(vec3<u32>(c))];
}

// The velocity's divergence, less the expansion the flames ask for.
@compute @workgroup_size(4, 4, 4)
fn cs_divergence(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !inside(gid) {
        return;
    }
    let i = cell_index(gid);
    let c = vec3<i32>(gid);
    let own = vel_a[i].xyz;
    let du = neighbour_velocity(c + vec3<i32>(1, 0, 0), own).x
        - neighbour_velocity(c - vec3<i32>(1, 0, 0), own).x;
    let dv = neighbour_velocity(c + vec3<i32>(0, 1, 0), own).y
        - neighbour_velocity(c - vec3<i32>(0, 1, 0), own).y;
    let dw = neighbour_velocity(c + vec3<i32>(0, 0, 1), own).z
        - neighbour_velocity(c - vec3<i32>(0, 0, 1), own).z;
    divergence[i] = (du + dv + dw) / (2.0 * params.h) - params.expansion * state_a[i].w;
}

// One colour of a red-black Gauss-Seidel sweep with over-relaxation: a cell
// reads only the other colour, so the sweep is race free.
fn pressure_sweep(gid: vec3<u32>, colour: u32) {
    if !inside(gid) {
        return;
    }
    if ((gid.x + gid.y + gid.z) & 1u) != colour {
        return;
    }
    let i = cell_index(gid);
    let c = vec3<i32>(gid);
    let own = pressure[i];
    let sum = neighbour_pressure(c + vec3<i32>(1, 0, 0), own)
        + neighbour_pressure(c - vec3<i32>(1, 0, 0), own)
        + neighbour_pressure(c + vec3<i32>(0, 1, 0), own)
        + neighbour_pressure(c - vec3<i32>(0, 1, 0), own)
        + neighbour_pressure(c + vec3<i32>(0, 0, 1), own)
        + neighbour_pressure(c - vec3<i32>(0, 0, 1), own);
    let solved = (sum - params.h * params.h * divergence[i]) / 6.0;
    pressure[i] = mix(own, solved, SOR_OMEGA);
}

@compute @workgroup_size(4, 4, 4)
fn cs_pressure_red(@builtin(global_invocation_id) gid: vec3<u32>) {
    pressure_sweep(gid, 0u);
}

@compute @workgroup_size(4, 4, 4)
fn cs_pressure_black(@builtin(global_invocation_id) gid: vec3<u32>) {
    pressure_sweep(gid, 1u);
}

// Subtract the pressure gradient; nothing flows into the floor.
@compute @workgroup_size(4, 4, 4)
fn cs_project(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !inside(gid) {
        return;
    }
    let i = cell_index(gid);
    let c = vec3<i32>(gid);
    let own = pressure[i];
    let gradient = vec3<f32>(
        neighbour_pressure(c + vec3<i32>(1, 0, 0), own) - neighbour_pressure(c - vec3<i32>(1, 0, 0), own),
        neighbour_pressure(c + vec3<i32>(0, 1, 0), own) - neighbour_pressure(c - vec3<i32>(0, 1, 0), own),
        neighbour_pressure(c + vec3<i32>(0, 0, 1), own) - neighbour_pressure(c - vec3<i32>(0, 0, 1), own),
    ) / (2.0 * params.h);
    var v = vel_a[i].xyz - gradient;
    if gid.y == 0u {
        v.y = max(v.y, 0.0);
    }
    vel_a[i] = vec4<f32>(v, 0.0);
}

// ---- Publishing (on R) --------------------------------------------------------

fn pack_half(a: f32, b: f32) -> u32 {
    return pack2x16float(clamp(vec2<f32>(a, b), vec2<f32>(-65504.0), vec2<f32>(65504.0)));
}

// The radiator's luminance relative to 1500 K's at `kelvin`, from the table.
fn blackbody_luminance(kelvin: f32) -> f32 {
    let span = BLACKBODY_MAX_KELVIN - BLACKBODY_MIN_KELVIN;
    let position = clamp((kelvin - BLACKBODY_MIN_KELVIN) / span, 0.0, 1.0) * f32(BLACKBODY_ENTRIES - 1u);
    let lower = min(u32(floor(position)), BLACKBODY_ENTRIES - 2u);
    return mix(blackbody[lower].w, blackbody[lower + 1u].w, position - f32(lower));
}

// A non-negative sum as fixed point; the sums wrap past 2^32 / LIGHT_FIXED_SCALE.
fn light_fixed(value: f32) -> u32 {
    return u32(clamp(round(value * LIGHT_FIXED_SCALE), 0.0, 4.0e9));
}

var<workgroup> reduce_power: array<f32, 64>;
var<workgroup> reduce_x: array<f32, 64>;
var<workgroup> reduce_y: array<f32, 64>;
var<workgroup> reduce_z: array<f32, 64>;
var<workgroup> reduce_heat: array<f32, 64>;

@compute @workgroup_size(1)
fn cs_clear_sums() {
    for (var k = 0u; k < LIGHT_WORDS; k++) {
        atomicStore(&light_sums[k], 0u);
    }
}

// Writes the packed fields, and adds the flames' light to light_sums:
// word 0 the emission e = flame * luminance * h^3, words 1 to 3 e times the
// cell centre relative to the box origin, word 4 e times the temperature.
// Each workgroup reduces its cells, then adds once.
@compute @workgroup_size(4, 4, 4)
fn cs_publish(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
) {
    var power = 0.0;
    var moment = vec3<f32>(0.0);
    var heat = 0.0;
    if inside(gid) {
        let i = cell_index(gid);
        let v = vel_a[i].xyz;
        let s = state_a[i];
        state_out[2u * i] = pack_half(s.x, s.y);
        state_out[2u * i + 1u] = pack_half(s.z, s.w);
        velocity_out[2u * i] = pack_half(v.x, v.y);
        velocity_out[2u * i + 1u] = pack_half(v.z, 0.0);
        if s.w > 0.0 {
            let kelvin = mix(FLAME_KELVIN_COOL, FLAME_KELVIN_HOT, s.y);
            power = s.w * blackbody_luminance(kelvin) * params.h * params.h * params.h;
            moment = power * (vec3<f32>(gid) + vec3<f32>(0.5)) * params.h;
            heat = power * s.y;
        }
    }
    reduce_power[lid] = power;
    reduce_x[lid] = moment.x;
    reduce_y[lid] = moment.y;
    reduce_z[lid] = moment.z;
    reduce_heat[lid] = heat;
    workgroupBarrier();
    for (var stride = 32u; stride > 0u; stride = stride >> 1u) {
        if lid < stride {
            reduce_power[lid] += reduce_power[lid + stride];
            reduce_x[lid] += reduce_x[lid + stride];
            reduce_y[lid] += reduce_y[lid + stride];
            reduce_z[lid] += reduce_z[lid + stride];
            reduce_heat[lid] += reduce_heat[lid + stride];
        }
        workgroupBarrier();
    }
    if lid == 0u && reduce_power[0] > 0.0 {
        atomicAdd(&light_sums[0], light_fixed(reduce_power[0]));
        atomicAdd(&light_sums[1], light_fixed(reduce_x[0]));
        atomicAdd(&light_sums[2], light_fixed(reduce_y[0]));
        atomicAdd(&light_sums[3], light_fixed(reduce_z[0]));
        atomicAdd(&light_sums[4], light_fixed(reduce_heat[0]));
    }
}

// ---- Reset --------------------------------------------------------------------

// Zero every field. vel_in/state_in are read-only here, so the host runs this
// twice with the roles rotated until each of the three buffers has been
// vel_a or vel_b.
@compute @workgroup_size(4, 4, 4)
fn cs_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !inside(gid) {
        return;
    }
    let i = cell_index(gid);
    vel_a[i] = vec4<f32>(0.0);
    vel_b[i] = vec4<f32>(0.0);
    state_a[i] = vec4<f32>(0.0);
    state_b[i] = vec4<f32>(0.0);
    curl[i] = vec4<f32>(0.0);
    pressure[i] = 0.0;
    divergence[i] = 0.0;
}
