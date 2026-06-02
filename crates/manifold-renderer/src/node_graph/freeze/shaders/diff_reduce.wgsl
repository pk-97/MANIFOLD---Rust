// Oracle diff-core reduction (freeze/fusion compiler, design §7).
//
// Compares two same-dimension textures texel-by-texel — no sampler, because
// the oracle wants an EXACT same-texel comparison, not a filtered one — and
// reduces the whole image to a 16-byte verdict on the GPU. The harness reads
// back four words, never the image, so a 4K compare costs one dispatch + a
// tiny readback instead of the per-pixel CPU scan the legacy parity path used.
//
// Tolerance model (design §11.D, two-sided + discontinuity-aware): a texel
// "fails" only when it exceeds BOTH the absolute and relative bounds. We tally
// those into `over_count`, so the verdict can tolerate a small fraction of
// post-discontinuity boundary texels (where one f16 quantum on the wrong side
// of a clamp/fract/step lands far from the f32 fused value) rather than trip on
// a single one. `max_abs` / `max_rel` are reported alongside for diagnostics.
//
// Float-as-u32 atomicMax trick: abs and relative diffs are >= 0, and for non-
// negative IEEE-754 floats the raw bit pattern is monotonic in value, so an
// atomicMax over the bitcast u32 yields the maximum float. A NaN input bitcasts
// to a very large u32 (exponent all ones), so NaN surfaces AS the max — it is
// reported, never silently dropped (design §11.D: classify NaN/Inf, don't skip).

struct DiffOut {
    max_abs_bits: atomic<u32>,
    max_rel_bits: atomic<u32>,
    over_count: atomic<u32>,
    _pad: u32,
}

struct Params {
    width: u32,
    height: u32,
    abs_tol: f32,
    rel_tol: f32,
}

@group(0) @binding(0) var<storage, read_write> out: DiffOut;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var<uniform> params: Params;

// Denominator floor for the relative metric, so a near-zero reference can't
// manufacture a huge relative diff out of a tiny absolute one.
const REL_DENOM_EPS: f32 = 1.0e-3;

fn max4(v: vec4<f32>) -> f32 {
    return max(max(v.x, v.y), max(v.z, v.w));
}

@compute @workgroup_size(16, 16, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let a = textureLoad(tex_a, coord, 0);
    let b = textureLoad(tex_b, coord, 0);

    let abs_diff = max4(abs(a - b));
    let denom = max(max(max4(abs(a)), max4(abs(b))), REL_DENOM_EPS);
    let rel_diff = abs_diff / denom;

    atomicMax(&out.max_abs_bits, bitcast<u32>(abs_diff));
    atomicMax(&out.max_rel_bits, bitcast<u32>(rel_diff));
    if (abs_diff > params.abs_tol && rel_diff > params.rel_tol) {
        atomicAdd(&out.over_count, 1u);
    }
}
