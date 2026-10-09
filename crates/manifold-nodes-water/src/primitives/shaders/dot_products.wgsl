// node.dot_products — dot products of one vector with each of the first
// `rows` rows of a row-major matrix (row r at matrix[r·length ..]), or each
// row's sum when no vector is wired (`has_vector` 0; the matrix is bound in
// the vector's slot and not read there). Two passes with a barrier between:
//   partial_main  — workgroup (g, r): grid-stride partial sum over row r,
//                   tree-reduced in workgroup memory to partials[r·groups + g].
//   finalize_main — one thread per output row adds its partials in order;
//                   rows past `rows` read 0. `root` 1 takes square roots.
// Fixed stride and fixed tree, so the result is identical run to run.

struct Params {
    length: u32,
    rows: u32,
    groups: u32,
    root: u32,
    has_vector: u32,
    max_rows: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> matrix: array<f32>;
@group(0) @binding(2) var<storage, read> other: array<f32>;
@group(0) @binding(3) var<storage, read_write> partials: array<f32>;
@group(0) @binding(4) var<storage, read_write> out: array<f32>;

var<workgroup> sums: array<f32, 256>;

@compute @workgroup_size(256, 1, 1)
fn partial_main(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    let row = wg.y;
    let base = row * u.length;
    var acc = 0.0;
    for (var e = wg.x * 256u + li; e < u.length; e = e + u.groups * 256u) {
        var b = 1.0;
        if u.has_vector != 0u {
            b = other[e];
        }
        acc = acc + matrix[base + e] * b;
    }
    sums[li] = acc;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width = width >> 1u) {
        if li < width {
            sums[li] = sums[li] + sums[li + width];
        }
        workgroupBarrier();
    }
    if li == 0u {
        partials[row * u.groups + wg.x] = sums[0];
    }
}

@compute @workgroup_size(64, 1, 1)
fn finalize_main(@builtin(local_invocation_index) li: u32) {
    if li >= u.max_rows {
        return;
    }
    var total = 0.0;
    if li < u.rows {
        for (var g = 0u; g < u.groups; g = g + 1u) {
            total = total + partials[li * u.groups + g];
        }
        if u.root != 0u {
            total = sqrt(max(total, 0.0));
        }
    }
    out[li] = total;
}
