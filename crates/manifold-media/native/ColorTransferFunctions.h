// ColorTransferFunctions.h — shared Metal Shading Language snippet for the
// piecewise sRGB OETF (linear -> encoded) and EOTF (encoded -> linear).
//
// The live display (scanout, ExtendedLinearSRGB surface) and the still
// exporter both use the true piecewise function, so video was the odd one
// out. This header is the ONE shared definition both native plugins use, so
// they can't drift from each other or from the reference again.
//
// The constants here are ported literally from the tested Rust reference —
// crates/manifold-media/src/still_exporter.rs `linear_to_srgb` (the encode
// direction; see its doc comment and the `srgb_encodes_linear_midgray` /
// `faithful_clip_saturates_at_and_above_white` tests). `manifold_srgb_decode`
// below is the algebraic inverse of that same piecewise function (breakpoint
// 0.0031308 * 12.92 == 0.04045033..., rounded to 0.04045 as is conventional).
//
// This is a plain Objective-C header (not a Metal .metal file) so both
// MetalEncoderPlugin.m and MetalVideoDecoderPlugin.m can #import it and
// splice `kManifoldColorTransferFunctionsMSL` into the Metal source string
// they hand to `-[MTLDevice newLibraryWithSource:options:error:]` — there is
// no separate Metal compilation step in this build (see build.rs), so this
// is the natural way to share literal shader source between the two .m
// files without a real file-system #include at the MSL level.
#ifndef MANIFOLD_COLOR_TRANSFER_FUNCTIONS_H
#define MANIFOLD_COLOR_TRANSFER_FUNCTIONS_H

#import <Foundation/Foundation.h>

static NSString *const kManifoldColorTransferFunctionsMSL =
    @"// -- Shared sRGB OETF/EOTF (ColorTransferFunctions.h) --------------------\n"
     "inline float3 manifold_srgb_encode(float3 x) {\n"
     "    x = clamp(x, float3(0.0), float3(1.0));\n"
     "    float3 lo = x * 12.92;\n"
     "    float3 hi = 1.055 * pow(x, float3(1.0 / 2.4)) - 0.055;\n"
     "    return select(hi, lo, x <= float3(0.0031308));\n"
     "}\n"
     "\n"
     "inline float3 manifold_srgb_decode(float3 x) {\n"
     "    x = clamp(x, float3(0.0), float3(1.0));\n"
     "    float3 lo = x / 12.92;\n"
     "    float3 hi = pow((x + 0.055) / 1.055, float3(2.4));\n"
     "    return select(hi, lo, x <= float3(0.04045));\n"
     "}\n";

#endif // MANIFOLD_COLOR_TRANSFER_FUNCTIONS_H
