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
// atomicMax over the bitcast u32 yields the maximum float.
//
// NaN/Inf agreement (design §11.D / §12.3 step 6): we do NOT trust `max()` to
// propagate a NaN — WGSL `max(NaN, x)` may return `x`, silently dropping it,
// and an Inf would survive the max yet pass an `is_nan`-only verdict. So
// non-finite texels are classified EXPLICITLY, with the bit pattern (NaN != NaN
// is the one reliable NaN test, here done exactly via the abs-bitcast), and
// counted into a dedicated `special_count` whenever the two sides DISAGREE on
// finiteness (one blew up, the other didn't). Such texels are excluded from the
// finite abs/rel reduction so a dropped-or-surviving NaN can't corrupt the
// diagnostic maxima. The fused kernel agreeing with the unfused oracle on a
// non-finite texel (both NaN, both same Inf) is fine; introducing or erasing
// one is the regression this catches.

struct DiffOut {
    max_abs_bits: atomic<u32>,
    max_rel_bits: atomic<u32>,
    over_count: atomic<u32>,
    // Texels where exactly one side is non-finite on some channel — fusion
    // introduced or erased a NaN/Inf relative to the unfused oracle.
    special_count: atomic<u32>,
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

// A channel is "special" (Inf or NaN) iff its abs bit pattern is >= the Inf
// pattern 0x7F800000: Inf is exactly that, every NaN is strictly greater
// (exponent all ones, non-zero mantissa), and every finite value is strictly
// less. Exact — no float thresholds, no reliance on max()/comparison ordering
// (NaN comparisons are all false, which is precisely why we test the bits).
fn channel_special(x: f32) -> bool {
    return bitcast<u32>(abs(x)) >= 0x7F800000u;
}

fn any_special(v: vec4<f32>) -> bool {
    return channel_special(v.x) || channel_special(v.y)
        || channel_special(v.z) || channel_special(v.w);
}

// The two sides DISAGREE on finiteness if any channel is special in one but
// not the other. Both-finite → false (the normal path handles it); both-special
// → false (agreement — fusion reproduced the oracle's non-finite result).
fn special_disagrees(a: vec4<f32>, b: vec4<f32>) -> bool {
    return channel_special(a.x) != channel_special(b.x)
        || channel_special(a.y) != channel_special(b.y)
        || channel_special(a.z) != channel_special(b.z)
        || channel_special(a.w) != channel_special(b.w);
}

@compute @workgroup_size(16, 16, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let a = textureLoad(tex_a, coord, 0);
    let b = textureLoad(tex_b, coord, 0);

    // Non-finite texels take the explicit classifier, NOT the finite reduction —
    // a NaN/Inf must never reach the abs/rel maxima (max() can't be trusted to
    // carry or drop it consistently).
    if (any_special(a) || any_special(b)) {
        if (special_disagrees(a, b)) {
            atomicAdd(&out.special_count, 1u);
        }
        return;
    }

    let abs_diff = max4(abs(a - b));
    let denom = max(max(max4(abs(a)), max4(abs(b))), REL_DENOM_EPS);
    let rel_diff = abs_diff / denom;

    atomicMax(&out.max_abs_bits, bitcast<u32>(abs_diff));
    atomicMax(&out.max_rel_bits, bitcast<u32>(rel_diff));
    if (abs_diff > params.abs_tol && rel_diff > params.rel_tol) {
        atomicAdd(&out.over_count, 1u);
    }
}
