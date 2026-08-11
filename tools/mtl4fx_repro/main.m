// MTL4FX Temporal Denoised Scaler — typed creation repro
//
// Xcode 26.6 ships real headers for MTL4FXTemporalDenoisedScaler. This
// repro exercises the full typed descriptor contract against the MTL4
// compiler path to isolate BUG-woji.
//
// Build: clang -framework Metal -framework MetalFX -framework Foundation main.m -o repro
// Run:   ./repro
//
// Variants via preprocessor for bisection:
//   clang ... -DFULL -o repro-full && ./repro-full
//   clang ... -DREX_DISABLE -o repro-rex && ./repro-rex
//   etc.

#import <Metal/Metal.h>
#import <MetalFX/MetalFX.h>
#import <Foundation/Foundation.h>
#import <stdio.h>
#import <stdlib.h>

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        printf("=== MTL4FX Temporal Denoised Scaler — typed repro (Xcode 26.6 SDK) ===\n");
        printf("macOS: 26.6.1 (25G76)  MetalFX: 31.8\n\n");

        // ── Device ──────────────────────────────────────────────────
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) {
            printf("FATAL: no Metal device\n");
            return 1;
        }
        printf("[1/4] Device: %s\n", [[device name] UTF8String]);

        // Pre-flight checks
        BOOL m4fx = [MTLFXTemporalDenoisedScalerDescriptor supportsMetal4FX:device];
        printf("      supportsMetal4FX: %s\n", m4fx ? "YES" : "NO");
        printf("      supportsDevice:  %s\n",
               [MTLFXTemporalDenoisedScalerDescriptor supportsDevice:device] ? "YES" : "NO");

        // ── MTL4Compiler ────────────────────────────────────────────
        MTL4CompilerDescriptor *compilerDesc = [MTL4CompilerDescriptor new];
        NSError *error = nil;
        id<MTL4Compiler> compiler = [device newCompilerWithDescriptor:compilerDesc error:&error];
        if (!compiler) {
            printf("FATAL: MTL4Compiler creation returned nil: %s\n",
                   error ? [[error localizedDescription] UTF8String] : "?");
            return 1;
        }
        printf("[2/4] MTL4Compiler created: yes\n");

        // ── Descriptor — full documented property set ───────────────
        // Every property from MTLFXTemporalDenoisedScalerDescriptor.h
        // (Xcode 26.6 SDK) set to a valid, non-default value.
        MTLFXTemporalDenoisedScalerDescriptor *desc = [MTLFXTemporalDenoisedScalerDescriptor new];

        // Dimensions: 1:1 denoise (no upscale), 64x64 minimum viable
        desc.inputWidth  = 64;
        desc.inputHeight = 64;
        desc.outputWidth  = 64;
        desc.outputHeight = 64;

        // Core formats
        desc.colorTextureFormat  = MTLPixelFormatRGBA16Float;
        desc.outputTextureFormat = MTLPixelFormatRGBA16Float;

        // G-buffer aux texture formats (match render_scene G-buffer MRT)
        desc.depthTextureFormat               = MTLPixelFormatR32Float;
        desc.motionTextureFormat              = MTLPixelFormatRG16Float;
        desc.normalTextureFormat              = MTLPixelFormatRGBA16Float;
        desc.roughnessTextureFormat           = MTLPixelFormatR16Float;
        desc.diffuseAlbedoTextureFormat       = MTLPixelFormatRGBA16Float;
        desc.specularAlbedoTextureFormat      = MTLPixelFormatRGBA16Float;
        desc.specularHitDistanceTextureFormat = MTLPixelFormatR16Float;

        // Xcode 26.0+ properties (not in earlier SDKs; not in objc2-metal-fx 0.3.2)
        desc.specularHitDistanceTextureEnabled = YES;
        desc.denoiseStrengthMaskTextureFormat  = MTLPixelFormatR16Float;
        desc.denoiseStrengthMaskTextureEnabled = YES;
        desc.transparencyOverlayTextureFormat  = MTLPixelFormatRGBA16Float;
        desc.transparencyOverlayTextureEnabled = YES;

        // Reactive mask (match render_scene's reactive-mask MRT)
        desc.reactiveMaskTextureEnabled = YES;
        desc.reactiveMaskTextureFormat  = MTLPixelFormatR16Float;

        // Auto exposure off — our content pipeline drives exposure separately
        desc.autoExposureEnabled = NO;

        // Synchronous init — let the MTL4 compiler handle everything at once
        desc.requiresSynchronousInitialization = YES;

        printf("[3/4] Descriptor configured (full documented property set)\n");

        // ── Create — THE MOMENT OF TRUTH ────────────────────────────
        printf("[4/4] Calling newTemporalDenoisedScalerWithDevice:compiler: ...\n");
        fflush(stdout);

        id<MTL4FXTemporalDenoisedScaler> scaler =
            [desc newTemporalDenoisedScalerWithDevice:device compiler:compiler];

        if (scaler) {
            printf("\nSUCCESS: MTL4FXTemporalDenoisedScaler created.\n");
            return 0;
        } else {
            printf("\nRESULT: nil (no crash) — descriptor accepted but scaler creation failed.\n");
            return 1;
        }

        // NOTE: On macOS 26.6.1 (25G76), MetalFX 31.8, this test
        // SIGABRTs inside MPSGraphExecutable::convertMPSGraphShapesToMLIRTypes
        // with "Incompatible shape for parameter at index 0." The crash is
        // in the mlKernelMetal4→MPSGraph compilation path — Apple's internal
        // denoiser kernel graph, not our descriptor. Every documented property
        // combination (full set, rex-disable, no mask formats, no sync init,
        // varied dimensions from 64x64 to 1920x1080) produces the same crash.
        //
        // The MTL4FXTemporalScaler (non-denoised) creates and encodes fine
        // on the same device, same compiler, same macOS build.
    }
}
