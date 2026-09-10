struct U {
    n: u32,
    color: u32,
    h: f32,
    omega: f32,
    sor_min: vec4<u32>,
    sor_extent: vec4<u32>,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> faces: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> flags: array<u32>;
@group(0) @binding(3) var<storage, read_write> phi: array<f32>;
@group(0) @binding(4) var<storage, read_write> rhs: array<f32>;
@group(0) @binding(5) var<storage, read_write> out_faces: array<vec4<f32>>;

fn index(i: u32, j: u32, k: u32) -> u32 {
    return (k * u.n + j) * u.n + i;
}

fn cell(i: i32, j: i32, k: i32) -> u32 {
    if (i < 0 || j < 0 || k < 0 || i >= i32(u.n) || j >= i32(u.n) || k >= i32(u.n)) {
        return 0u;
    }
    return flags[index(u32(i), u32(j), u32(k))];
}

fn pressure(i: i32, j: i32, k: i32) -> f32 {
    if (cell(i, j, k) == 2u) {
        return phi[index(u32(i), u32(j), u32(k))];
    }
    return 0.0;
}

fn solid_x(i: i32, j: i32, k: i32) -> bool {
    return cell(i, j, k) == 0u || cell(i - 1, j, k) == 0u;
}

fn solid_y(i: i32, j: i32, k: i32) -> bool {
    return cell(i, j, k) == 0u || cell(i, j - 1, k) == 0u;
}

fn solid_z(i: i32, j: i32, k: i32) -> bool {
    return cell(i, j, k) == 0u || cell(i, j, k - 1) == 0u;
}

@compute @workgroup_size(64)
fn boundary(@builtin(global_invocation_id) q: vec3<u32>) {
    let x = q.x;
    if (x >= u.n * u.n * u.n) { return; }
    let i = i32(x % u.n);
    let j = i32((x / u.n) % u.n);
    let k = i32(x / (u.n * u.n));
    let f = faces[x];
    out_faces[x] = vec4(
        select(f.x, 0.0, solid_x(i, j, k)),
        select(f.y, 0.0, solid_y(i, j, k)),
        select(f.z, 0.0, solid_z(i, j, k)),
        0.0,
    );
}

@compute @workgroup_size(64)
fn divergence(@builtin(global_invocation_id) q: vec3<u32>) {
    let x = q.x;
    if (x >= u.n * u.n * u.n) { return; }
    let i = x % u.n;
    let j = (x / u.n) % u.n;
    let k = x / (u.n * u.n);
    if (flags[x] != 2u) { rhs[x] = 0.0; return; }
    let i1 = min(i + 1u, u.n - 1u);
    let j1 = min(j + 1u, u.n - 1u);
    let k1 = min(k + 1u, u.n - 1u);
    rhs[x] = (faces[index(i1, j, k)].x - faces[x].x
        + faces[index(i, j1, k)].y - faces[x].y
        + faces[index(i, j, k1)].z - faces[x].z) / u.h;
}

@compute @workgroup_size(64)
fn sor(@builtin(global_invocation_id) q: vec3<u32>) {
    let local = q.x;
    let volume = u.sor_extent.x * u.sor_extent.y * u.sor_extent.z;
    if (local >= volume) { return; }
    let li = local % u.sor_extent.x;
    let lj = (local / u.sor_extent.x) % u.sor_extent.y;
    let lk = local / (u.sor_extent.x * u.sor_extent.y);
    let i = i32(u.sor_min.x + li);
    let j = i32(u.sor_min.y + lj);
    let k = i32(u.sor_min.z + lk);
    let x = index(u32(i), u32(j), u32(k));
    if (flags[x] != 2u || u32((i + j + k) & 1) != u.color) { return; }
    var sum = 0.0;
    var count = 0.0;
    for (var d = 0u; d < 6u; d++) {
        var a = i; var b = j; var c = k;
        if (d == 0u) { a += 1; } else if (d == 1u) { a -= 1; }
        else if (d == 2u) { b += 1; } else if (d == 3u) { b -= 1; }
        else if (d == 4u) { c += 1; } else { c -= 1; }
        let f = cell(a, b, c);
        if (f == 2u) { sum += pressure(a, b, c); count += 1.0; }
        else if (f == 1u) { count += 1.0; }
    }
    if (count > 0.0) {
        let pressure_target = (sum - u.h * u.h * rhs[x]) / count;
        phi[x] += u.omega * (pressure_target - phi[x]);
    }
}

@compute @workgroup_size(64)
fn project(@builtin(global_invocation_id) q: vec3<u32>) {
    let x = q.x;
    if (x >= u.n * u.n * u.n) { return; }
    let i = i32(x % u.n);
    let j = i32((x / u.n) % u.n);
    let k = i32(x / (u.n * u.n));
    let f = faces[x];
    out_faces[x] = vec4(
        select(f.x - (pressure(i, j, k) - pressure(i - 1, j, k)) / u.h, 0.0, solid_x(i, j, k)),
        select(f.y - (pressure(i, j, k) - pressure(i, j - 1, k)) / u.h, 0.0, solid_y(i, j, k)),
        select(f.z - (pressure(i, j, k) - pressure(i, j, k - 1)) / u.h, 0.0, solid_z(i, j, k)),
        0.0,
    );
}
