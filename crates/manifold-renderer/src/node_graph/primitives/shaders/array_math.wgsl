// node.array_math — element-wise math over Array<f32>(s).
//
// One bundled primitive, op enum dispatch — mirrors node.math (scalar)
// for the array domain. Binary ops read `a` and `b`; unary ops read
// `a` only and ignore `b`. Op-specific scalars (scale / offset / exp /
// bias) are uniforms and used selectively per op.
//
// Op codes (must match Rust ARRAY_MATH_OPS table):
//   0 Add          out = a + b
//   1 Subtract     out = a - b
//   2 Multiply     out = a * b
//   3 Divide       out = a / b   (b == 0 → 0)
//   4 Min          out = min(a, b)
//   5 Max          out = max(a, b)
//   6 ScaleOffset  out = a * scale + offset
//   7 ShapePowClip out = pow(max(a + bias, 0.0), exp) * scale
//   8 MirrorRamp   out = smoothstep(0, 1, 1 - abs(a * 2 - 1))
//   9 Clamp01      out = clamp(a, 0.0, 1.0)
//  10 Abs          out = abs(a)

struct Uniforms {
    count:   u32,
    op:      u32,
    scale:   f32,
    offset:  f32,
    exp:     f32,
    bias:    f32,
    _pad0:   u32,
    _pad1:   u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read>       a:   array<f32>;
@group(0) @binding(2) var<storage, read>       b:   array<f32>;
@group(0) @binding(3) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }
    let av = a[idx];
    let bv = b[idx];
    var result: f32 = 0.0;
    switch u.op {
        case 0u: { result = av + bv; }
        case 1u: { result = av - bv; }
        case 2u: { result = av * bv; }
        case 3u: {
            // Divide-by-(near-)zero clamps to 0 to keep downstream
            // shader code from propagating NaN/Inf.
            if abs(bv) < 1e-20 {
                result = 0.0;
            } else {
                result = av / bv;
            }
        }
        case 4u: { result = min(av, bv); }
        case 5u: { result = max(av, bv); }
        case 6u: { result = av * u.scale + u.offset; }
        case 7u: { result = pow(max(av + u.bias, 0.0), u.exp) * u.scale; }
        case 8u: {
            let t = 1.0 - abs(av * 2.0 - 1.0);
            result = smoothstep(0.0, 1.0, t);
        }
        case 9u: { result = clamp(av, 0.0, 1.0); }
        case 10u: { result = abs(av); }
        default: { result = av; }
    }
    out[idx] = result;
}
